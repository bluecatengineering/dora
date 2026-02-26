use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::sync::Arc;
use std::time::Duration;

use dhcproto::{Decodable, Decoder, Encodable, v6};
use socket2::{Domain, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, oneshot};

use crate::transport::TransportError;

type PendingMap = Arc<Mutex<HashMap<[u8; 3], oneshot::Sender<v6::Message>>>>;

#[derive(Debug)]
pub struct UdpV6Transport {
    socket: Arc<UdpSocket>,
    pending: PendingMap,
}

impl UdpV6Transport {
    pub fn bind(iface: Option<&str>, iface_index: u32) -> Result<Self, TransportError> {
        Self::bind_with_port(iface, iface_index, dhcproto::v6::CLIENT_PORT)
    }

    fn bind_with_port(
        iface: Option<&str>,
        iface_index: u32,
        bind_port: u16,
    ) -> Result<Self, TransportError> {
        let socket = Socket::new(Domain::IPV6, Type::DGRAM, None)?;
        socket.set_only_v6(true)?;
        socket.set_nonblocking(true)?;

        if let Some(iface_name) = iface {
            socket.bind_device(Some(iface_name.as_bytes()))?;
        }

        if iface_index != 0 {
            socket.set_multicast_if_v6(iface_index)?;
        }
        socket.bind(
            &SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, bind_port, 0, 0)).into(),
        )?;

        let std_socket = unsafe { std::net::UdpSocket::from_raw_fd(socket.into_raw_fd()) };
        let socket = Arc::new(UdpSocket::from_std(std_socket)?);

        let pending = Arc::new(Mutex::new(HashMap::new()));
        spawn_recv_loop(Arc::clone(&socket), Arc::clone(&pending));

        Ok(Self { socket, pending })
    }

    #[cfg(test)]
    pub fn bind_ephemeral(iface: Option<&str>, iface_index: u32) -> Result<Self, TransportError> {
        Self::bind_with_port(iface, iface_index, 0)
    }

    pub async fn exchange(
        &self,
        msg: &v6::Message,
        target: SocketAddr,
        timeout: Duration,
    ) -> Result<v6::Message, TransportError> {
        if !target.is_ipv6() {
            return Err(TransportError::AddressFamilyMismatch);
        }

        let xid = msg.xid();
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.insert(xid, tx).is_some() {
                return Err(TransportError::XidCollision(format_xid(xid)));
            }
        }

        let payload = msg
            .to_vec()
            .map_err(|err| TransportError::Encode(err.to_string()))?;
        if let Err(err) = self.socket.send_to(&payload, target).await {
            self.pending.lock().await.remove(&xid);
            return Err(TransportError::Io(err));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&xid);
                Err(TransportError::ChannelClosed)
            }
            Err(_) => {
                self.pending.lock().await.remove(&xid);
                Err(TransportError::Timeout(timeout))
            }
        }
    }

    pub async fn send(&self, msg: &v6::Message, target: SocketAddr) -> Result<(), TransportError> {
        if !target.is_ipv6() {
            return Err(TransportError::AddressFamilyMismatch);
        }

        let payload = msg
            .to_vec()
            .map_err(|err| TransportError::Encode(err.to_string()))?;
        self.socket.send_to(&payload, target).await?;
        Ok(())
    }
}

fn format_xid(xid: [u8; 3]) -> String {
    format!("0x{:02x}{:02x}{:02x}", xid[0], xid[1], xid[2])
}

fn spawn_recv_loop(socket: Arc<UdpSocket>, pending: PendingMap) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let (len, _) = match socket.recv_from(&mut buf).await {
                Ok(value) => value,
                Err(_) => break,
            };

            let decoded = v6::Message::decode(&mut Decoder::new(&buf[..len]));
            let msg = match decoded {
                Ok(msg) => msg,
                Err(_) => continue,
            };

            let xid = msg.xid();
            let tx = pending.lock().await.remove(&xid);
            if let Some(tx) = tx {
                let _ = tx.send(msg);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use dhcproto::{Decodable, Encodable, v6};
    use tokio::net::UdpSocket;

    use super::UdpV6Transport;

    #[tokio::test]
    async fn correlates_response_by_xid() {
        let server = match UdpSocket::bind("[::1]:0").await {
            Ok(socket) => socket,
            Err(_) => return,
        };
        let server_addr = server.local_addr().expect("local addr");

        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let (len, peer) = server.recv_from(&mut buf).await.expect("recv v6 packet");
            let req = v6::Message::decode(&mut dhcproto::Decoder::new(&buf[..len]))
                .expect("decode request");

            let mut resp = v6::Message::new_with_id(v6::MessageType::Reply, req.xid());
            resp.opts_mut()
                .insert(v6::DhcpOption::ServerId(vec![1, 2, 3]));

            server
                .send_to(&resp.to_vec().expect("encode response"), peer)
                .await
                .expect("send response");
        });

        let transport = UdpV6Transport::bind_ephemeral(None, 0).expect("transport bind");
        let mut req = v6::Message::new_with_id(v6::MessageType::Solicit, [0xab, 0xcd, 0xef]);
        req.opts_mut()
            .insert(v6::DhcpOption::ClientId(vec![0, 1, 2]));

        let resp = transport
            .exchange(&req, server_addr, std::time::Duration::from_millis(250))
            .await
            .expect("exchange");

        assert_eq!(resp.xid(), req.xid());
        assert_eq!(resp.msg_type(), v6::MessageType::Reply);
    }
}

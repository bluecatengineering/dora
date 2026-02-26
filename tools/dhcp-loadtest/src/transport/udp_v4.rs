use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::sync::Arc;
use std::time::Duration;

use dhcproto::{Decodable, Decoder, Encodable, v4};
use socket2::{Domain, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, oneshot};

use crate::transport::TransportError;

type PendingMap = Arc<Mutex<HashMap<u32, oneshot::Sender<v4::Message>>>>;

#[derive(Debug)]
pub struct UdpV4Transport {
    socket: Arc<UdpSocket>,
    pending: PendingMap,
}

impl UdpV4Transport {
    pub fn bind(iface: Option<&str>) -> Result<Self, TransportError> {
        Self::bind_with_port(iface, dhcproto::v4::CLIENT_PORT)
    }

    #[cfg(test)]
    fn bind_ephemeral(iface: Option<&str>) -> Result<Self, TransportError> {
        Self::bind_with_port(iface, 0)
    }

    fn bind_with_port(iface: Option<&str>, port: u16) -> Result<Self, TransportError> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, None)?;
        socket.set_nonblocking(true)?;
        socket.set_broadcast(true)?;

        if let Some(iface_name) = iface {
            socket.bind_device(Some(iface_name.as_bytes()))?;
        }

        socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;

        let std_socket = unsafe { std::net::UdpSocket::from_raw_fd(socket.into_raw_fd()) };
        let socket = Arc::new(UdpSocket::from_std(std_socket)?);

        let pending = Arc::new(Mutex::new(HashMap::new()));
        spawn_recv_loop(Arc::clone(&socket), Arc::clone(&pending));

        Ok(Self { socket, pending })
    }

    pub async fn exchange(
        &self,
        msg: &v4::Message,
        target: SocketAddr,
        timeout: Duration,
    ) -> Result<v4::Message, TransportError> {
        if !target.is_ipv4() {
            return Err(TransportError::AddressFamilyMismatch);
        }

        let xid = msg.xid();
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.insert(xid, tx).is_some() {
                return Err(TransportError::XidCollision(format!("0x{xid:08x}")));
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

    pub async fn send(&self, msg: &v4::Message, target: SocketAddr) -> Result<(), TransportError> {
        if !target.is_ipv4() {
            return Err(TransportError::AddressFamilyMismatch);
        }
        let payload = msg
            .to_vec()
            .map_err(|err| TransportError::Encode(err.to_string()))?;
        self.socket.send_to(&payload, target).await?;
        Ok(())
    }
}

fn spawn_recv_loop(socket: Arc<UdpSocket>, pending: PendingMap) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let (len, _) = match socket.recv_from(&mut buf).await {
                Ok(value) => value,
                Err(_) => break,
            };

            let decoded = v4::Message::decode(&mut Decoder::new(&buf[..len]));
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
    use std::net::{Ipv4Addr, SocketAddr};

    use dhcproto::{Decodable, Encodable, v4};
    use tokio::net::UdpSocket;

    use super::UdpV4Transport;

    #[tokio::test]
    async fn correlates_response_by_xid() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind v4 test server");
        let server_addr = server.local_addr().expect("server addr");

        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let (len, peer) = server
                .recv_from(&mut buf)
                .await
                .expect("recv client packet");
            let req = v4::Message::decode(&mut dhcproto::Decoder::new(&buf[..len]))
                .expect("decode v4 request");

            let mut resp = v4::Message::new_with_id(
                req.xid(),
                Ipv4Addr::UNSPECIFIED,
                "192.168.1.10".parse().expect("ip parse"),
                Ipv4Addr::UNSPECIFIED,
                Ipv4Addr::UNSPECIFIED,
                req.chaddr(),
            );
            resp.opts_mut()
                .insert(v4::DhcpOption::MessageType(v4::MessageType::Offer));

            server
                .send_to(&resp.to_vec().expect("encode response"), peer)
                .await
                .expect("send response");
        });

        let transport = UdpV4Transport::bind_ephemeral(None).expect("transport bind");

        let mut req = v4::Message::new_with_id(
            0x1234_5678,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            &[0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee],
        );
        req.opts_mut()
            .insert(v4::DhcpOption::MessageType(v4::MessageType::Discover));

        let resp = transport
            .exchange(
                &req,
                SocketAddr::from(server_addr),
                std::time::Duration::from_millis(250),
            )
            .await
            .expect("exchange");

        assert_eq!(resp.xid(), req.xid());
        assert_eq!(resp.opts().msg_type(), Some(v4::MessageType::Offer));
    }
}

use std::{
    fmt::{self, Debug},
    marker::PhantomData,
    net::{IpAddr, SocketAddr, UdpSocket},
    os::unix::prelude::{FromRawFd, IntoRawFd},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chan::{Receiver, Sender};
use crossbeam_channel as chan;
use dora_core::tracing::{debug, error, trace, warn};

use dora_core::dhcproto::{
    decoder::{Decodable, Decoder},
    encoder::Encodable,
    v4, v6,
};

use super::builder::{ClientSettings, MsgType};

#[derive(Debug)]
pub struct Client<I> {
    args: ClientSettings,
    // zero sized value, the type parameter is just so
    // a client knows if it can return a v4 or v6 message type
    _marker: PhantomData<I>,
}

impl<I> Client<I> {
    pub fn new(args: ClientSettings) -> Self {
        Self {
            args,
            _marker: PhantomData,
        }
    }

    fn spawn_send(&self, msg_type: MsgType, retry_rx: Receiver<()>, send: Arc<UdpSocket>) {
        thread::spawn({
            let args = self.args.clone();
            let send_count = args.send_retries;
            move || {
                let mut count = 0;
                while retry_rx.recv().is_ok() {
                    if let Err(err) = try_send(&args, &msg_type, &send) {
                        error!(?err, "error sending");
                    }
                    count += 1;
                }
                if count >= send_count {
                    warn!("max retries-- exiting");
                }
                Ok::<_, anyhow::Error>(())
            }
        });
    }

    fn spawn_recv<M: Decodable + Encodable + Send + Sync + 'static>(
        &self,
        tx: Sender<M>,
        recv: Arc<UdpSocket>,
    ) {
        thread::spawn({
            let args = self.args.clone();
            move || {
                if let Err(err) = try_recv::<M>(&args, &tx, &recv) {
                    error!(?err, "could not receive");
                }
                Ok::<_, anyhow::Error>(())
            }
        });
    }

    fn send_recv<M: Decodable + Encodable + Send + Sync + 'static>(
        &mut self,
        msg_type: MsgType,
    ) -> Result<M> {
        let start = Instant::now();
        // TODO: make not just v4 sockets
        let bind_addr: SocketAddr = "0.0.0.0:0".parse()?;
        let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
        debug!("client socket created");

        // SO_BINDTODEVICE
        if let Some(ref iface) = self.args.iface_name {
            socket
                .bind_device(Some(iface.as_bytes()))
                .context("failed to bind interface")?;
            debug!(?iface, "client socket bound to");
        }
        socket.bind(&bind_addr.into())?;
        debug!(?bind_addr, "client socket bound to");
        let send = Arc::new(unsafe { UdpSocket::from_raw_fd(socket.into_raw_fd()) });
        let recv = Arc::clone(&send);

        // this channel is for receiving a decoded v4/v6 message
        let (tx, rx) = chan::bounded::<M>(1);
        // this is for controlling when we send so we're able to retry
        let (retry_tx, retry_rx) = chan::bounded(1);
        self.spawn_recv(tx, recv);
        self.spawn_send(msg_type, retry_rx, send);

        let timeout = chan::tick(Duration::from_millis(self.args.timeout));

        retry_tx.send(()).expect("retry channel send failed");

        let mut count = 0;
        while count < self.args.send_retries {
            chan::select! {
                recv(rx) -> res => {
                    match res {
                        Ok(msg) => {
                            return Ok(msg);
                        }
                        Err(err) => {
                            error!(?err, "channel returned error");
                            break;
                        }
                    }
                }
                recv(timeout) -> _ => {
                    debug!(elapsed = %PrettyDuration(start.elapsed()), "received timeout-- retrying");
                    count += 1;
                    retry_tx.send(()).expect("retry channel send failed");
                    continue;
                }
            }
        }
        drop(retry_tx);

        Err(anyhow::anyhow!(
            "hit max retries-- failed to get a response"
        ))
    }
}

// Specialized in case `run` needs to print different output for v4/v6
impl Client<v4::Message> {
    pub fn run(&mut self, msg_type: MsgType) -> Result<v4::Message> {
        let msg = self.send_recv::<v4::Message>(msg_type)?;
        debug!(msg_type = ?msg.opts().msg_type(), %msg, "decoded");
        Ok(msg)
    }
}

impl Client<v6::Message> {
    pub fn run(&mut self, msg_type: MsgType) -> Result<v6::Message> {
        let msg = self.send_recv::<v6::Message>(msg_type)?;
        debug!(msg_type = ?msg.msg_type(), ?msg, "decoded");
        Ok(msg)
    }
}

fn try_recv<M: Decodable + Encodable + Send + Sync + 'static>(
    args: &ClientSettings,
    tx: &Sender<M>,
    recv: &Arc<UdpSocket>,
) -> Result<()> {
    let mut buf = vec![0; 1024];
    let (len, _addr) = recv.recv_from(&mut buf)?;
    let msg = M::decode(&mut Decoder::new(&buf[..len]))?;
    tx.send_timeout(msg, Duration::from_secs(1))?;

    Ok(())
}

fn try_send(args: &ClientSettings, msg_type: &MsgType, send: &Arc<UdpSocket>) -> Result<()> {
    let mut broadcast = false;
    let target: SocketAddr = match args.target {
        IpAddr::V4(addr) if addr.is_broadcast() => {
            send.set_broadcast(true)?;
            broadcast = true;
            (args.target, args.port).into()
        }
        IpAddr::V4(addr) => (addr, args.port).into(),
        // TODO: IPv6
        IpAddr::V6(addr) if addr.is_multicast() => {
            send.join_multicast_v6(&addr, 0)?;
            (addr, args.port).into()
        }
        IpAddr::V6(addr) => (IpAddr::V6(addr), args.port).into(),
    };

    let msg = match msg_type {
        MsgType::Discover(args) => args.build(broadcast),
        MsgType::Request(args) => args.build(),
        MsgType::Decline(args) => args.build(),
        MsgType::BootP(args) => args.build(broadcast),
    };

    debug!(msg_type = ?msg.opts().msg_type(), ?target, ?msg, "sending msg");

    let res = send.send_to(&msg.to_vec()?[..], target)?;
    trace!(?res, "sent");
    Ok(())
}

#[derive(Copy, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct PrettyDuration(Duration);

impl fmt::Display for PrettyDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}s", &self.0.as_secs_f32().to_string()[0..=4])
    }
}

#[derive(Copy, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct PrettyPrint<T>(T);

impl<T: fmt::Debug> fmt::Display for PrettyPrint<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#?}", &self.0)
    }
}

#[derive(Clone, Debug)]
pub enum Response {
    V4(v4::Message),
    V6(v6::Message),
}

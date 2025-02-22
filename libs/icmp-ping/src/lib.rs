mod errors;
mod icmp;
mod shutdown;
mod socket;

pub use crate::errors::Error;
pub use crate::icmp::{Decode, EchoReply, EchoRequest, Encode, ICMP_HEADER_SIZE, Icmpv4, Icmpv6};
use crate::{icmp::Proto, socket::Socket};

use dora_core::metrics;
use parking_lot::Mutex;
use shutdown::Shutdown;
use socket2::{Domain, Protocol, Type};
use tokio::sync::{broadcast, oneshot};
use tokio::task;
use tracing::{debug, error, trace, warn};

use core::fmt;
use std::{
    collections::HashMap,
    io,
    marker::PhantomData,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

pub const DEFAULT_TOKEN_SIZE: usize = 24;
pub type Token = [u8; DEFAULT_TOKEN_SIZE];
pub type PingMap = Arc<Mutex<HashMap<Token, Ping>>>;

const ECHO_REQUEST_BUFFER_SIZE: usize = ICMP_HEADER_SIZE + DEFAULT_TOKEN_SIZE;
type EchoRequestBuffer = [u8; ECHO_REQUEST_BUFFER_SIZE];

/// A socket that knows how to speak ICMP
pub struct IcmpEcho<M> {
    inner: Socket,
    decode_header: bool,
    _phantom: PhantomData<M>,
}

impl<M> fmt::Debug for IcmpEcho<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IcmpEcho")
            .field("_phantom", &self._phantom)
            .finish()
    }
}

impl<P: Proto> IcmpEcho<P> {
    pub async fn request<'a>(&self, hostname: IpAddr, req: &EchoRequest<'a>) -> io::Result<()>
    where
        EchoRequest<'a>: Encode<P>,
    {
        let target = SocketAddr::new(hostname, 0);
        let mut buf: EchoRequestBuffer = [0; ECHO_REQUEST_BUFFER_SIZE];

        <_ as Encode<P>>::encode(req, &mut buf[..])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        debug!(
            ?target,
            // ident = ?req.ident, // identifier gets overwritten with DGRAM socks
            seq_cnt = ?req.seq_cnt,
            payload = ?req.payload,
            "encoded buffer with payload"
        );
        self.inner.send_to(&buf, &target).await?;
        if target.is_ipv4() {
            metrics::ICMPV4_REQUEST_COUNT.inc();
        } else {
            metrics::ICMPV6_REQUEST_COUNT.inc();
        }
        Ok(())
    }

    /// not cancel-safe
    pub async fn reply(&self) -> io::Result<(EchoReply, SocketAddr)>
    where
        EchoReply: Decode<P>,
    {
        let mut buf = [0; 1024];
        loop {
            let (n, addr) = self.inner.recv(&mut buf).await?;
            trace!(buf = ?&buf[..n], ?addr, "received data on socket");
            if let Ok(payload) = <EchoReply as Decode<P>>::decode(&buf[..n], self.decode_header) {
                if addr.is_ipv4() {
                    metrics::ICMPV4_REPLY_COUNT.inc();
                } else {
                    metrics::ICMPV6_REPLY_COUNT.inc();
                }
                return Ok((payload, addr));
            }
        }
    }
}

impl<P: Proto> Pinger<P> {
    fn new(
        host: IpAddr,
        socket: Arc<IcmpEcho<P>>,
        map: PingMap,
        // shutdown: Shutdown,
    ) -> Pinger<P> {
        Self {
            socket,
            host,
            ident: rand::random(),
            map,
            // shutdown,
            timeout: Duration::from_millis(500),
        }
    }

    pub fn timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = timeout;
        self
    }

    pub async fn ping(&self, seq_cnt: u16) -> errors::Result<PingReply>
    where
        for<'a> EchoRequest<'a>: Encode<P>,
    {
        let (tx, rx) = oneshot::channel();
        let payload = rand::random::<Token>();

        let ident = self.ident;
        self.map.lock().insert(
            payload,
            Ping {
                sent: Instant::now(),
                tx,
            },
        );
        // make sure map is always cleaned up, even if this future is dropped
        let guard = Guard {
            inner: self.map.clone(),
            payload,
        };
        let req = EchoRequest {
            ident,
            seq_cnt,
            payload: &payload,
        };
        let start = Instant::now();

        self.socket.request(self.host, &req).await?;
        debug!("sent echo request-- waiting for reply");
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(reply)) => {
                record_resp_metric(self.host.is_ipv4(), start);
                drop(guard);
                if reply.reply == req {
                    Ok(reply)
                } else {
                    Err(errors::Error::WrongReply { seq_cnt, payload })
                }
            }
            Ok(Err(err)) => {
                error!(?err, "error in oneshot receiver (sender likely dropped)");
                drop(guard);
                Err(errors::Error::RecvError {
                    ident,
                    seq_cnt,
                    err,
                })
            }
            Err(_err) => {
                debug!(elapsed = ?self.timeout, ?seq_cnt, ?payload, "ping timed out"); // ?ident
                drop(guard);
                Err(errors::Error::Timeout { ident, seq_cnt })
            }
        }
    }
}

fn record_resp_metric(is_ipv4: bool, start: Instant) {
    let elapsed = start.elapsed().as_secs_f64();
    if is_ipv4 {
        metrics::ICMPV4_REPLY_DURATION
            .with_label_values(&["reply"])
            .observe(elapsed);
    } else {
        metrics::ICMPV6_REPLY_DURATION
            .with_label_values(&["reply"])
            .observe(elapsed);
    }
}

// macro is just to copy-paste the contents for both Icmpv4 & Icmpv6
macro_rules! impl_icmp {
    ($t:ty) => {
        impl Listener<$t> {
            pub fn new() -> errors::Result<Listener<$t>> {
                let soc = Arc::new(IcmpEcho::<$t>::new()?);
                // when notify_shutdown is dropped, all pingers will shutdown
                let (notify_shutdown, _) = broadcast::channel(1);

                let r = soc.clone();
                let mut shutdown = Shutdown::new(notify_shutdown.subscribe());
                let map: PingMap = Arc::new(Mutex::new(HashMap::new()));

                let task_map = map.clone();
                task::spawn(async move {
                    loop {
                        tokio::select! {
                            ret = r.reply() => {
                                if let Ok((reply, addr)) = ret {
                                    debug!(?addr, ?reply, "received reply");
                                    let now = Instant::now();
                                    if let Some(ping) = task_map.lock().remove(&reply.payload[..]) {
                                        let time = now - ping.sent;
                                        if let Err(err) = ping.tx.send(PingReply { reply, addr, time }) {
                                            error!(?err, "error on oneshot sender (receiver likely dropped)");
                                        }
                                    } else {
                                        error!(?reply, ?addr, "received reply that we've already received or that we've never sent");
                                    }
                                }
                            }
                            _ = shutdown.recv() => {
                                debug!("ICMP listener shutdown received");
                                break;
                            }
                        }
                    }
                });

                Ok(Self {
                    inner: soc,
                    map,
                    // once Dropped, triggers shutdown in listener task (could also just abort()?)
                    notify_shutdown,
                })
            }

            /// explicitly stop the task that is spawned in `new`
            pub fn shutdown(self) {
                drop(self);
            }

            pub fn pinger(&self, host: IpAddr) -> Pinger<$t> {
                Pinger::new(
                    host,
                    self.inner.clone(),
                    self.map.clone(),
                    // Shutdown::new(self.notify_shutdown.subscribe()),
                )
            }
        }
    };
}

impl IcmpEcho<Icmpv4> {
    /// create a new ICMPv4 socket
    pub fn new() -> io::Result<Self> {
        let (inner, decode_header) = match Socket::new(Domain::IPV4, Type::DGRAM, Protocol::ICMPV4)
        {
            Ok(s) => (s, false),
            Err(err) => {
                error!(
                    ?err,
                    "error building DGRAM socket, check ping_group_range. trying RAW socket"
                );
                (
                    Socket::new(Domain::IPV4, Type::RAW, Protocol::ICMPV4)?,
                    true,
                )
            }
        };
        debug!("created new icmpv4 socket");
        Ok(Self {
            inner,
            decode_header,
            _phantom: PhantomData,
        })
    }
}

impl IcmpEcho<Icmpv6> {
    /// create a new ICMPv6 socket
    pub fn new() -> io::Result<Self> {
        let (inner, decode_header) = match Socket::new(Domain::IPV6, Type::DGRAM, Protocol::ICMPV6)
        {
            Ok(s) => (s, false),
            Err(err) => {
                warn!(
                    ?err,
                    "error building DGRAM socket, check ping_group_range. trying RAW socket"
                );
                (
                    Socket::new(Domain::IPV6, Type::RAW, Protocol::ICMPV6)?,
                    true,
                )
            }
        };
        debug!("created new icmpv6 socket");
        Ok(Self {
            inner,
            decode_header,
            _phantom: PhantomData,
        })
    }
}

/// A new pinger interface
pub struct Pinger<M> {
    socket: Arc<IcmpEcho<M>>,
    map: PingMap,
    host: IpAddr,
    // may get swapped out by kernel
    ident: u16,
    timeout: Duration,
    // shutdown: Shutdown,
}

#[derive(Debug)]
pub struct Ping {
    sent: Instant,
    tx: oneshot::Sender<PingReply>,
}

/// guard is a local variable that implements Drop
/// so that the shared hashmap can be cleaned up
struct Guard {
    inner: PingMap,
    payload: Token,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.inner.lock().remove(&self.payload);
    }
}

/// starts a new ICMP socket and a listener task for replies
/// new senders can be created with `pinger()`.
#[derive(Debug)]
pub struct Listener<M> {
    inner: Arc<IcmpEcho<M>>,
    map: PingMap,
    // on Drop this will stop our spawned task, but it is never read
    #[allow(dead_code)]
    notify_shutdown: broadcast::Sender<()>,
}

impl<M> Drop for Listener<M> {
    fn drop(&mut self) {
        debug!("ICMP listener dropped");
    }
}

/// a reply received on the ICMP socket
#[derive(Debug, Clone)]
pub struct PingReply {
    pub reply: EchoReply,
    pub addr: SocketAddr,
    pub time: Duration,
}

impl_icmp!(Icmpv4);
impl_icmp!(Icmpv6);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tracing_test::traced_test;

    #[tokio::test]
    #[traced_test]
    async fn test_icmpv4() -> io::Result<()> {
        let s = Arc::new(IcmpEcho::<Icmpv4>::new()?);
        let r = s.clone();
        let handle = tokio::spawn(async move {
            let r = r.reply().await?;
            Ok::<_, io::Error>(r)
        });
        let payload = rand::random::<Token>();
        s.request(
            "127.0.0.1".parse().unwrap(),
            &EchoRequest {
                ident: rand::random(),
                seq_cnt: 0,
                payload: &payload,
            },
        )
        .await?;

        let r = handle.await??;
        assert_eq!(r.0.seq_cnt, 0);
        Ok(())
    }

    // The gitlab CI does not support ICMPv6 it seems
    // #[tokio::test]
    // #[traced_test]
    // async fn test_icmpv6() -> io::Result<()> {
    //     let s = Arc::new(IcmpEcho::<Icmpv6>::new()?);
    //     let r = s.clone();
    //     let handle = tokio::spawn(async move {
    //         let r = r.reply().await?;
    //         Ok::<_, io::Error>(r)
    //     });
    //     let payload = rand::random::<Token>();
    //     s.request(
    //         "::1".parse().unwrap(),
    //         &EchoRequest {
    //             ident: rand::random(),
    //             seq_cnt: 0,
    //             payload: &payload,
    //         },
    //     )
    //     .await?;

    //     let r = handle.await??;
    //     assert_eq!(r.0.seq_cnt, 0);
    //     Ok(())
    // }

    #[tokio::test]
    #[traced_test]
    async fn test_listener() -> errors::Result<()> {
        let listener = Listener::<Icmpv4>::new()?;
        let pinger = listener.pinger("127.0.0.1".parse().unwrap());
        for i in 0..5 {
            let res = pinger.ping(i).await?;
            assert_eq!(res.reply.seq_cnt, i);
        }

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_multiping() -> errors::Result<()> {
        let listener = Listener::<Icmpv4>::new()?;
        let pinger = listener.pinger("1.1.1.1".parse().unwrap());
        let a = tokio::spawn(async move {
            for i in 1..5 {
                let res = pinger.ping(i).await?;
                assert_eq!(res.reply.seq_cnt, i);
            }
            Ok::<_, errors::Error>(())
        });
        let pinger = listener.pinger("8.8.8.8".parse().unwrap());
        let b = tokio::spawn(async move {
            for i in 1..5 {
                let res = pinger.ping(i).await?;
                assert_eq!(res.reply.seq_cnt, i);
            }
            Ok::<_, errors::Error>(())
        });
        let pinger = listener.pinger("1.0.0.1".parse().unwrap());
        let c = tokio::spawn(async move {
            for i in 10..15 {
                let res = pinger.ping(i).await?;
                assert_eq!(res.reply.seq_cnt, i);
            }
            Ok::<_, errors::Error>(())
        });
        let _ = tokio::try_join!(a, b, c).unwrap();
        Ok(())
    }
}

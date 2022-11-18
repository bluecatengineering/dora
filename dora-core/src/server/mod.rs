//! # Server
//!
//! Contains the main server code which handles reading from TCP/UDP and driving
//! the handlers/plugins to completion
use anyhow::{Context, Result};
use dhcproto::{v4, v6, Decodable, Encodable};
use pnet::datalink::NetworkInterface;
use tokio::sync::{broadcast, mpsc};
use tokio::time;
use tokio_stream::StreamExt;
use tracing::{debug, error, info, instrument, trace, warn};
use unix_udp_sock::{Source, Transmit, UdpSocket};

use std::{
    any::{Any, TypeId},
    fmt,
    future::Future,
    marker::Send,
    os::unix::prelude::{FromRawFd, IntoRawFd},
    sync::Arc,
    time::Duration,
};

pub mod context;
pub mod ioctl;
pub mod msg;
pub mod shutdown;
pub mod state;
pub mod topo_sort;
pub mod typemap;
pub(crate) mod udp;

use crate::{
    config::cli::{Config, ALL_DHCP_RELAY_AGENTS_AND_SERVERS},
    handler::*,
    server::{
        context::MsgContext, msg::SerialMsg, shutdown::Shutdown, topo_sort::DependencyTree,
        udp::UdpStream,
    },
};

/// Handy type alias for different `handle` traits
pub(crate) type PluginFn<T> = Arc<dyn Plugin<T>>;
pub(crate) type PostResponseFn<T> = Arc<dyn PostResponse<T>>;

/// Holds list of plugin handler methods, can be initialized with some `State` which will be
/// passed through to handlers via [`MsgContext`].
///
/// [`MsgContext`]: crate::server::context::MsgContext
pub struct Server<T> {
    /// all the plugins the server will use expressed as a dependency tree
    plugins: DependencyTree<TypeId, PluginFn<T>>,
    /// there can only be one post response plugin as it consumes `MsgContext<T>`
    postresponse: Option<PostResponseFn<T>>,
    /// additional application state
    state: State,
    /// server config
    config: Config,
    interfaces: Vec<NetworkInterface>,
}

impl<T> fmt::Debug for Server<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("state", &self.state)
            .field("config", &self.config)
            .finish()
    }
}

impl<T> Server<T>
where
    T: Encodable + Decodable + Send + Sync + 'static,
{
    /// Make a new instance of dora
    pub fn new(config: Config, interfaces: Vec<NetworkInterface>) -> Result<Server<T>> {
        let state = State::new(config.max_live_msgs);

        Ok(Server {
            plugins: DependencyTree::new(),
            postresponse: None,
            state,
            config,
            interfaces,
        })
    }
    /// Add plugin to the preresolve list of handlers
    pub fn plugin<P, U>(&mut self, plugin: U) -> &mut Self
    where
        U: Into<Arc<P>>,
        P: Plugin<T>,
    {
        self.plugin_order(plugin, &[])
    }

    /// Add plugin to the preresolve list of handlers, specifying dependencies
    pub fn plugin_order<P, U>(&mut self, plugin: U, dependencies: &[TypeId]) -> &mut Self
    where
        U: Into<Arc<P>>,
        P: Plugin<T>,
    {
        let plugin = plugin.into();
        let id = <P as Any>::type_id(&plugin);
        self.plugins.add(id, plugin, dependencies.as_ref());
        self
    }

    /// Add plugin to the postresponse list of handlers
    pub fn postresponse<P, U>(&mut self, plugin: U) -> &mut Self
    where
        U: Into<Arc<P>>,
        P: PostResponse<T>,
    {
        if self.postresponse.is_some() {
            warn!("Replacing postresponse plugin. There can only be one.");
        }
        self.postresponse.replace(plugin.into());
        self
    }

    /// consume `Server<T>` and return `Service<T>` which has the
    /// dependencies topologically sorted and in a list, shutdown handlers, etc
    fn into_service(self) -> Result<Service<T>> {
        let (notify_shutdown, _) = broadcast::channel(1);
        let (shutdown_complete_tx, shutdown_complete_rx) = mpsc::channel(1);
        Ok(Service {
            plugins: Arc::new(ServiceInner {
                plugins: self.plugins.topological_sort()?,
                postresponse: self.postresponse,
                config: self.config,
                interfaces: self.interfaces,
            }),
            state: Arc::new(self.state),
            notify_shutdown,
            shutdown_complete_tx,
            shutdown_complete_rx,
        })
    }
}

impl<T> ServiceInner<T>
where
    T: Encodable + Decodable + Send + Sync + 'static + fmt::Debug,
{
    /// if Some(()) - an encoded `MsgContext::decoded_resp_msg` will be sent to client
    /// if None - No response
    async fn run_handlers(&self, ctx: &mut MsgContext<T>) -> Option<()> {
        for handler in &*self.plugins {
            match handler.handle(ctx).await {
                Ok(Action::Respond) => return Some(()),
                Ok(Action::NoResponse) => {
                    // remove the resp_msg if we don't plan to send a response
                    ctx.decoded_resp_msg_mut().take();
                    return None;
                }
                Err(ref err) => {
                    warn!(?err);
                    // The client will not get a response if we encounter an error
                    return None;
                }
                // continue
                _ => {}
            }
        }
        Some(())
    }

    async fn run_post_response_handler(&self, mut ctx: MsgContext<T>) {
        ctx.mark_as_not_live();
        if let Some(ref handler) = self.postresponse {
            handler.handle(ctx).await;
        }
    }
}

/// Service is the type that actually does all the work, it listens
/// to the UDP socket, decodes dhcp message, spawns tasks, and waits
/// for a shutdown signal
pub(crate) struct Service<T> {
    pub(crate) notify_shutdown: broadcast::Sender<()>,
    pub(crate) shutdown_complete_tx: mpsc::Sender<()>,
    pub(crate) shutdown_complete_rx: mpsc::Receiver<()>,
    pub(crate) plugins: Arc<ServiceInner<T>>,
    /// reference to server state
    pub(crate) state: Arc<State>,
}

pub(crate) struct ServiceInner<T> {
    /// our list of plugins to execute
    plugins: Vec<PluginFn<T>>,
    /// the postresponse plugin
    postresponse: Option<PostResponseFn<T>>,
    /// reference to server config
    config: Config,
    interfaces: Vec<NetworkInterface>,
}

impl<T> fmt::Debug for Service<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service").finish()
    }
}

/// Abstraction for running handler chains
struct RunTask<T> {
    /// split inner so we can destructure separately
    inner: RunInner<T>,
    /// shutdown notifier
    shutdown: Shutdown,
    /// used to determine when all tasks have exited
    _shutdown_complete: mpsc::Sender<()>,
}

struct RunInner<T> {
    /// the currently processing message
    ctx: MsgContext<T>,
    /// reference to Service
    service: Arc<ServiceInner<T>>,
    /// socket to reply on
    soc: Arc<UdpSocket>,
    udpstate: Arc<unix_udp_sock::UdpState>,
}

impl RunInner<v4::Message> {
    /// Process handlers
    #[instrument(name = "v4", level = "debug", skip_all)]
    async fn run(mut self) -> Result<()> {
        if let Err(err) = self.ctx.recv_metrics() {
            error!(?err, "error counting recv metrics");
        }
        let timeout = self.service.config.timeout();
        let ifindex = self.ctx.meta().ifindex;
        let source = self.ctx.meta().dst_local_ip;
        let interface = self
            .service
            .interfaces
            .iter()
            .find(|int| int.index == ifindex)
            .with_context(|| format!("can't find interface {}", ifindex))?;
        trace!(meta = ?self.ctx.meta(), ?interface, "received datagram");

        let resp = match time::timeout(timeout, self.service.run_handlers(&mut self.ctx)).await {
            // WARNING: any use of `?` inside this block will return early and stop post_response from running
            Ok(Some(())) => {
                let iname = interface.name.as_str();
                let dst_addr = self.ctx.resp_addr(
                    self.service.config.is_default_port_v4(),
                    socket2::SockRef::from(&*self.soc),
                );

                if let Some(resp) = self.ctx.decoded_resp_msg() {
                    let msg_type = resp.opts().msg_type();
                    if let Ok(msg) = SerialMsg::from_msg(resp, dst_addr) {
                        // https://github.com/imp/dnsmasq/blob/master/src/forward.c#L70
                        // set source IP to the same IP that was used in recv'd destination (ipi_spec_dst)
                        // otherwise use iface idx
                        let packet_src =
                            source.map(Source::Ip).unwrap_or(Source::Interface(ifindex));
                        let transmit = Transmit::new(dst_addr, msg.msg()).src_ip(packet_src);

                        debug!(
                            msg_type = ?msg_type.context("messages must have a type")?,
                            ?dst_addr,
                            ?iname,
                            source = ?packet_src,
                            %resp,
                        );
                        self.ctx.set_dst_addr(dst_addr);
                        if let Err(err) = self.soc.send_msg(&self.udpstate, transmit).await {
                            error!(?err);
                        }
                    }
                }
                Ok(())
            }
            // no response
            Ok(None) => Ok(()),
            // drop timeouts
            Err(error) => Err(anyhow::anyhow!(error)),
        };
        if let Err(err) = self.ctx.sent_metrics() {
            error!(?err, "error counting sent metrics");
        }

        // run post-response handler, if any
        self.service.run_post_response_handler(self.ctx).await;
        resp
    }
}

impl RunTask<v4::Message> {
    async fn run(self) -> Result<()> {
        let RunTask {
            inner,
            mut shutdown,
            _shutdown_complete,
        } = self;
        tokio::select! {
            _ = shutdown.recv() => {
                trace!("task received shutdown notifier");
                Ok(())
            }
            res = inner.run() => {
                res
            }
        }
    }
}

impl RunTask<v6::Message> {
    async fn run(self) -> Result<()> {
        let RunTask {
            inner,
            mut shutdown,
            _shutdown_complete,
        } = self;
        tokio::select! {
            _ = shutdown.recv() => {
                trace!("task received shutdown notifier");
                Ok(())
            }
            res = inner.run() => {
                res
            }
        }
    }
}

impl RunInner<v6::Message> {
    /// Process handlers
    #[instrument(name = "v6", level = "debug", skip_all)]
    async fn run(mut self) -> Result<()> {
        if let Err(err) = self.ctx.recv_metrics() {
            error!(?err, "error counting recv metrics");
        }
        let timeout = self.service.config.timeout();
        let ifindex = self.ctx.meta().ifindex;
        let interface = self
            .service
            .interfaces
            .iter()
            .find(|int| int.index == ifindex)
            .with_context(|| format!("can't find interface {}", ifindex))?;
        trace!(meta = ?self.ctx.meta(), ?interface, "received datagram");

        let resp = match time::timeout(timeout, self.service.run_handlers(&mut self.ctx)).await {
            // WARNING: any use of `?` inside this block will return early and stop post_response from running
            Ok(Some(())) => {
                let iname = interface.name.as_str();
                let dst_addr = self.ctx.resp_addr(self.service.config.is_default_port_v6());

                if let Some(resp) = self.ctx.decoded_resp_msg() {
                    let msg_type = resp.msg_type();
                    if let Ok(msg) = SerialMsg::from_msg(resp, dst_addr) {
                        debug!(
                            ?msg_type,
                            ?dst_addr,
                            ?iname,
                            %resp,
                        );
                        self.ctx.set_dst_addr(dst_addr);
                        if let Err(err) = self.soc.send_to(msg.bytes(), dst_addr).await {
                            error!(?err);
                        }
                    }
                }
                Ok(())
            }
            // no response
            Ok(None) => Ok(()),
            // drop timeouts
            Err(error) => Err(anyhow::anyhow!(error)),
        };
        if let Err(err) = self.ctx.sent_metrics() {
            error!(?err, "error counting sent metrics");
        }
        // run post-response handler, if any
        self.service.run_post_response_handler(self.ctx).await;
        resp
    }
}

// This is unfortunate,
// the key problem is that Server/Service is defined over T, and yet
// they need to call send code to handle broadcast/multicast differently for v4/v6
// In order to do this statically we need
// to either copy/paste, use macros, or parameterize the send future.
// I'd rather let the compiler copy-paste for me, as parameterizing the future is not
// without its own hurdles (would require allocating the future see `experiment_runtask` branch).
macro_rules! impl_server {
    ($t:ty) => {
        impl Server<$t> {
            /// start server with parsed config values
            pub async fn start<F>(self, shutdown: F) -> Result<()>
            where
                F: Future<Output = Result<()>>,
            {
                self.listen(shutdown).await?;
                Ok(())
            }

            /// listen on a given address, consumes `self`
            /// The future startup_complete is intended for post startup tasks ex:
            /// setting dora's health status to Good and is required here as
            /// listen will not return unless an error occurs
            pub async fn listen<F>(self, shutdown: F) -> Result<()>
            where
                F: Future<Output = Result<()>>,
            {
                let mut service = self
                    .into_service()
                    .context("creating list of services failed in topological sort")?;

                tokio::select! {
                    res = service.listen() => {
                        if let Err(err) = res {
                            error!(?err, "error occurred in UDP listener");
                        }
                    }
                    res = shutdown => {
                        info!("caught shutdown signal handler");
                        if let Err(err) = res {
                            error!(?err);
                        }
                    }
                }

                info!("notifying tasks of shutdown...");
                let Service {
                    mut shutdown_complete_rx,
                    shutdown_complete_tx,
                    notify_shutdown,
                    ..
                } = service;

                // When `notify_shutdown` is dropped, all tasks which have `subscribe`d will
                // receive the shutdown signal and can exit
                drop(notify_shutdown);
                // Drop final `Sender` so the `Receiver` below can complete
                drop(shutdown_complete_tx);
                // Wait for all active tasks to finish processing. As the `Sender`
                // handle held by the listener has been dropped above, the only remaining
                // `Sender` instances are held by connection handler tasks. When those drop,
                // the `mpsc` channel will close and `recv()` will return `None`.
                if let Err(_) =
                    time::timeout(Duration::from_secs(3), shutdown_complete_rx.recv()).await
                {
                    error!("tasks did not finish within 3 seconds-- exiting anyway");
                } else {
                    info!("all tasks finished cleanly");
                }

                Ok(())
            }
        }

        impl Service<$t> {
            // handles listening on UDP and spawning a new task per `MsgContext` created
            // Also, we spawn a separate task that handles sending data on UDP to avoid
            // locking on the sender
            async fn listen(&mut self) -> Result<()> {
                let soc = self.create_socket().await?;

                let udp_recv = Arc::new(soc);
                let udp_send = Arc::clone(&udp_recv);
                let udp_state = Arc::new(unix_udp_sock::UdpState::new());

                let mut ctx_stream = UdpStream::<$t, _>::new(udp_recv, self.state.clone());
                while let Some(ctx) = ctx_stream.next().await {
                    if let Ok(ctx) = ctx {
                        self.state.inc_live_msgs().await;
                        let shutdown = Shutdown::new(self.notify_shutdown.subscribe());
                        let _shutdown_complete = self.shutdown_complete_tx.clone();
                        let task = RunTask {
                            inner: RunInner {
                                ctx,
                                soc: udp_send.clone(),
                                service: self.plugins.clone(),
                                udpstate: udp_state.clone(),
                            },
                            shutdown,
                            _shutdown_complete,
                        };
                        // TODO: when `JoinSet` is removed from unstable-- add handles
                        // here. Eventually, we should be able to use `CancellationToken`
                        // & `JoinSet` and have simpler shutdown code by avoiding
                        // broadcast/mpsc channels and explicit drops.
                        // Using JoinSet will likely mean that we no longer need `_shutdown_complete`
                        // and using CancellationToken will replace `shutdown`
                        tokio::spawn(task.run());
                    }
                }
                Ok(())
            }
        }
    };
}

impl_server!(v4::Message);
impl_server!(v6::Message);

impl Service<v4::Message> {
    #[instrument(name = "v4", level = "debug", skip_all)]
    async fn create_socket(&self) -> Result<unix_udp_sock::UdpSocket> {
        let addr = self.plugins.config.v4_addr;
        let interfaces = self.plugins.interfaces.clone();
        debug!(?addr, "binding UDP socket");
        let soc = if interfaces.len() == 1 {
            trace!("binding exactly one interface so use SO_BINDTODEVICE");
            // to bind to an interface, we must create the socket using libc
            let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
            // SO_BINDTODEVICE
            socket
                .bind_device(Some(interfaces.first().unwrap().name.as_bytes()))
                .context("failed to find interface")?;
            socket
                .set_nonblocking(true)
                .context("failed to set nonblocking mode on socket")?;
            socket
                .bind(&addr.into())
                .context("failed to bind interface")?;

            unix_udp_sock::UdpSocket::from_std(unsafe {
                std::net::UdpSocket::from_raw_fd(socket.into_raw_fd())
            })?
        } else {
            unix_udp_sock::UdpSocket::bind(addr).await?
        };
        soc.set_broadcast(true).context("failed to set_broadcast")?;
        Ok(soc)
    }
}

impl Service<v6::Message> {
    #[instrument(name = "v6", level = "debug", skip_all)]
    async fn create_socket(&self) -> Result<unix_udp_sock::UdpSocket> {
        let addr = self.plugins.config.v6_addr;
        let interfaces = self.plugins.interfaces.clone();
        debug!(?addr, "binding v6 UDP socket");
        let socket = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None)?;
        socket.set_only_v6(true).context("only ipv6")?;

        socket
            .set_reuse_address(true)
            .context("failed to set_reuse_address")?;
        socket
            .set_reuse_port(true)
            .context("failed to set_reuse_address")?;
        socket
            .set_nonblocking(true)
            .context("failed to set nonblocking mode on socket")?;
        socket
            .bind(&addr.into())
            .context("failed to bind interface")?;

        for int in &interfaces {
            debug!("joining multicast");

            socket
                .join_multicast_v6(&ALL_DHCP_RELAY_AGENTS_AND_SERVERS, int.index)
                .context("join v6 multicast")?;
            // socket
            //     .set_multicast_if_v6(int.index)
            //     .context("set multicast interface")?;
        }
        if interfaces.len() == 1 {
            trace!("binding exactly one interface, use SO_BINDTODEVICE");
            // to bind to an interface, we must create the socket using libc
            // SO_BINDTODEVICE
            socket
                .bind_device(Some(interfaces.first().unwrap().name.as_bytes()))
                .context("failed to find interface")?;
        }
        Ok(unix_udp_sock::UdpSocket::from_std(unsafe {
            std::net::UdpSocket::from_raw_fd(socket.into_raw_fd())
        })?)
    }
}

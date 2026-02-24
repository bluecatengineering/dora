use std::{
    fmt,
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    time::{Duration, SystemTime},
};

use client_protection::RenewThreshold;
use config::{
    DhcpConfig,
    v4::{NetRange, Network},
};
use ddns::DdnsUpdate;
use dora_core::{
    anyhow::anyhow,
    async_trait,
    chrono::{DateTime, SecondsFormat, Utc},
    dhcproto::v4::{DhcpOption, Message, MessageType, OptionCode},
    handler::{Action, Plugin},
    prelude::*,
    tracing::warn,
};
use ip_manager::IpState;
use message_type::MatchedClasses;
use static_addr::StaticAddr;

use crate::backend::{BackendError, LeaseBackend};

const OFFER_TIME: Duration = Duration::from_secs(60);

/// NATS-mode leases plugin that uses a `LeaseBackend` trait object.
pub struct NatsLeases {
    cfg: Arc<DhcpConfig>,
    ddns: DdnsUpdate,
    backend: Arc<dyn LeaseBackend>,
    renew_cache: Option<RenewThreshold<Vec<u8>>>,
}

impl fmt::Debug for NatsLeases {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NatsLeases")
            .field("cfg", &self.cfg)
            .field("backend", &self.backend)
            .finish()
    }
}

impl NatsLeases {
    pub fn new(cfg: Arc<DhcpConfig>, backend: Arc<dyn LeaseBackend>) -> Self {
        Self {
            renew_cache: cfg.v4().cache_threshold().map(RenewThreshold::new),
            backend,
            cfg,
            ddns: DdnsUpdate::new(),
        }
    }

    pub fn cache_threshold(&self, id: &[u8]) -> Option<Duration> {
        self.renew_cache
            .as_ref()
            .and_then(|cache| cache.threshold(id))
    }

    pub fn cache_remove(&self, id: &[u8]) {
        self.renew_cache
            .as_ref()
            .and_then(|cache| cache.remove(&id.to_vec()));
    }

    pub fn cache_insert(&self, id: &[u8], lease_time: Duration) {
        self.renew_cache.as_ref().and_then(|cache| {
            let old = cache.insert(id.to_vec(), lease_time);
            trace!(?old, ?id, "replacing old renewal time");
            old
        });
    }

    fn set_lease(
        &self,
        ctx: &mut MsgContext<Message>,
        (lease, t1, t2): (Duration, Duration, Duration),
        ip: Ipv4Addr,
        expires_at: SystemTime,
        classes: Option<&[String]>,
        range: &NetRange,
    ) -> Result<()> {
        ctx.resp_msg_mut()
            .context("response message must be set before leases is run")?
            .set_yiaddr(ip);
        ctx.populate_opts_lease(
            &self.cfg.v4().collect_opts(range.opts(), classes),
            lease,
            t1,
            t2,
        );
        ctx.set_local(ExpiresAt(expires_at));
        Ok(())
    }
}

impl dora_core::Register<Message> for NatsLeases {
    fn register(self, srv: &mut dora_core::Server<Message>) {
        info!("NatsLeases plugin registered");
        let this = Arc::new(self);
        srv.plugin_order::<Self, _>(this, &[std::any::TypeId::of::<StaticAddr>()]);
    }
}

#[async_trait]
impl Plugin<Message> for NatsLeases {
    #[instrument(level = "debug", skip_all)]
    async fn handle(&self, ctx: &mut MsgContext<Message>) -> Result<Action> {
        let req = ctx.msg();

        let client_id = self.cfg.v4().client_id(req).to_vec();
        let subnet = ctx.subnet()?;
        let network = self.cfg.v4().network(subnet);
        let classes = ctx.get_local::<MatchedClasses>().map(|c| c.0.to_owned());
        let resp_has_yiaddr = matches!(ctx.resp_msg(), Some(msg) if !msg.yiaddr().is_unspecified());
        let rapid_commit =
            ctx.msg().opts().get(OptionCode::RapidCommit).is_some() && self.cfg.v4().rapid_commit();
        let bootp = self.cfg.v4().bootp_enabled();

        match (req.opts().msg_type(), network) {
            (Some(MessageType::Discover), _) if resp_has_yiaddr => {
                return Ok(Action::Continue);
            }
            (Some(MessageType::Discover), Some(net)) => {
                self.nats_discover(ctx, &client_id, net, classes, rapid_commit)
                    .await
            }
            (Some(MessageType::Request), Some(net)) => {
                self.nats_request(ctx, &client_id, net, classes).await
            }
            (Some(MessageType::Release), _) => self.nats_release(ctx, &client_id).await,
            (Some(MessageType::Decline), Some(net)) => {
                self.nats_decline(ctx, &client_id, net).await
            }
            (_, Some(net)) if bootp => self.nats_bootp(ctx, &client_id, net, classes).await,
            _ => {
                debug!(?subnet, giaddr = ?req.giaddr(), "message type or subnet did not match");
                Ok(Action::NoResponse)
            }
        }
    }
}

impl NatsLeases {
    async fn nats_bootp(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
        classes: Option<Vec<String>>,
    ) -> Result<Action> {
        let expires_at = SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 7 * 12 * 40);
        let state = Some(IpState::Lease);
        let resp = self
            .nats_first_available(ctx, client_id, network, classes, expires_at, state)
            .await;
        ctx.filter_dhcp_opts();
        resp
    }

    async fn nats_first_available(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
        classes: Option<Vec<String>>,
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<Action> {
        let classes = classes.as_deref();

        if let Some(ip) = ctx.requested_ip() {
            if let Some(range) = network.range(ip, classes) {
                match self
                    .backend
                    .try_ip(
                        ip.into(),
                        network.subnet().into(),
                        client_id,
                        expires_at,
                        network,
                        state,
                    )
                    .await
                {
                    Ok(_) => {
                        debug!(
                            ?ip,
                            ?client_id,
                            expires_at = %print_time(expires_at),
                            range = ?range.addrs(),
                            subnet = ?network.subnet(),
                            mode = "nats",
                            "reserved IP for client-- sending offer"
                        );
                        let lease = range.lease().determine_lease(ctx.requested_lease_time());
                        self.set_lease(ctx, lease, ip, expires_at, classes, range)?;
                        return Ok(Action::Continue);
                    }
                    Err(BackendError::CoordinationUnavailable) => {
                        debug!(mode = "nats", "new allocation blocked: NATS unavailable");
                        return Ok(Action::NoResponse);
                    }
                    Err(err) => {
                        debug!(
                            ?err,
                            "could not assign requested IP, attempting to get new one"
                        );
                    }
                }
            }
        }

        for range in network.ranges_with_class(classes) {
            match self
                .backend
                .reserve_first(range, network, client_id, expires_at, state)
                .await
            {
                Ok(IpAddr::V4(ip)) => {
                    debug!(
                        ?ip,
                        ?client_id,
                        expires_at = %print_time(expires_at),
                        range = ?range.addrs(),
                        subnet = ?network.subnet(),
                        mode = "nats",
                        "reserved IP for client-- sending offer"
                    );
                    let lease = range.lease().determine_lease(ctx.requested_lease_time());
                    self.set_lease(ctx, lease, ip, expires_at, classes, range)?;
                    return Ok(Action::Continue);
                }
                Err(BackendError::CoordinationUnavailable) => {
                    debug!(mode = "nats", "new allocation blocked: NATS unavailable");
                    return Ok(Action::NoResponse);
                }
                Err(err) => {
                    debug!(?err, "error in nats reserve_first, trying next range");
                }
                _ => {}
            }
        }
        warn!(
            mode = "nats",
            "leases plugin did not assign ip in nats mode"
        );
        Ok(Action::NoResponse)
    }

    async fn nats_discover(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
        classes: Option<Vec<String>>,
        rapid_commit: bool,
    ) -> Result<Action> {
        let expires_at = SystemTime::now() + OFFER_TIME;
        let state = if rapid_commit {
            Some(IpState::Lease)
        } else {
            None
        };
        self.nats_first_available(ctx, client_id, network, classes, expires_at, state)
            .await
    }

    async fn nats_request(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
        classes: Option<Vec<String>>,
    ) -> Result<Action> {
        let ip = match ctx.requested_ip() {
            Some(ip) => ip,
            None if network.authoritative() => {
                debug!("no requested IP and we are authoritative, so NAK");
                ctx.update_resp_msg(MessageType::Nak)
                    .context("failed to set msg type")?;
                return Ok(Action::Respond);
            }
            None => {
                debug!("couldn't get requested IP, No response");
                return Ok(Action::NoResponse);
            }
        };

        let classes = classes.as_deref();
        let range = network.range(ip, classes);
        debug!(?ip, range = ?range.map(|r| r.addrs()), "is IP in range?");

        if let Some(range) = range {
            if let Some(remaining) = self.cache_threshold(client_id) {
                dora_core::metrics::RENEW_CACHE_HIT.inc();
                let lease = (
                    remaining,
                    config::renew(remaining),
                    config::rebind(remaining),
                );
                let expires_at = SystemTime::now() + lease.0;
                debug!(
                    ?ip,
                    ?client_id,
                    range = ?range.addrs(),
                    subnet = ?network.subnet(),
                    mode = "nats",
                    "reusing LEASE. client is attempting to renew inside of the renew threshold"
                );
                self.set_lease(ctx, lease, ip, expires_at, classes, range)?;
                return Ok(Action::Continue);
            }

            let lease = range.lease().determine_lease(ctx.requested_lease_time());
            let expires_at = SystemTime::now() + lease.0;

            match self
                .backend
                .try_lease(ip.into(), client_id, expires_at, network)
                .await
            {
                Ok(_) => {
                    debug!(
                        ?ip,
                        ?client_id,
                        expires_at = %print_time(expires_at),
                        range = ?range.addrs(),
                        subnet = ?network.subnet(),
                        mode = "nats",
                        "sending LEASE"
                    );
                    self.set_lease(ctx, lease, ip, expires_at, classes, range)?;
                    self.cache_insert(client_id, lease.0);

                    let dhcid = leases::dhcid(self.cfg.v4(), ctx.msg());
                    if let Err(err) = self
                        .ddns
                        .update(ctx, dhcid, self.cfg.v4().ddns(), range, ip, lease.0)
                        .await
                    {
                        error!(?err, "error during ddns update");
                    }
                    return Ok(Action::Continue);
                }
                Err(BackendError::CoordinationUnavailable) => {
                    debug!(
                        mode = "nats",
                        "lease blocked: NATS unavailable and not a known renewal"
                    );
                    if network.authoritative() {
                        ctx.update_resp_msg(MessageType::Nak)
                            .context("failed to set msg type")?;
                        return Ok(Action::Respond);
                    }
                    ctx.resp_msg_take();
                }
                Err(err) if network.authoritative() => {
                    debug!(?err, mode = "nats", "can't give out lease");
                    ctx.update_resp_msg(MessageType::Nak)
                        .context("failed to set msg type")?;
                    return Ok(Action::Respond);
                }
                Err(err) => {
                    debug!(
                        ?err,
                        mode = "nats",
                        "can't give out lease & not authoritative"
                    );
                    ctx.resp_msg_take();
                }
            }
            Ok(Action::Continue)
        } else {
            Ok(Action::Continue)
        }
    }

    async fn nats_release(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
    ) -> Result<Action> {
        let ip = ctx.msg().ciaddr().into();
        match self.backend.release_ip(ip, client_id).await {
            Ok(Some(info)) => {
                self.cache_remove(client_id);
                debug!(?info, mode = "nats", "released ip");
            }
            Ok(None) => {
                debug!(?ip, ?client_id, mode = "nats", "ip not found in storage");
            }
            Err(err) => {
                warn!(?err, mode = "nats", "error releasing IP");
            }
        }
        Ok(Action::NoResponse)
    }

    async fn nats_decline(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
    ) -> Result<Action> {
        let declined_ip = if let Some(DhcpOption::RequestedIpAddress(ip)) =
            ctx.msg().opts().get(OptionCode::RequestedIpAddress)
        {
            Ok(ip)
        } else {
            Err(anyhow!("decline has no option 50 (requested IP)"))
        }?;
        let expires_at = SystemTime::now() + network.probation_period();
        if let Err(err) = self
            .backend
            .probate_ip(
                (*declined_ip).into(),
                client_id,
                expires_at,
                network.subnet().into(),
            )
            .await
        {
            warn!(?err, mode = "nats", "error probating IP");
        }
        self.cache_remove(ctx.msg().chaddr());
        debug!(
            ?declined_ip,
            expires_at = %print_time(expires_at),
            mode = "nats",
            "added declined IP with probation set"
        );
        Ok(Action::Continue)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct ExpiresAt(pub SystemTime);

fn print_time(expires_at: SystemTime) -> String {
    DateTime::<Utc>::from(expires_at).to_rfc3339_opts(SecondsFormat::Secs, true)
}

#![warn(
    missing_debug_implementations,
    // missing_docs, // we shall remove thee, someday!
    rust_2018_idioms,
    unreachable_pub,
    non_snake_case,
    non_upper_case_globals
)]
#![deny(rustdoc::broken_intra_doc_links)]
#![allow(clippy::cognitive_complexity, clippy::too_many_arguments)]

const OFFER_TIME: Duration = Duration::from_secs(60);

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr},
    time::{Duration, SystemTime},
};

use client_protection::RenewThreshold;
use dora_core::{
    anyhow::anyhow,
    chrono::{DateTime, SecondsFormat, Utc},
    dhcproto::v4::{DhcpOption, Message, MessageType, OptionCode},
    metrics,
    prelude::*,
};
use message_type::MatchedClasses;
use register_derive::Register;
use static_addr::StaticAddr;

use config::{
    v4::{NetRange, Network},
    DhcpConfig,
};
use ip_manager::{IpError, IpManager, IpState, Storage};

#[derive(Register)]
#[register(msg(Message))]
#[register(plugin(StaticAddr))]
pub struct Leases<S>
where
    S: Storage,
{
    cfg: Arc<DhcpConfig>,
    ip_mgr: IpManager<S>,
    renew_cache: Option<RenewThreshold<Vec<u8>>>,
}

impl<S> fmt::Debug for Leases<S>
where
    S: Storage,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Leases").field("cfg", &self.cfg).finish()
    }
}

impl<S> Leases<S>
where
    S: Storage,
{
    pub fn new(cfg: Arc<DhcpConfig>, ip_mgr: IpManager<S>) -> Self {
        Self {
            renew_cache: cfg.v4().cache_threshold().map(RenewThreshold::new),
            ip_mgr,
            cfg,
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
        self.renew_cache
            .as_ref()
            // TODO: try to remove to_vec?
            .and_then(|cache| {
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

#[async_trait]
impl<S> Plugin<Message> for Leases<S>
where
    S: Storage + Send + Sync + 'static,
{
    #[instrument(level = "debug", skip_all)]
    async fn handle(&self, ctx: &mut MsgContext<Message>) -> Result<Action> {
        let req = ctx.msg();

        let client_id = self.cfg.v4().client_id(req).to_vec(); // to_vec required b/c of borrowck error
        let subnet = ctx.subnet()?;
        // look up that subnet from our config
        let network = self.cfg.v4().network(subnet);
        let classes = ctx.get_local::<MatchedClasses>().map(|c| c.0.to_owned());
        let resp_has_yiaddr = matches!(ctx.resp_msg(), Some(msg) if !msg.yiaddr().is_unspecified());
        let rapid_commit =
            ctx.msg().opts().get(OptionCode::RapidCommit).is_some() && self.cfg.v4().rapid_commit();
        let bootp = self.cfg.v4().bootp_enabled();

        match (req.opts().msg_type(), network) {
            // if yiaddr is set, then a previous plugin has already given the message an IP (like static)
            (Some(MessageType::Discover), _) if resp_has_yiaddr => {
                return Ok(Action::Continue);
            }
            // giaddr has matched one of our configured subnets
            (Some(MessageType::Discover), Some(net)) => {
                self.discover(ctx, &client_id, net, classes, rapid_commit)
                    .await
            }
            (Some(MessageType::Request), Some(net)) => {
                self.request(ctx, &client_id, net, classes).await
            }
            (Some(MessageType::Release), _) => self.release(ctx, &client_id).await,
            (Some(MessageType::Decline), Some(net)) => self.decline(ctx, &client_id, net).await,
            // if BOOTP enabled and no msg type
            // getting here means no static address has been assigned either
            (_, Some(net)) if bootp => self.bootp(ctx, &client_id, net, classes).await,
            _ => {
                debug!(?subnet, giaddr = ?req.giaddr(), "message type or subnet did not match");
                // NoResponse means no other plugin gets to try to send a message
                Ok(Action::NoResponse)
            }
        }
    }
}

impl<S> Leases<S>
where
    S: Storage,
{
    async fn bootp(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
        classes: Option<Vec<String>>,
    ) -> Result<Action> {
        // BOOTP addresses are forever
        // TODO: we should probably set the expiry time to NULL but for now, 40 years in the future
        let expires_at = SystemTime::now() + Duration::from_secs(60 * 60 * 24 * 7 * 12 * 40);
        let state = Some(IpState::Lease);
        let resp = self
            .first_available(ctx, client_id, network, classes, expires_at, state)
            .await;
        ctx.filter_dhcp_opts();

        resp
    }

    /// uses requested ip from client, or the first available IP in the range
    async fn first_available(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
        classes: Option<Vec<String>>,
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<Action> {
        let classes = classes.as_deref();
        // requested ip included in message, try to reserve
        if let Some(ip) = ctx.requested_ip() {
            // within our range. `range` makes sure IP is not in exclude list
            if let Some(range) = network.range(ip, classes) {
                match self
                    .ip_mgr
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
                           "reserved IP for client-- sending offer"
                        );
                        let lease = range.lease().determine_lease(ctx.requested_lease_time());
                        self.set_lease(ctx, lease, ip, expires_at, classes, range)?;
                        return Ok(Action::Continue);
                    }
                    // address in use from ping or cannot reserve this ip
                    // try to assign an IP
                    Err(err) => {
                        debug!(
                            ?err,
                            "could not assign requested IP, attempting to get new one"
                        );
                    }
                }
            }
        }
        // no requested IP, so find the next available
        for range in network.ranges_with_class(classes) {
            match self
                .ip_mgr
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
                        "reserved IP for client-- sending offer"
                    );
                    let lease = range.lease().determine_lease(ctx.requested_lease_time());
                    self.set_lease(ctx, lease, ip, expires_at, classes, range)?;
                    return Ok(Action::Continue);
                }
                Err(IpError::DbError(err)) => {
                    // log database error and try next IP
                    error!(?err);
                }
                _ => {
                    // all other errors try next
                }
            }
        }
        debug!("leases plugin did not assign ip");
        Ok(Action::NoResponse)
    }

    async fn discover(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
        classes: Option<Vec<String>>,
        rapid_commit: bool,
    ) -> Result<Action> {
        // give 60 seconds between discover & request, TODO: configurable?
        let expires_at = SystemTime::now() + OFFER_TIME;
        let state = if rapid_commit {
            Some(IpState::Lease)
        } else {
            None
        };
        self.first_available(ctx, client_id, network, classes, expires_at, state)
            .await
    }

    async fn request(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
        classes: Option<Vec<String>>,
    ) -> Result<Action> {
        // requested ip comes from opts or ciaddr
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
        // within our range
        let range = network.range(ip, classes);
        debug!(?ip, range = ?range.map(|r| r.addrs()), "is IP in range?");
        if let Some(range) = range {
            // if we got a recent renewal and the threshold has not past yet, return the existing lease time
            // TODO: move to ip-manager?
            if let Some(remaining) = self.cache_threshold(client_id) {
                metrics::RENEW_CACHE_HIT.inc();
                // lease was already handed out so it is valid for this range
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
                    "reusing LEASE. client is attempting to renew inside of the renew threshold"
                );
                self.set_lease(ctx, lease, ip, expires_at, classes, range)?;
                return Ok(Action::Continue);
            }
            // no lease info found -- calculate the lease time
            let lease = range.lease().determine_lease(ctx.requested_lease_time());
            let expires_at = SystemTime::now() + lease.0;

            match self
                .ip_mgr
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
                        "sending LEASE"
                    );
                    self.set_lease(ctx, lease, ip, expires_at, classes, range)?;
                    // insert lease into cache
                    self.cache_insert(client_id, lease.0);
                    return Ok(Action::Continue);
                }
                // ip not reserved or chaddr doesn't match
                Err(err) if network.authoritative() => {
                    debug!(?err, "can't give out lease");
                    ctx.update_resp_msg(MessageType::Nak)
                        .context("failed to set msg type")?;
                    return Ok(Action::Respond);
                }
                Err(err) => {
                    debug!(?err, "can't give out lease & not authoritative");
                    ctx.resp_msg_mut().take();
                }
            }
            Ok(Action::Continue)
        } else {
            Ok(Action::Continue)
        }
    }

    async fn release(&self, ctx: &mut MsgContext<Message>, client_id: &[u8]) -> Result<Action> {
        let ip = ctx.msg().ciaddr().into();
        if let Some(info) = self.ip_mgr.release_ip(ip, client_id).await? {
            self.cache_remove(client_id);
            debug!(?info, "released ip");
        } else {
            debug!(?ip, ?client_id, "ip not found in storage");
        }
        // release has no response
        Ok(Action::NoResponse)
    }

    async fn decline(
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
        self.ip_mgr
            .probate_ip((*declined_ip).into(), client_id, expires_at)
            .await?;
        // IP is decline, remove from cache
        self.cache_remove(ctx.msg().chaddr());
        debug!(
            ?declined_ip,
            expires_at = %print_time(expires_at),
            "added declined IP with probation set"
        );
        Ok(Action::Continue)
    }
}

/// When the lease will expire at
#[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct ExpiresAt(pub SystemTime);

fn print_time(expires_at: SystemTime) -> String {
    DateTime::<Utc>::from(expires_at).to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use dora_core::{
        dhcproto::{v4, Encodable},
        server::msg::SerialMsg,
        unix_udp_sock::RecvMeta,
    };
    use ip_manager::sqlite::SqliteDb;
    use std::net::SocketAddr;
    use tracing_test::traced_test;

    use super::*;

    #[test]
    fn test_time_print() {
        assert_eq!(
            print_time(SystemTime::UNIX_EPOCH),
            "1970-01-01T00:00:00Z".to_owned()
        );
    }

    static SAMPLE_YAML: &str = include_str!("../../../libs/config/sample/config.yaml");
    // static LONG_OPTS: &str = include_str!("../../../libs/config/sample/long_opts.yaml");

    #[tokio::test]
    #[traced_test]
    async fn test_request() -> Result<()> {
        let cfg = DhcpConfig::parse_str(SAMPLE_YAML).unwrap();
        // println!("{cfg:#?}");
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let leases = Leases::new(Arc::new(cfg.clone()), mgr);
        let mut ctx = blank_ctx(
            "192.168.0.1:67".parse()?,
            "192.168.0.1".parse()?,
            "192.168.0.1".parse()?,
            v4::MessageType::Request,
        )?;
        leases.handle(&mut ctx).await?;

        // no requested IP put in message, NAK
        assert!(ctx
            .resp_msg()
            .unwrap()
            .opts()
            .has_msg_type(v4::MessageType::Nak));

        let mut ctx = blank_ctx(
            "192.168.0.1:67".parse()?,
            "192.168.0.1".parse()?,
            "192.168.0.1".parse()?,
            v4::MessageType::Discover,
        )?;
        ctx.msg_mut()
            .opts_mut()
            .insert(v4::DhcpOption::RequestedIpAddress("192.168.1.100".parse()?));
        ctx.resp_msg_mut()
            .unwrap()
            .opts_mut()
            .insert(v4::DhcpOption::MessageType(v4::MessageType::Ack)); // ack is set in msg type plugin

        leases.handle(&mut ctx).await?;
        debug!(?ctx);
        // requested IP, OFFER
        assert!(ctx
            .resp_msg()
            .unwrap()
            .opts()
            .has_msg_type(v4::MessageType::Ack));

        Ok(())
    }

    fn blank_ctx(
        recv_addr: SocketAddr,
        siaddr: Ipv4Addr,
        giaddr: Ipv4Addr,
        msg_type: v4::MessageType,
    ) -> Result<MsgContext<dhcproto::v4::Message>> {
        let uns = Ipv4Addr::UNSPECIFIED;
        let mut msg = dhcproto::v4::Message::new(uns, uns, siaddr, giaddr, &[1, 2, 3, 4, 5, 6]);
        msg.opts_mut().insert(v4::DhcpOption::MessageType(msg_type));
        msg.opts_mut()
            .insert(v4::DhcpOption::SubnetSelection(giaddr));
        msg.opts_mut()
            .insert(v4::DhcpOption::ParameterRequestList(vec![
                v4::OptionCode::SubnetMask,
                v4::OptionCode::Router,
                v4::OptionCode::DomainNameServer,
                v4::OptionCode::DomainName,
            ]));
        let buf = msg.to_vec().unwrap();
        let meta = RecvMeta {
            addr: recv_addr,
            len: buf.len(),
            ..RecvMeta::default()
        };
        let resp = message_type::util::new_msg(&msg, siaddr, None, None);
        let mut ctx: MsgContext<dhcproto::v4::Message> = MsgContext::new(
            SerialMsg::new(buf.into(), recv_addr),
            meta,
            Arc::new(State::new(10)),
        )?;
        ctx.set_resp_msg(resp);
        Ok(ctx)
    }
}

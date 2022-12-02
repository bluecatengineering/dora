#![warn(
    missing_debug_implementations,
    // missing_docs, // we shall remove thee, someday!
    rust_2018_idioms,
    unreachable_pub,
    non_snake_case,
    non_upper_case_globals
)]
#![deny(rustdoc::broken_intra_doc_links)]
#![allow(clippy::cognitive_complexity)]

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr},
    time::{Duration, SystemTime},
};

use dora_core::{
    anyhow::anyhow,
    chrono::{DateTime, SecondsFormat, Utc},
    dhcproto::v4::{DhcpOption, Message, MessageType, OptionCode},
    prelude::*,
};
use register_derive::Register;
use static_addr::StaticAddr;

use config::{
    v4::{NetRange, Network},
    DhcpConfig,
};
use ip_manager::{IpError, IpManager, Storage};

#[derive(Register)]
#[register(msg(Message))]
#[register(plugin(StaticAddr))]
pub struct Leases<S>
where
    S: Storage,
{
    cfg: Arc<DhcpConfig>,
    ip_mgr: IpManager<S>,
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
        Self { cfg, ip_mgr }
    }

    async fn set_response(
        &self,
        network: &Network,
        ip: Ipv4Addr,
        range: &NetRange,
        client_id: &[u8],
        expires_at: SystemTime,
        ctx: &mut MsgContext<Message>,
    ) -> Result<()> {
        let (lease, t1, t2) = range.lease().determine_lease(ctx.requested_lease_time());
        debug!(
            ?ip,
            ?client_id,
            expires_at = %DateTime::<Utc>::from(expires_at).to_rfc3339_opts(SecondsFormat::Secs, true),
            range = ?range.addrs(),
            subnet = ?network.subnet(),
            "reserved requested ip"
        );
        ctx.decoded_resp_msg_mut()
            .context("response message must be set before leases is run")?
            .set_yiaddr(ip);
        ctx.populate_opts_lease(range.opts(), lease, t1, t2);
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
        let req = ctx.decoded_msg();

        let client_id = self.cfg.v4().client_id(req).to_vec(); // to_vec required b/c of borrowck error
                                                               // we could split the decoded_resp_msg from MsgContext to fix this?
        let subnet = ctx.subnet()?;
        // look up that subnet from our config
        let network = self.cfg.v4().get_network(subnet);
        let resp_has_yiaddr =
            matches!(ctx.decoded_resp_msg(), Some(msg) if !msg.yiaddr().is_unspecified());

        match (
            req.opts().msg_type().context("No message type found")?,
            network,
        ) {
            // if yiaddr is set, then a previous plugin has already given the message an IP (like static)
            (MessageType::Discover, _) if resp_has_yiaddr => {
                return Ok(Action::Continue);
            }
            // giaddr has matched one of our configured subnets
            (MessageType::Discover, Some(net)) => self.discover(ctx, &client_id, net).await,
            (MessageType::Request, Some(net)) => self.request(ctx, &client_id, net).await,
            (MessageType::Release, _) => self.release(ctx, &client_id).await,
            (MessageType::Decline, Some(net)) => self.decline(ctx, &client_id, net).await,
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
    async fn discover(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
    ) -> Result<Action> {
        let req = ctx.decoded_msg();
        // give 60 seconds between discover & request, TODO: configurable?
        let expires_at = SystemTime::now() + Duration::from_secs(60);
        // requested ip included in message, try to reserve
        if let Some(DhcpOption::RequestedIpAddress(ip)) =
            req.opts().get(OptionCode::RequestedIpAddress)
        {
            let ip = *ip;
            // within our range. `get_range` makes sure IP is not in exclude list
            if let Some(range) = network.get_range(ip) {
                match self
                    .ip_mgr
                    .try_ip(
                        ip.into(),
                        network.subnet().into(),
                        client_id,
                        expires_at,
                        network,
                    )
                    .await
                {
                    Ok(_) => {
                        self.set_response(network, ip, range, client_id, expires_at, ctx)
                            .await?;
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
        for range in network.ranges() {
            match self
                .ip_mgr
                .reserve_first(range, network, client_id, expires_at)
                .await
            {
                Ok(IpAddr::V4(ip)) => {
                    debug!(?ip, ?client_id, "got IP for client-- sending offer");
                    self.set_response(network, ip, range, client_id, expires_at, ctx)
                        .await?;
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

    async fn request(
        &self,
        ctx: &mut MsgContext<Message>,
        client_id: &[u8],
        network: &Network,
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

        // within our range
        let range = network.get_range(ip);
        debug!(?ip, range = ?range.map(|r| r.addrs()), "is IP in our range?");
        if let Some(range) = range {
            // calculate the lease time
            let (lease, t1, t2) = range.lease().determine_lease(ctx.requested_lease_time());
            let expires_at = SystemTime::now() + lease;
            match self
                .ip_mgr
                .try_lease(ip.into(), client_id, expires_at, network)
                .await
            {
                Ok(_) => {
                    ctx.decoded_resp_msg_mut()
                        .context("response message must be set before leases is run")?
                        .set_yiaddr(ip);
                    debug!(
                        ?ip,
                        ?client_id,
                        expires_at = %DateTime::<Utc>::from(expires_at).to_rfc3339_opts(SecondsFormat::Secs, true),
                        "leased requested ip"
                    );
                    ctx.populate_opts_lease(range.opts(), lease, t1, t2);
                    ctx.set_local(ExpiresAt(expires_at));
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
                    ctx.decoded_resp_msg_mut().take();
                }
            }
            Ok(Action::Continue)
        } else {
            Ok(Action::Continue)
        }
    }

    async fn release(&self, ctx: &mut MsgContext<Message>, client_id: &[u8]) -> Result<Action> {
        let ip = ctx.decoded_msg().ciaddr().into();
        if let Some(info) = self.ip_mgr.release_ip(ip, client_id).await? {
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
            ctx.decoded_msg().opts().get(OptionCode::RequestedIpAddress)
        {
            Ok(ip)
        } else {
            Err(anyhow!("decline has no option 50 (requested IP)"))
        }?;
        let expires_at = SystemTime::now() + network.probation_period();
        self.ip_mgr
            .probate_ip((*declined_ip).into(), client_id, expires_at)
            .await?;
        debug!(
            ?declined_ip,
            expires_at = %DateTime::<Utc>::from(expires_at).to_rfc3339_opts(SecondsFormat::Secs, true),
            "added declined IP with probation set"
        );
        Ok(Action::Continue)
    }
}

/// When the lease will expire at
#[derive(Debug, Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct ExpiresAt(pub SystemTime);

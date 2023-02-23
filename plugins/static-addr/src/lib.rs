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

use dora_core::{
    dhcproto::v4::{Message, MessageType},
    prelude::*,
};
use register_derive::Register;

use config::{v4::Reserved, DhcpConfig};
use message_type::{MatchedClasses, MsgType};

#[derive(Debug, Register)]
#[register(msg(Message))]
#[register(plugin(MsgType))]
pub struct StaticAddr {
    cfg: Arc<DhcpConfig>,
}

impl StaticAddr {
    pub fn new(cfg: Arc<DhcpConfig>) -> Result<Self> {
        Ok(Self { cfg })
    }
}

#[async_trait]
impl Plugin<Message> for StaticAddr {
    #[instrument(level = "debug", skip_all)]
    async fn handle(&self, ctx: &mut MsgContext<Message>) -> Result<Action> {
        let req = ctx.decoded_msg();
        let chaddr = req.chaddr().to_vec();

        let subnet = ctx.subnet()?;

        // matched classes clone necessary because of ctx borrowck
        let classes = ctx.get_local::<MatchedClasses>().map(|m| m.0.to_owned());
        let classes = classes.as_deref();
        if let Some(net) = self.cfg.v4().network(subnet) {
            // determine if we have a reservation based on mac
            if chaddr.len() == 6 {
                let mac = MacAddr::new(
                    chaddr[0], chaddr[1], chaddr[2], chaddr[3], chaddr[4], chaddr[5],
                );
                if let Some(res) = net.get_reserved_mac(mac, classes) {
                    // mac is present in our config
                    match req.opts().msg_type().context("no message type found")? {
                        MessageType::Discover => self.discover(ctx, &chaddr, classes, res)?,
                        MessageType::Request => self.request(ctx, &chaddr, classes, res)?,
                        // we have a reservation, but we didn't et a DISCOVER or REQUEST
                        // drop the message
                        _ => return Ok(Action::NoResponse),
                    };
                    return Ok(Action::Continue);
                }
            }

            // determine if we have a reservation based on opt
            if let Some(res) = net.search_reserved_opt(req.opts(), classes) {
                // matching opt is present in our config
                match req.opts().msg_type().context("no message type found")? {
                    MessageType::Discover => self.discover(ctx, &chaddr, classes, res)?,
                    MessageType::Request => self.request(ctx, &chaddr, classes, res)?,
                    // we have a reservation, but we didn't et a DISCOVER or REQUEST
                    // drop the message
                    _ => return Ok(Action::NoResponse),
                };
                return Ok(Action::Continue);
            }
        }
        Ok(Action::Continue)
    }
}

impl StaticAddr {
    #[inline]
    fn discover(
        &self,
        ctx: &mut MsgContext<Message>,
        chaddr: &[u8],
        classes: Option<&[String]>,
        res: &Reserved,
    ) -> Result<Action> {
        let static_ip = res.ip();
        let (lease, t1, t2) = res.lease().determine_lease(ctx.requested_lease_time());
        debug!(?static_ip, ?chaddr, "use static requested ip");
        ctx.decoded_resp_msg_mut()
            .context("response message must be set before leases is run")?
            .set_yiaddr(static_ip);
        ctx.populate_opts_lease(
            &self.cfg.v4().collect_opts(res.opts(), classes),
            lease,
            t1,
            t2,
        );
        Ok(Action::Continue)
    }

    #[inline]
    fn request(
        &self,
        ctx: &mut MsgContext<Message>,
        chaddr: &[u8],
        classes: Option<&[String]>,
        res: &Reserved,
    ) -> Result<Action> {
        let static_ip = res.ip();
        // requested ip comes from opts or ciaddr
        let ip = if let Some(ip) = ctx.requested_ip() {
            ip
        } else {
            ctx.update_resp_msg(MessageType::Nak)
                .context("failed to set msg type")?;
            return Ok(Action::Respond);
        };

        if ip != static_ip {
            debug!(
                ?chaddr,
                ?ip,
                ?static_ip,
                "configured static ip does not match"
            );
            ctx.update_resp_msg(MessageType::Nak)
                .context("failed to set msg type")?;
            return Ok(Action::Respond);
        }

        let (lease, t1, t2) = res.lease().determine_lease(ctx.requested_lease_time());
        ctx.decoded_resp_msg_mut()
            .context("response message must be set before static plugin is run")?
            .set_yiaddr(ip);
        ctx.populate_opts_lease(
            &self.cfg.v4().collect_opts(res.opts(), classes),
            lease,
            t1,
            t2,
        );
        trace!(?ip, "populating response with static ip");

        Ok(Action::Continue)
    }
}

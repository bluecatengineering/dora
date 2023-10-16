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

use client_protection::FloodCache;
use dora_core::{
    dhcproto::{
        v4::{DhcpOption, Message, MessageType, Opcode, OptionCode},
        v6,
    },
    metrics,
    prelude::*,
    tracing::warn,
};
use register_derive::Register;
use std::fmt::Debug;

use config::{client_classes, DhcpConfig};

#[derive(Register)]
#[register(msg(Message))]
#[register(msg(v6::Message))]
#[register(plugin())]
pub struct MsgType {
    cfg: Arc<DhcpConfig>,
    flood: Option<FloodCache<Vec<u8>>>,
}

impl Debug for MsgType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MsgType").field("cfg", &self.cfg).finish()
    }
}

impl MsgType {
    pub fn new(cfg: Arc<DhcpConfig>) -> Result<Self> {
        Ok(Self {
            flood: cfg.v4().flood_threshold().map(FloodCache::new),
            cfg,
        })
    }

    pub fn flood_check(&self, id: &Vec<u8>) -> bool {
        self.flood
            .as_ref()
            .map(|flood| flood.is_allowed(id))
            .unwrap_or(true)
    }
}

#[async_trait]
impl Plugin<Message> for MsgType {
    #[instrument(level = "debug", skip_all)]
    async fn handle(&self, ctx: &mut MsgContext<Message>) -> Result<Action> {
        // set the interface, using data from config
        // MsgType plugin must run first because future plugins use this data
        let meta = ctx.meta();
        let interface = self
            .cfg
            .v4()
            .find_network(meta.ifindex)
            .context("interface message was received on does not exist?")?;
        ctx.set_interface(interface);

        let req = ctx.msg();
        let msg_type = req.opts().msg_type();

        let subnet = ctx.subnet()?;
        debug!(
            opcode = ?req.opcode(),
            msg_type = ?msg_type,
            src_addr = %ctx.src_addr(),
            ?subnet,
            req = %ctx.msg(),
        );

        let client_id = self.cfg.v4().client_id(req).to_vec(); // to_vec required b/c of borrowck error
        if !self.flood_check(&client_id) {
            metrics::FLOOD_THRESHOLD_COUNT.inc();
            debug!(
                ?client_id,
                "client is chatty, engaging rate limit and not responding"
            );
            return Ok(Action::NoResponse);
        }
        // otherwise our interface IP as the id
        let server_id = self
            .cfg
            .v4()
            .server_id(meta.ifindex, subnet)
            .context("cannot find server_id")?;
        // look up which network the message belongs to
        let network = self.cfg.v4().network(subnet);
        let sname = network.and_then(|net| net.server_name());
        let fname = network.and_then(|net| net.file_name());
        // message that will be returned
        let mut resp = util::new_msg(req, server_id, sname, fname);

        // if there is a server identifier it must match ours
        if matches!(req.opts().get(OptionCode::ServerIdentifier), Some(DhcpOption::ServerIdentifier(id)) if *id != server_id && !id.is_unspecified())
        {
            debug!(?server_id, "server identifier in msg doesn't match");
            return Ok(Action::NoResponse);
        }
        if req.opcode() == Opcode::BootReply {
            debug!("BootReply not supported");
            return Ok(Action::NoResponse);
        }
        // add server id to response
        resp.opts_mut()
            .insert(DhcpOption::ServerIdentifier(server_id));
        // evaluate client classes
        let matched = util::client_classes(self.cfg.v4(), ctx)?;
        let addr = {
            let ciaddr = ctx.msg().ciaddr();
            if !ciaddr.is_unspecified() {
                ciaddr
            } else {
                // TODO: when `subnet` is used to select a range, it probably doesn't exist.
                subnet
            }
        };
        let rapid_commit =
            ctx.msg().opts().get(OptionCode::RapidCommit).is_some() && self.cfg.v4().rapid_commit();

        match msg_type {
            Some(MessageType::Discover) if rapid_commit => {
                resp.opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Ack));
            }
            Some(MessageType::Discover) => {
                resp.opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Offer));
            }
            Some(MessageType::Request) => {
                if req.giaddr().is_unspecified() {
                    resp.set_flags(req.flags().set_broadcast());
                }
                resp.opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Ack));
            }
            Some(MessageType::Release) => {
                resp.opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Ack));
            }
            // got INFORM & we are authoritative, give a response
            Some(MessageType::Inform) if matches!(network, Some(net) if net.authoritative()) => {
                resp.opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Ack));

                if let Some(range) = self.cfg.v4().range(addr, addr, matched.as_deref()) {
                    ctx.set_resp_msg(resp);
                    ctx.populate_opts(
                        &self.cfg.v4().collect_opts(range.opts(), matched.as_deref()),
                    );
                    if let Some(classes) = matched {
                        ctx.set_local(MatchedClasses(classes));
                    }
                    return Ok(Action::Respond);
                }
                warn!(msg_type = ?MessageType::Inform, "couldn't match appropriate range with INFORM message");
            }
            Some(MessageType::Decline) => {
                if let Some(DhcpOption::RequestedIpAddress(ip)) =
                    req.opts().get(OptionCode::RequestedIpAddress)
                {
                    debug!(declined_ip = ?ip, "got DECLINE");
                    return Ok(Action::Continue);
                } else {
                    // TODO: is this a real case? AFAIK all declines must include the IP
                    error!("got DECLINE with no option 50 (requested IP)");
                    return Ok(Action::NoResponse);
                }
            }
            None if req.opcode() == Opcode::BootRequest && self.cfg.v4().bootp_enabled() => {
                // No message type but BOOTREQUEST, this is a BOOTP message
                ctx.set_resp_msg(resp);
                return Ok(Action::Continue);
            }
            _ => {
                debug!("unsupported message type");
                return Ok(Action::NoResponse);
            }
        }

        if let Some(classes) = matched {
            if classes
                .iter()
                .any(|class| class == client_classes::client_classification::DROP_CLASS)
            {
                // contains DROP class, drop packet
                debug!("DROP class matched");
                return Ok(Action::NoResponse);
            }
            ctx.set_local(MatchedClasses(classes));
        }
        ctx.set_resp_msg(resp);
        Ok(Action::Continue)
    }
}

pub mod util {
    use config::{client_classes::client_classification::PacketDetails, v4::Config};

    use super::*;

    pub fn new_msg(
        req: &Message,
        siaddr: Ipv4Addr,
        sname: Option<&str>,
        fname: Option<&str>,
    ) -> Message {
        let mut msg = Message::new_with_id(
            req.xid(),
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            siaddr,
            req.giaddr(),
            req.chaddr(),
        );
        msg.set_opcode(Opcode::BootReply)
            .set_htype(req.htype())
            .set_flags(req.flags())
            .set_hops(req.hops());
        // set the sname & fname header fields
        if let Some(sname) = sname {
            msg.set_sname_str(sname);
        }
        if let Some(fname) = fname {
            msg.set_fname_str(fname);
        }
        msg
    }

    pub fn packet_details(cfg: &Config, meta: RecvMeta) -> Result<PacketDetails<'_>> {
        Ok(PacketDetails {
            iface: cfg
                .find_interface(meta.ifindex)
                .context("could not find interface")?
                .name
                .as_str(),
            src: match meta.addr.ip() {
                IpAddr::V4(ip) => ip,
                IpAddr::V6(_ip) => {
                    // this error shouldn't happen but we'll cover it anyway
                    return Err(anyhow::anyhow!(
                        "addr recvd an ipv6 address for ipv4 message"
                    ));
                }
            },
            dst: match meta.dst_ip.context("no destination ip on recvd message")? {
                IpAddr::V4(ip) => ip,
                IpAddr::V6(_ip) => {
                    return Err(anyhow::anyhow!(
                        "dst_ip recvd an ipv6 address for ipv4 message"
                    ))
                }
            },
            len: meta.len,
        })
    }

    pub fn client_classes(cfg: &Config, ctx: &MsgContext<Message>) -> Result<Option<Vec<String>>> {
        // TODO: what should we do if there is an error processing client classes?
        Ok(cfg
            .eval_client_classes(ctx.msg(), util::packet_details(cfg, ctx.meta())?)
            .and_then(|classes| match classes {
                Ok(classes) => {
                    debug!(matched_classes = ?classes, "matched classes");
                    Some(classes)
                }
                Err(err) => {
                    error!(?err, "error processing client classes");
                    None
                }
            }))
    }

    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use anyhow::Result;
    use dhcproto::{v4, Encodable};
    use dora_core::server::msg::SerialMsg;
    use unix_udp_sock::RecvMeta;

    /// for testing
    pub fn blank_ctx(
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
            ifindex: 1,
            // recv addr copied here
            dst_ip: Some(recv_addr.ip()),
            ..RecvMeta::default()
        };
        let resp = crate::util::new_msg(&msg, siaddr, None, None);
        let mut ctx: MsgContext<dhcproto::v4::Message> = MsgContext::new(
            SerialMsg::new(buf.into(), recv_addr),
            meta,
            Arc::new(State::new(10)),
        )?;
        ctx.set_resp_msg(resp);
        Ok(ctx)
    }
}

#[async_trait]
impl Plugin<v6::Message> for MsgType {
    #[instrument(level = "debug", skip_all)]
    async fn handle(&self, ctx: &mut MsgContext<v6::Message>) -> Result<Action> {
        // import message type variants
        use v6::MessageType::*;
        // set the interface, using data from config
        // MsgType plugin must run first because future plugins use this data
        let meta = ctx.meta();
        let interface = self
            .cfg
            .v6()
            .get_interface_link_local(meta.ifindex)
            .context("no link-local address on interface?")?;
        ctx.set_interface(interface);

        if let Some(global_unicast) = self.cfg.v6().get_interface_global(meta.ifindex) {
            ctx.set_global(global_unicast);
        }

        let req = ctx.msg();
        let msg_type = req.msg_type();

        debug!(
            ?msg_type,
            %interface,
            global = ?ctx.global(),
            src_addr = %ctx.src_addr(),
            req = %ctx.msg(),
        );

        // let network = self.cfg.v6().get_network(meta.ifindex);

        // create initial response with reply type
        let mut resp = v6::Message::new_with_id(Reply, req.xid());

        let server_id = self.cfg.v6().server_id();
        // TODO RelayForw type
        // TODO: make sure we handle client ids as specified - https://www.rfc-editor.org/rfc/rfc8415#section-16.1
        let req_sid = req.opts().get(v6::OptionCode::ServerId);
        // if the request includes a server id, it must match our server id
        if matches!(req_sid, Some(v6::DhcpOption::ServerId(id)) if *id != server_id) {
            debug!(?server_id, "server identifier in msg doesn't match");
            return Ok(Action::NoResponse);
        }
        // add server id to response
        resp.opts_mut()
            .insert(v6::DhcpOption::ServerId(server_id.to_vec()));

        match msg_type {
            // discard if it has these types but NO server id
            // https://www.rfc-editor.org/rfc/rfc8415#section-16.6
            Request | Renew | Decline | Release if req_sid.is_none() => {
                return Ok(Action::NoResponse);
            }
            InformationRequest => {
                if let Some(opts) = self.cfg.v6().get_opts(meta.ifindex) {
                    ctx.set_resp_msg(resp);
                    ctx.populate_opts(opts);
                    return Ok(Action::Respond);
                }

                warn!(
                    ?msg_type,
                    "couldn't match any options with INFORMATION-REQUEST message"
                );
            }
            _ => {
                debug!("currently unsupported message type");
                return Ok(Action::NoResponse);
            }
        }

        ctx.set_resp_msg(resp);
        Ok(Action::Continue)
    }
}

/// a list of matching client classes for this message
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedClasses(pub Vec<String>);

#[cfg(test)]
mod tests {
    use dora_core::dhcproto::v4;
    use tracing_test::traced_test;

    use super::*;

    static SAMPLE_YAML: &str = include_str!("../../../libs/config/sample/config.yaml");

    #[tokio::test]
    #[traced_test]
    async fn test_request() -> Result<()> {
        let cfg = DhcpConfig::parse_str(SAMPLE_YAML).unwrap();
        let plugin = MsgType::new(Arc::new(cfg.clone()))?;
        let mut ctx = util::blank_ctx(
            "192.168.0.1:67".parse()?,
            "192.168.0.1".parse()?,
            "192.168.0.1".parse()?,
            v4::MessageType::Request,
        )?;
        plugin.handle(&mut ctx).await?;

        assert!(ctx
            .resp_msg()
            .unwrap()
            .opts()
            .has_msg_type(v4::MessageType::Ack));
        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_discover() -> Result<()> {
        let cfg = DhcpConfig::parse_str(SAMPLE_YAML).unwrap();
        let plugin = MsgType::new(Arc::new(cfg.clone()))?;
        let mut ctx = util::blank_ctx(
            "192.168.0.1:67".parse()?,
            "192.168.0.1".parse()?,
            "192.168.0.1".parse()?,
            v4::MessageType::Discover,
        )?;
        plugin.handle(&mut ctx).await?;

        assert!(ctx
            .resp_msg()
            .unwrap()
            .opts()
            .has_msg_type(v4::MessageType::Offer));
        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_bootp() -> Result<()> {
        let cfg = DhcpConfig::parse_str(SAMPLE_YAML).unwrap();
        let plugin = MsgType::new(Arc::new(cfg.clone()))?;
        let mut ctx = util::blank_ctx(
            "192.168.0.1:67".parse()?,
            "192.168.0.1".parse()?,
            "192.168.0.1".parse()?,
            v4::MessageType::Request,
        )?;
        // remove msg type so we're bootp
        ctx.msg_mut().opts_mut().remove(v4::OptionCode::MessageType);
        plugin.handle(&mut ctx).await?;

        assert!(ctx.resp_msg().unwrap().opts().msg_type().is_none());
        Ok(())
    }
}

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
use std::{fmt::Debug, net::Ipv4Addr};

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
        let cfg_server_id = self
            .cfg
            .v4()
            .server_id(meta.ifindex, subnet)
            .context("cannot find server_id")?;
        // look up which network the message belongs to
        let network = self.cfg.v4().network(subnet);
        let sname = network.and_then(|net| net.server_name());
        let fname = network.and_then(|net| net.file_name());
        // message that will be returned
        let mut resp = util::new_msg(req, cfg_server_id, sname, fname);

        // determine the server id to use in the response message
        let resp_server_id = RespServerId::new(cfg_server_id, req);

        if let Some(server_id) = resp_server_id.get() {
            // add the correct server identifier to response
            resp.opts_mut()
                .insert(DhcpOption::ServerIdentifier(server_id));
        } else {
            debug!(
                ?cfg_server_id,
                "server identifier in msg doesn't match server address or server id override"
            );
            return Ok(Action::NoResponse);
        }
        if req.opcode() == Opcode::BootReply {
            debug!("BootReply not supported");
            return Ok(Action::NoResponse);
        }

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

/// supports 3 variants:
/// CfgServerId - the server id retrieved from the config
/// ServerIdOverride - the server id override retrieved from the RAI in the message
/// None - no valid server id, we should not process the message
enum RespServerId {
    CfgServerId(Ipv4Addr),
    ServerIdOverride(Ipv4Addr),
    None,
}

impl RespServerId {
    /// returns either the server id override or the server id from the config (RFC 5107)
    fn new(cfg_server_id: Ipv4Addr, req: &Message) -> Self {
        // get the server id and server id override from the message
        let server_id_override = util::get_server_id_override(req.opts());
        let msg_server_id_opt = req.opts().get(OptionCode::ServerIdentifier);

        if let Some(&DhcpOption::ServerIdentifier(msg_id)) = msg_server_id_opt {
            // if the server override matches the msg server id, we should respond
            if let Some(override_id) = server_id_override {
                if override_id == msg_id {
                    return Self::ServerIdOverride(override_id);
                }
            }
            // we should not respond if the server id from the config does not match the msg server id and
            // the msg server id is not unspecified
            if cfg_server_id != msg_id && !msg_id.is_unspecified() {
                return Self::None;
            }
        }
        Self::CfgServerId(cfg_server_id)
    }

    fn get(&self) -> Option<Ipv4Addr> {
        match self {
            Self::CfgServerId(addr) => Some(*addr),
            Self::ServerIdOverride(addr) => Some(*addr),
            Self::None => None,
        }
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

    pub fn blank_ctx_v6(
        recv_addr: SocketAddr,
        ifindex: u32,
        msg_type: v6::MessageType,
    ) -> Result<MsgContext<dhcproto::v6::Message>> {
        let msg = dhcproto::v6::Message::new(msg_type);
        let buf = msg.to_vec().unwrap();
        let meta = RecvMeta {
            addr: recv_addr,
            len: buf.len(),
            ifindex,
            // recv addr copied here
            dst_ip: Some(recv_addr.ip()),
            ..RecvMeta::default()
        };
        let ctx: MsgContext<dhcproto::v6::Message> = MsgContext::new(
            SerialMsg::new(buf.into(), recv_addr),
            meta,
            Arc::new(State::new(10)),
        )?;
        Ok(ctx)
    }

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

    /// Convenience for RFC 5107 compliance. Fetches the ServerIdentifierOverride suboption (11) from
    /// RelayAgentInformation (82) to use in comparisons between the server id and override id.
    pub fn get_server_id_override(opts: &v4::DhcpOptions) -> Option<Ipv4Addr> {
        // fetch the RelayAgentInformation option (option 82)
        if let Some(DhcpOption::RelayAgentInformation(relay_info)) =
            opts.get(OptionCode::RelayAgentInformation)
        {
            // fetch the ServerIdentifierOverride suboption (suboption 11) from the relay information
            let override_info = relay_info.get(v4::relay::RelayCode::ServerIdentifierOverride);
            if let Some(v4::relay::RelayInfo::ServerIdentifierOverride(addr)) = override_info {
                return Some(*addr);
            }
        }
        None
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

        let rapid_commit = ctx.msg().opts().get(v6::OptionCode::RapidCommit).is_some()
            && self.cfg.v4().rapid_commit();
        let server_id = self.cfg.v6().server_id();
        // TODO RelayForw type
        // TODO: make sure we handle client ids as specified - https://www.rfc-editor.org/rfc/rfc8415#section-16.1
        let req_sid = req.opts().get(v6::OptionCode::ServerId);
        let req_cid = req.opts().get(v6::OptionCode::ClientId);
        // if the request includes a server id, it must match our server id
        if matches!(req_sid, Some(v6::DhcpOption::ServerId(id)) if *id != server_id) {
            debug!(?server_id, "server identifier in msg doesn't match");
            return Ok(Action::NoResponse);
        }
        // add server id to response
        resp.opts_mut()
            .insert(v6::DhcpOption::ServerId(server_id.to_vec()));

        match msg_type {
            Solicit => {
                //https://datatracker.ietf.org/doc/html/rfc8415#section-16.2
                if req_sid.is_some() || req_cid.is_none() {
                    return Ok(Action::NoResponse);
                }
                if rapid_commit {
                    resp.set_msg_type(v6::MessageType::Reply);
                } else {
                    resp.set_msg_type(v6::MessageType::Advertise);
                }
                //TODO: discard if req not fulfill administrative policy
            }
            Request => {
                // https://datatracker.ietf.org/doc/html/rfc8415#section-16.4
                if req_sid.is_none() || req_cid.is_none() {
                    return Ok(Action::NoResponse);
                }
                resp.set_msg_type(v6::MessageType::Reply);
            }
            Confirm => {
                // https://datatracker.ietf.org/doc/html/rfc8415#section-16.5
                if req_sid.is_some() || req_cid.is_none() {
                    return Ok(Action::NoResponse);
                }
                resp.set_msg_type(v6::MessageType::Reply);
            }
            Renew => {
                // https://datatracker.ietf.org/doc/html/rfc8415#section-16.6
                if req_sid.is_none() || req_cid.is_none() {
                    return Ok(Action::NoResponse);
                }
                resp.set_msg_type(v6::MessageType::Reply);
            }
            Rebind => {
                // https://datatracker.ietf.org/doc/html/rfc8415#section-16.7
                if req_sid.is_some() || req_cid.is_none() {
                    return Ok(Action::NoResponse);
                }
                resp.set_msg_type(v6::MessageType::Reply);
            }
            Decline => {
                // https://datatracker.ietf.org/doc/html/rfc8415#section-16.8
                if req_sid.is_none() || req_cid.is_none() {
                    return Ok(Action::NoResponse);
                }
                resp.set_msg_type(v6::MessageType::Reply);
            }
            Release => {
                // https://datatracker.ietf.org/doc/html/rfc8415#section-16.9
                if req_sid.is_none() || req_cid.is_none() {
                    return Ok(Action::NoResponse);
                }
                resp.set_msg_type(v6::MessageType::Reply);
            }
            InformationRequest => {
                // discard if req has IA option
                // https://datatracker.ietf.org/doc/html/rfc8415#section-16.12
                if req.opts().get(v6::OptionCode::IANA).is_some()
                    || req.opts().get(v6::OptionCode::IATA).is_some()
                    || req.opts().get(v6::OptionCode::IAPD).is_some()
                {
                    return Ok(Action::NoResponse);
                }
                resp.set_msg_type(v6::MessageType::Reply);
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
            //RelayForw => {}
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
    use config::{generate_random_bytes, v6::is_unicast_link_local};
    use util::get_server_id_override;

    use dora_core::dhcproto::{
        v4::{self, relay},
        v6::{duid::Duid, ORO},
    };
    use tracing_test::traced_test;

    use super::*;

    static SAMPLE_YAML: &str = include_str!("../../../libs/config/sample/config.yaml");
    static V6_EXAMPLE_YAML: &str = include_str!("../../../libs/config/sample/config_v6.yaml");

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

    // ensure the server identifier override is written to the response server identifier when they match
    #[tokio::test]
    #[traced_test]
    async fn test_server_id_eq_override() -> Result<()> {
        let cfg = DhcpConfig::parse_str(SAMPLE_YAML).unwrap();
        let plugin = MsgType::new(Arc::new(cfg.clone()))?;
        let mut ctx = util::blank_ctx(
            "192.168.0.1:67".parse()?,
            "192.168.0.1".parse()?,
            "192.168.0.1".parse()?,
            v4::MessageType::Discover,
        )?;

        let mut relay_info = relay::RelayAgentInformation::default();
        relay_info.insert(relay::RelayInfo::ServerIdentifierOverride(
            "10.0.0.1".parse()?,
        ));
        // assign suboption 11 of DHCP relay info (opt 82)
        ctx.msg_mut()
            .opts_mut()
            .insert(v4::DhcpOption::RelayAgentInformation(relay_info));
        // assign the same address to the server identifier
        ctx.msg_mut()
            .opts_mut()
            .insert(DhcpOption::ServerIdentifier("10.0.0.1".parse()?));
        plugin.handle(&mut ctx).await?;

        let resp_server_id = ctx
            .resp_msg()
            .unwrap()
            .opts()
            .get(OptionCode::ServerIdentifier);
        let msg_server_id_override = get_server_id_override(ctx.msg().opts());

        // get and compare the Ipv4Addrs from resp_server_id and resp_server_id_override
        if let (Some(&DhcpOption::ServerIdentifier(addr1)), Some(addr2)) =
            (resp_server_id, msg_server_id_override)
        {
            assert_eq!(addr1, addr2);
        } else {
            panic!("Server identifier and server identifier override are not both Ipv4Addrs:\n\nOpt 54 = {:?}\nOpt 82 Subopt 11 = {:?}\n\n", resp_server_id, msg_server_id_override);
        }
        // ensure we respond with an offer
        assert!(ctx
            .resp_msg()
            .unwrap()
            .opts()
            .has_msg_type(v4::MessageType::Offer));
        Ok(())
    }

    // ensure the server identifier override is not written to the response server identifier when they don't match
    #[tokio::test]
    #[traced_test]
    async fn test_server_id_ne_override() -> Result<()> {
        let cfg = DhcpConfig::parse_str(SAMPLE_YAML).unwrap();
        let plugin = MsgType::new(Arc::new(cfg.clone()))?;
        let mut ctx = util::blank_ctx(
            "192.168.0.1:67".parse()?,
            "192.168.0.1".parse()?,
            "192.168.0.1".parse()?,
            v4::MessageType::Discover,
        )?;

        let mut relay_info = relay::RelayAgentInformation::default();
        relay_info.insert(relay::RelayInfo::ServerIdentifierOverride(
            "10.0.0.2".parse()?,
        ));
        // assign suboption 11 of DHCP relay info (opt 82)
        ctx.msg_mut()
            .opts_mut()
            .insert(v4::DhcpOption::RelayAgentInformation(relay_info));
        // assign an address to the server identifier that does not match the override or our address
        ctx.msg_mut()
            .opts_mut()
            .insert(DhcpOption::ServerIdentifier("10.0.0.10".parse()?));
        let res = plugin.handle(&mut ctx).await?;
        // when the the server id in the message matches neither the server id override nor our server
        // id, we must not respond
        assert_eq!(res, Action::NoResponse);
        Ok(())
    }

    /// for testing
    fn find_interface_with_unicast_link_local(cfg: &DhcpConfig) -> Option<u32> {
        let interfaces = cfg.v6().interfaces();
        //find index of first interface that has a unicast address
        interfaces.iter().find_map(|int| {
            int.ips.iter().find_map(|ip| match ip {
                IpNetwork::V6(ip) => {
                    if is_unicast_link_local(&ip.ip()) {
                        Some(int.index)
                    } else {
                        None
                    }
                }
                _ => None,
            })
        })
    }

    // create a uuid type duid
    fn generate_duid() -> Result<Duid, ()> {
        let bytes = generate_random_bytes(16);
        println!("!!!{:?}",bytes);
        let duid = Duid::uuid(&bytes);
        Ok(duid)
    }
    ///test that we respond to an information request
    #[tokio::test]
    #[traced_test]
    async fn test_information_request() -> Result<()> {
        let cfg = DhcpConfig::parse_str(V6_EXAMPLE_YAML).unwrap();
        //find index of first interface that has a unicast address
        let ifindex = find_interface_with_unicast_link_local(&cfg)
            .context("no interface with unicast link local address")?;
        let plugin = MsgType::new(Arc::new(cfg.clone()))?;
        let mut ctx = util::blank_ctx_v6(
            "[2001:db8::1]:546".parse()?,
            ifindex as u32,
            v6::MessageType::InformationRequest,
        )?;
        //according to https://datatracker.ietf.org/doc/html/rfc8415#section-18.2.6, Information-request Messages might not include  Client Identifier option, so here we ignore it.
        //add elapsed time option
        ctx.msg_mut()
            .opts_mut()
            .insert(v6::DhcpOption::ElapsedTime(60));
        let oro = ORO {
            opts: vec![
                v6::OptionCode::InfMaxRt,
                v6::OptionCode::InformationRefreshTime,
                v6::OptionCode::DomainNameServers,
            ],
        };
        //add option request
        ctx.msg_mut().opts_mut().insert(v6::DhcpOption::ORO(oro));
        let res = plugin.handle(&mut ctx).await?;
        let resp = ctx.resp_msg().unwrap();
        println!("{:?}", resp);
        assert_eq!(res, Action::Respond);
        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_solicit() -> Result<()> {
        let cfg = DhcpConfig::parse_str(V6_EXAMPLE_YAML).unwrap();
        //find index of first interface that has a unicast address
        let ifindex = find_interface_with_unicast_link_local(&cfg)
            .context("no interface with unicast link local address")?;
        let plugin = MsgType::new(Arc::new(cfg.clone()))?;

        let mut ctx = util::blank_ctx_v6(
            "[2001:db8::1]:546".parse()?,
            ifindex as u32,
            v6::MessageType::Solicit,
        )?;
        let client_id = generate_duid().unwrap().as_ref().to_vec();
        ctx.msg_mut()
            .opts_mut()
            .insert(v6::DhcpOption::ClientId(client_id));
        //ctx.msg_mut().opts_mut().insert(v6::DhcpOption::ClientId());
        let res = plugin.handle(&mut ctx).await?;
        println!("{:?}", res);
        //let resp = ctx.resp_msg().unwrap();
        //println!("{:?}", resp);
        //assert_eq!(res, Action::Continue);
        Ok(())
    }
}

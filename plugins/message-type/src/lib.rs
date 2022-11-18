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
    dhcproto::{
        v4::{DhcpOption, Message, MessageType, Opcode, OptionCode},
        v6,
    },
    prelude::*,
    tracing::warn,
};
use register_derive::Register;
use std::net::Ipv4Addr;

use config::DhcpConfig;

#[derive(Debug, Register)]
#[register(msg(Message))]
#[register(msg(v6::Message))]
#[register(plugin())]
pub struct MsgType {
    cfg: Arc<DhcpConfig>,
}

impl MsgType {
    pub fn new(cfg: Arc<DhcpConfig>) -> Result<Self> {
        Ok(Self { cfg })
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
            .get_interface(meta.ifindex)
            .context("interface message was received on does not exist?")?;
        ctx.set_interface(interface);

        let req = ctx.decoded_msg();
        let msg_type = req.opts().msg_type();

        let subnet = ctx.subnet()?;
        debug!(
            msg_type = ?msg_type.context("messages must have a type")?,
            src_addr = %ctx.src_addr(),
            ?subnet,
            req = %ctx.decoded_msg(),
        );
        // otherwise our interface IP as the id
        let server_id = self
            .cfg
            .v4()
            .server_id(meta.ifindex, subnet)
            .context("cannot find server_id")?;

        // message that will be returned
        let mut resp = util::new_msg(req, server_id);

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

        // look up which network the message belongs to
        let network = self.cfg.v4().get_network(subnet);

        match msg_type.context("no option 53 (message type) found")? {
            MessageType::Discover => {
                resp.opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Offer));
            }
            MessageType::Request => {
                if !req.giaddr().is_unspecified() {
                    resp.set_flags(req.flags().set_broadcast());
                }
                resp.opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Ack));
            }
            MessageType::Release => {
                resp.opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Ack));
            }
            // got INFORM & we are authoritative, give a response
            MessageType::Inform if matches!(network, Some(net) if net.authoritative()) => {
                resp.opts_mut()
                    .insert(DhcpOption::MessageType(MessageType::Ack));
                let ciaddr = ctx.decoded_msg().ciaddr();
                let addr = if !ciaddr.is_unspecified() {
                    ciaddr
                } else {
                    subnet
                };
                if let Some(network) = self.cfg.v4().get_network(addr) {
                    if let Some(range) = network.get_range(addr) {
                        ctx.set_decoded_resp_msg(resp);
                        ctx.populate_opts(range.opts());
                        return Ok(Action::Respond);
                    }
                }
                warn!(msg_type = ?MessageType::Inform, "couldn't match appropriate range with INFORM message");
            }
            MessageType::Decline => {
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
            _ => {
                debug!("unsupported message type");
                return Ok(Action::NoResponse);
            }
        }

        ctx.set_decoded_resp_msg(resp);
        Ok(Action::Continue)
    }
}

pub mod util {
    use super::*;

    pub fn new_msg(req: &Message, siaddr: Ipv4Addr) -> Message {
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
            .set_flags(req.flags());
        msg
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

        let req = ctx.decoded_msg();
        let msg_type = req.msg_type();

        debug!(
            ?msg_type,
            %interface,
            global = ?ctx.global(),
            src_addr = %ctx.src_addr(),
            req = %ctx.decoded_msg(),
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
                    ctx.set_decoded_resp_msg(resp);
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

        ctx.set_decoded_resp_msg(resp);
        Ok(Action::Continue)
    }
}

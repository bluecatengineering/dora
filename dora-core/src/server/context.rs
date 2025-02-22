//! context of current server message
use chrono::{DateTime, Utc};
use dhcproto::{Decodable, Decoder, Encodable, v4, v6};
use pnet::ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use tracing::{error, trace};
use unix_udp_sock::RecvMeta;

use std::{
    fmt,
    io::{self, Error, ErrorKind},
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use crate::{
    metrics::{self, RECV_TYPE_COUNT, SENT_TYPE_COUNT, V6_RECV_TYPE_COUNT, V6_SENT_TYPE_COUNT},
    server::{State, msg::SerialMsg, typemap::TypeMap},
};

/// Context is what will be passed to the [handler] traits and mutated by
/// the plugins to enrich with data.
///
/// [handler]: crate::handler
pub struct MsgContext<T> {
    /// underlying byte message and address. msg.addr will always be the address
    /// we received the message from and that we send packets back to.
    msg_buf: SerialMsg,
    /// address received. This is initially set to the address that the
    /// UDP packet, but can be overridden with `set_src_addr`.
    src_addr: SocketAddr,
    /// address response sent to
    dst_addr: Option<SocketAddr>,
    /// time this context was created
    time: DateTime<Utc>,
    /// decoded from msg
    msg: T,
    /// decoded response msg  -- **CAREFUL** do not call `take()` on this before
    /// logging the query (or we won't have the data for logging)
    resp_msg: Option<T>,
    /// a type map for use by plugins to store values
    type_map: TypeMap,
    /// unique id we assign to each `MsgContext`
    id: usize,
    /// reference to `State`
    state: Arc<State>,
    /// whether the `MsgContext` counts towards `state.live_msgs`
    is_live: bool,
    /// metadata about the packet we received
    meta: RecvMeta,
    /// contains ip/mask/broadcast where we received msg from
    interface: Option<IpNetwork>,
    /// global unicast address
    global: Option<IpNetwork>,
}

impl<T: fmt::Debug> fmt::Debug for MsgContext<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MsgContext")
            .field("src_addr", &self.src_addr)
            .field("dst_addr", &self.dst_addr)
            .field("time", &self.time)
            .field("id", &self.id)
            .field("is_live", &self.is_live)
            .field("msg", &self.msg)
            .field("resp_msg", &self.resp_msg)
            .field("interface", &self.interface)
            .finish()
    }
}

impl<T> Drop for MsgContext<T> {
    fn drop(&mut self) {
        if self.is_live {
            self.state.dec_live_msgs();
        }
    }
}

impl<T> MsgContext<T> {
    /// Get the id
    pub fn id(&self) -> usize {
        self.id
    }

    /// Get the `SerialMsg` bytes by shared ref
    pub fn bytes(&self) -> &[u8] {
        self.msg_buf.bytes()
    }
    /// return meta data associated with recv'd packet
    pub fn meta(&self) -> RecvMeta {
        self.meta
    }

    /// Get `Serial` message by shared ref
    pub fn msg_buf(&self) -> &SerialMsg {
        &self.msg_buf
    }

    /// Get `SerialMsg` by mutable ref
    pub fn msg_buf_mut(&mut self) -> &mut SerialMsg {
        &mut self.msg_buf
    }

    /// Set the original buffer/address pair that we received for this
    /// `MsgContext`
    pub fn set_msg_buf(&mut self, msg: SerialMsg) {
        self.msg_buf = msg;
    }

    /// Get the `DateTime` that we first created this `MsgContext`
    ///
    /// [`DateTime`]: chrono::DateTime
    pub fn time(&self) -> DateTime<Utc> {
        self.time
    }

    /// Store a value in the current `MsgContext` based on a type.
    /// This value will be available across any step in the lifecycle of a
    /// request
    ///
    /// If this type already exists, it will be returned
    pub fn set_local<U: Send + Sync + 'static>(&mut self, val: U) -> Option<U> {
        self.type_map.insert(val)
    }

    /// Return a value in the current `MsgContext` based on a type, or `None` if
    /// no such value is present.
    pub fn get_local<U: Send + Sync + 'static>(&self) -> Option<&U> {
        self.type_map.get::<U>()
    }

    /// Return a mutable reference to a value in the current `MsgContext` based
    /// on a type, or `None` if no such value is present.
    pub fn get_mut_local<U: Send + Sync + 'static>(&mut self) -> Option<&mut U> {
        self.type_map.get_mut::<U>()
    }

    /// Return a mutable reference to a value in the current `MsgContext`, or
    /// insert a default value if no such value is present.
    /// This requires that the type implement `Default`.
    pub fn get_mut_local_or_default<U: Send + Sync + Default + 'static>(&mut self) -> &mut U {
        if self.type_map.get::<U>().is_none() {
            self.type_map.insert(U::default());
        }
        self.type_map.get_mut::<U>().unwrap()
    }

    /// Removes an item from the type map, returning it.
    ///
    pub fn remove_local<U: Send + Sync + 'static>(&mut self) -> Option<U> {
        self.type_map.remove::<U>()
    }

    /// Return the source address and port.
    ///
    /// This is initially set to the address that the UDP packet
    /// originates from, but can be overridden with `set_src_addr`.
    ///
    pub fn src_addr(&self) -> SocketAddr {
        self.src_addr
    }

    /// Overrides the `src_addr` with a new address/port.
    ///
    pub fn set_src_addr(&mut self, addr: SocketAddr) {
        self.src_addr = addr;
    }

    /// Return the destination address and port IF it has been set.
    ///
    /// `dst_addr` is determined when a response is sent. In dora,
    /// it will most often be the IP of the DHCP relay (giaddr).
    ///
    pub fn dst_addr(&self) -> Option<SocketAddr> {
        self.dst_addr
    }

    /// Overrides the `dst_addr` with a new address/port.
    ///
    pub fn set_dst_addr(&mut self, addr: SocketAddr) {
        self.dst_addr = Some(addr);
    }

    /// Decrement the `state.live_msgs` counter and mark this as not live
    /// This gets done before passing the `MsgContext` to the postresponse
    /// plugins.
    pub fn mark_as_not_live(&mut self) {
        if self.is_live {
            self.state.dec_live_msgs();
            self.is_live = false;
        }
    }
}

impl<T: Encodable + Decodable> MsgContext<T> {
    /// Create a `MsgContext` with state
    pub fn new(msg_buf: SerialMsg, meta: RecvMeta, state: Arc<State>) -> io::Result<Self> {
        let msg = {
            let mut decoder = Decoder::new(msg_buf.bytes());
            T::decode(&mut decoder).map_err(|op| io::Error::new(io::ErrorKind::InvalidData, op))?
        };

        Ok(Self {
            msg_buf,
            src_addr: meta.addr,
            meta,
            dst_addr: None,
            time: Utc::now(),
            msg,
            type_map: TypeMap::new(),
            resp_msg: None,
            id: state.inc_id(),
            state,
            is_live: true,
            interface: None,
            global: None,
        })
    }

    /// Decode the currently held binary data in `resp_msg` using [`Decoder`] into a message.
    /// A decoded DHCP query.
    ///
    /// [`Decoder`]: dhcproto::decoder::Decoder
    pub fn decode_resp(&mut self, msg: SerialMsg) -> io::Result<()> {
        self.resp_msg = Some({
            let mut decoder = Decoder::new(msg.bytes());
            T::decode(&mut decoder).map_err(|op| io::Error::new(io::ErrorKind::InvalidData, op))?
        });

        Ok(())
    }

    /// Takes the decoded response message, encodes into a `SerialMsg`
    pub fn encode_resp_msg(&mut self) -> io::Result<SerialMsg> {
        let msg = self
            .resp_msg
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "no response message"))?;
        SerialMsg::from_msg(msg, self.msg_buf.addr())
    }

    /// The deserialized contents of `msg`
    pub fn msg(&self) -> &T {
        &self.msg
    }

    /// The mutable deserialized contents of `msg`
    pub fn msg_mut(&mut self) -> &mut T {
        &mut self.msg
    }

    /// The contents of `resp_msg`
    pub fn resp_msg(&self) -> Option<&T> {
        self.resp_msg.as_ref()
    }

    /// sets the resp_msg with a `Message`
    pub fn set_resp_msg(&mut self, msg: T) {
        self.resp_msg = Some(msg);
    }
    /// take response message and replace with None
    pub fn resp_msg_take(&mut self) -> Option<T> {
        self.resp_msg.take()
    }
    /// The mutable deserialized contents of `resp_msg`
    pub fn resp_msg_mut(&mut self) -> Option<&mut T> {
        self.resp_msg.as_mut()
    }
    /// set the interface for the message
    pub fn set_interface<I: Into<IpNetwork>>(&mut self, interface: I) {
        self.interface = Some(interface.into());
    }
    /// set the global unicast address associated with the interface the message was received on
    pub fn set_global<I: Into<IpNetwork>>(&mut self, global: I) {
        self.global = Some(global.into());
    }
}

// v4 specific functions
impl MsgContext<v4::Message> {
    /// get the interface for the message. this should always be set
    pub fn interface(&self) -> Option<Ipv4Network> {
        self.interface.and_then(|int| match int {
            IpNetwork::V4(int) => Some(int),
            _ => None,
        })
    }
    /// determine the response addr based on request. Sets response giaddr
    /// if we are talking to a relay. Injects into ARP cache if response will be
    /// unicast to yiaddr.
    //
    /// From RFC (https://tools.ietf.org/html/rfc2131):
    //
    // 1. If the 'giaddr' field in a DHCP message from a client is non-zero,
    // the server sends any return messages to the 'DHCP server' port on the
    // BOOTP relay agent whose address appears in 'giaddr'.
    //
    // 2. If the 'giaddr' field is zero and the 'ciaddr' field is nonzero,
    // then the server unicasts DHCPOFFER and DHCPACK messages to the address in 'ciaddr'.
    //
    // 3. If 'giaddr' is zero and 'ciaddr' is zero, and the broadcast bit is
    // set, then the server broadcasts DHCPOFFER and DHCPACK messages to
    // 0xffffffff.
    //
    // 4. If the broadcast bit is not set and 'giaddr' is zero and
    // 'ciaddr' is zero, then the server unicasts DHCPOFFER and DHCPACK
    // messages to the client's hardware address and 'yiaddr' address.
    //
    // 5. In all cases, when 'giaddr' is zero, the server broadcasts any NAK
    // messages to 0xffffffff.
    pub fn resp_addr(
        &mut self,
        default_port: bool,
        // device: Option<&str>,
        soc: socket2::SockRef<'_>,
    ) -> SocketAddr {
        let req = self.msg();
        let giaddr = req.giaddr();
        let ciaddr = req.ciaddr();

        let (giaddr_zero, ciaddr_zero, broadcast) = (
            req.giaddr().is_unspecified(),
            req.ciaddr().is_unspecified(),
            req.flags().broadcast(),
        );
        //
        let yiaddr = self.resp_msg().map(|msg| msg.yiaddr());
        // TODO: set siaddr (dnsmasq does this)? ciaddr?

        if !default_port {
            trace!("using non-default port for response");
            // if we are not on the default v4 port, send the response
            // back to the source ip:port as unicast.
            // This is useful for testing
            self.msg_buf().addr()
        } else if !giaddr_zero {
            // relay situation: giaddr nonzero
            // use giaddr
            trace!("responding using giaddr");
            self.resp_msg.as_mut().map(|resp| resp.set_giaddr(giaddr));
            (giaddr, v4::SERVER_PORT).into()
        } else if !ciaddr_zero {
            // giaddr zero, ciaddr nonzero
            trace!("responding using ciaddr");
            // use ciaddr
            (ciaddr, v4::CLIENT_PORT).into()
        } else if !broadcast && matches!(yiaddr, Some(ip) if !ip.is_unspecified()) {
            // broadcast false and yiaddr exists
            // INJECT yiaddr IN ARP CACHE:
            trace!("responding using yiaddr");
            // create the sockaddr_in for `yiaddr`
            let yiaddr = yiaddr.unwrap();
            let htype = self.msg().htype();
            let chaddr = self.msg().chaddr();

            // use a different socket for arp injection?
            if let Err(err) = super::ioctl::arp_set(soc, yiaddr, htype, chaddr) {
                error!(
                    ?err,
                    "failed to inject into ARP cache-- fall back to broadcast"
                );

                (Ipv4Addr::BROADCAST, v4::CLIENT_PORT).into()
            } else {
                (yiaddr, v4::CLIENT_PORT).into()
            }
        } else {
            // broadcast set & giaddr/ciaddr zero
            // OR
            // otherwise just broadcast
            trace!("use broadcast addr");
            (Ipv4Addr::BROADCAST, v4::CLIENT_PORT).into()
        }
    }

    /// records metrics for recvd DHCP message
    pub fn recv_metrics(&self) -> io::Result<()> {
        metrics::DHCPV4_BYTES_RECV.inc_by(self.bytes().len() as u64);
        match self.msg().opts().msg_type() {
            Some(v4::MessageType::Discover) => {
                RECV_TYPE_COUNT.discover.inc();
            }
            Some(v4::MessageType::Request) => {
                RECV_TYPE_COUNT.request.inc();
            }
            Some(v4::MessageType::Decline) => {
                RECV_TYPE_COUNT.decline.inc();
            }
            Some(v4::MessageType::Release) => {
                RECV_TYPE_COUNT.release.inc();
            }
            Some(v4::MessageType::Offer) => {
                RECV_TYPE_COUNT.offer.inc();
            }
            Some(v4::MessageType::Ack) => {
                RECV_TYPE_COUNT.ack.inc();
            }
            Some(v4::MessageType::Nak) => {
                RECV_TYPE_COUNT.nak.inc();
            }
            Some(v4::MessageType::Inform) => {
                RECV_TYPE_COUNT.inform.inc();
            }
            _ => {
                RECV_TYPE_COUNT.unknown.inc();
            }
        }
        Ok(())
    }

    /// records metrics for sent DHCP message
    pub fn sent_metrics(&self, elapsed: Duration) -> io::Result<()> {
        let elapsed = elapsed.as_secs_f64();
        match self
            .resp_msg()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "v4 response not found"))?
            .opts()
            .msg_type()
        {
            Some(v4::MessageType::Discover) => {
                SENT_TYPE_COUNT.discover.inc();
            }
            Some(v4::MessageType::Request) => {
                SENT_TYPE_COUNT.request.inc();
            }
            Some(v4::MessageType::Decline) => {
                SENT_TYPE_COUNT.decline.inc();
            }
            Some(v4::MessageType::Release) => {
                SENT_TYPE_COUNT.release.inc();
            }
            Some(v4::MessageType::Offer) => {
                SENT_TYPE_COUNT.offer.inc();
                metrics::DHCPV4_REPLY_DURATION
                    .with_label_values(&["offer"])
                    .observe(elapsed);
            }
            Some(v4::MessageType::Ack) => {
                SENT_TYPE_COUNT.ack.inc();
                metrics::DHCPV4_REPLY_DURATION
                    .with_label_values(&["ack"])
                    .observe(elapsed);
            }
            Some(v4::MessageType::Nak) => {
                SENT_TYPE_COUNT.nak.inc();
                metrics::DHCPV4_REPLY_DURATION
                    .with_label_values(&["nak"])
                    .observe(elapsed);
            }
            Some(v4::MessageType::Inform) => {
                SENT_TYPE_COUNT.inform.inc();
            }
            _ => {
                metrics::DHCPV4_REPLY_DURATION
                    .with_label_values(&["unknown"])
                    .observe(elapsed);
                SENT_TYPE_COUNT.unknown.inc();
            }
        }
        Ok(())
    }

    /// replace `decoded_resp_msg` with a new message type
    /// should clear/update corresponding fields in the msg.
    /// for example, if switched to Nak, yiaddr/siaddr/ciaddr will be cleared
    pub fn update_resp_msg(&mut self, msg_type: v4::MessageType) -> Option<()> {
        let resp = self.resp_msg_mut()?;
        let server_id = resp.opts().get(v4::OptionCode::ServerIdentifier).cloned();
        let client_id = resp.opts().get(v4::OptionCode::ClientIdentifier).cloned();

        #[allow(clippy::single_match)]
        match msg_type {
            v4::MessageType::Nak => {
                let giaddr = resp.giaddr();
                resp.clear_addrs();
                resp.clear_fname();
                resp.clear_sname();
                resp.set_giaddr(giaddr);
                // remove all opts. in the future, we may need to remove exclusively
                // what was added in the param req list, for now we will just remove all
                // and add back server identifier
                resp.opts_mut().clear();
                // add back the server identifier
                if let Some(server_opt) = server_id {
                    resp.opts_mut().insert(server_opt);
                }
                if let Some(client_id) = client_id {
                    resp.opts_mut().insert(client_id);
                }
            }
            _ => {
                // TODO: others?
            }
        };
        resp.opts_mut()
            .insert(v4::DhcpOption::MessageType(msg_type));
        Some(())
    }
    /// Look in the `decoded_msg` and see if there was a lease time requested
    pub fn requested_lease_time(&self) -> Option<Duration> {
        if let Some(v4::DhcpOption::AddressLeaseTime(t)) =
            self.msg().opts().get(v4::OptionCode::AddressLeaseTime)
        {
            Some(Duration::from_secs(*t as u64))
        } else {
            None
        }
    }
    /// Determine what the requested IP is
    /// If `ciaddr` is not unspecified, return it
    /// else if opts has `RequestedIpAddress`, return it,
    /// otherwise return None, there is no requested IP
    pub fn requested_ip(&self) -> Option<Ipv4Addr> {
        let req = self.msg();
        if !req.ciaddr().is_unspecified() {
            // renew or rebind
            Some(req.ciaddr())
        } else if let Some(v4::DhcpOption::RequestedIpAddress(ip)) =
            // recovering previously used IP
            // this is supposed to be based on matching other client details. Something to
            // add in the future maybe
            req.opts().get(v4::OptionCode::RequestedIpAddress)
        {
            Some(*ip)
        } else {
            None
        }
    }

    /// determine the correct subnet of a DHCP message
    /// <https://www.rfc-editor.org/rfc/rfc3527.html>
    ///
    /// > In the event that a DHCP server receives a packet that contains both
    /// >  a subnet-selection option [RFC 3011], as well as a link-selection
    /// > sub-option, the information contained in the link-selection sub-
    /// > option MUST be used to control the allocation of an IP address in
    /// > preference to the information contained in the subnet-selection
    /// > option.
    ///
    /// # Returns
    /// returns an Err if no link/subnet/giaddr/ciaddr available
    pub fn relay_subnet(&self) -> io::Result<Ipv4Addr> {
        use dhcproto::v4::{
            DhcpOption, OptionCode,
            relay::{RelayCode, RelayInfo},
        };
        // get link-selection relay agent subopt first
        // OR use subnet-selection option
        let link = self
            .msg
            .opts()
            .get(OptionCode::RelayAgentInformation)
            .and_then(|opt| {
                if let DhcpOption::RelayAgentInformation(info) = opt {
                    if let Some(RelayInfo::LinkSelection(ip)) = info.get(RelayCode::LinkSelection) {
                        return Some(ip);
                    }
                }
                None
            })
            .or_else(|| match self.msg.opts().get(OptionCode::SubnetSelection) {
                Some(DhcpOption::SubnetSelection(ip)) => Some(ip),
                _ => None,
            });
        let giaddr = self.msg().giaddr();
        let ciaddr = self.msg().ciaddr();

        if let Some(ip) = link {
            Ok(*ip)
        } else if !giaddr.is_unspecified() {
            Ok(giaddr)
        } else if !ciaddr.is_unspecified() {
            Ok(ciaddr)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "We can't determine which subnet to apply because:
                 - has no link selection relay info
                 - has no subnet selection option
                 - its giaddr is unspecified
                 - ciaddr is unspecified",
            ))
        }
    }

    /// tries to determine the subnet for this MsgContext. calls `relay_subnet` first,
    /// and if there is no relay information, falls back on the IP of the interface
    /// the message was recv'd on
    pub fn subnet(&self) -> io::Result<Ipv4Addr> {
        self.relay_subnet().or_else(|_| {
            self.interface().map(|int| int.ip()).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "no interface set for MsgContext",
                )
            })
        })
    }

    /// looks in `decoded_msg` for `DhcpOption::ParameterRequestList` and provides any options
    /// in `decoded_resp_msg` that match both in `opts` and in the param req list
    ///
    /// Copies over options from request that should be present on response
    /// Also, looks at `interface` and adds subnetmask/broadcast. If provided by `param_opts`
    /// these will be overwritten.
    pub fn populate_opts(&mut self, param_opts: &v4::DhcpOptions) -> Option<()> {
        use dhcproto::v4::{DhcpOption, OptionCode};
        let subnet = self.subnet();
        // https://datatracker.ietf.org/doc/html/rfc3046#section-2.2
        // copy opt 82 (relay agent) into response
        let resp = self.resp_msg.as_mut()?;
        if let Some(info) = self.msg.opts().get(OptionCode::RelayAgentInformation) {
            resp.opts_mut().insert(info.clone());
        }

        // https://datatracker.ietf.org/doc/html/rfc6842#section-3
        // copy client id
        if let Some(id) = self.msg.opts().get(OptionCode::ClientIdentifier) {
            resp.opts_mut().insert(id.clone());
        }
        let mut interface_match = false;
        // insert router/netmask
        // if the config provides these also, they will be overwritten
        if let Some(IpNetwork::V4(interface)) = self.interface {
            // if we populate from interface, interface must be on same subnet as packet (local)
            if matches!(subnet, Ok(subnet) if interface.contains(subnet)) {
                resp.opts_mut()
                    .insert(DhcpOption::Router(vec![interface.ip()]));
                resp.opts_mut()
                    .insert(DhcpOption::SubnetMask(interface.mask()));
                interface_match = true;
            }
            // configured router/netmask will override interface
            if let Some(v) = param_opts.get(OptionCode::Router) {
                resp.opts_mut().insert(v.clone());
            }
            if let Some(v) = param_opts.get(OptionCode::SubnetMask) {
                resp.opts_mut().insert(v.clone());
            }
        }

        if let Some(DhcpOption::ParameterRequestList(requested)) =
            self.msg.opts().get(OptionCode::ParameterRequestList)
        {
            // if broadcast addr is requested, try to fill from interface
            if let Some(IpNetwork::V4(interface)) = self.interface {
                if requested.contains(&v4::OptionCode::BroadcastAddr) && interface_match {
                    resp.opts_mut()
                        .insert(DhcpOption::BroadcastAddr(interface.broadcast()));
                }
            }
            // look in the requested list of params
            for code in requested {
                // if we have that option, add it to the response
                if let Some(v) = param_opts.get(*code) {
                    resp.opts_mut().insert(v.clone());
                }
            }
        }
        Some(())
    }

    /// clears DHCP specific options from the response leaving only BOOTP options as defined in RFC 1533
    pub fn filter_dhcp_opts(&mut self) -> Option<()> {
        const DHCP_OPTS: &[v4::OptionCode] = &[
            v4::OptionCode::RequestedIpAddress,
            v4::OptionCode::AddressLeaseTime,
            v4::OptionCode::OptionOverload,
            v4::OptionCode::MessageType,
            v4::OptionCode::ServerIdentifier,
            v4::OptionCode::ParameterRequestList,
            v4::OptionCode::Message,
            v4::OptionCode::MaxMessageSize,
            v4::OptionCode::Renewal,
            v4::OptionCode::Rebinding,
            v4::OptionCode::ClientIdentifier,
        ];
        let resp = self.resp_msg_mut()?;
        for opt in DHCP_OPTS {
            resp.opts_mut().remove(*opt);
        }
        Some(())
    }
    /// Populate the opts with lease times
    /// looks in `decoded_msg` for `DhcpOption::ParameterRequestList` and provides any options
    /// in `decoded_resp_msg` that match both in `opts` and in the param req list
    pub fn populate_opts_lease(
        &mut self,
        param_opts: &v4::DhcpOptions,
        lease: Duration,
        renew: Duration,
        rebind: Duration,
    ) -> Option<()> {
        self.populate_opts(param_opts)?; // add time
        let resp = self.resp_msg.as_mut()?;
        resp.opts_mut()
            .insert(v4::DhcpOption::AddressLeaseTime(whole_seconds(lease)));
        resp.opts_mut()
            .insert(v4::DhcpOption::Renewal(whole_seconds(renew)));
        resp.opts_mut()
            .insert(v4::DhcpOption::Rebinding(whole_seconds(rebind)));
        Some(())
    }
}

fn whole_seconds(t: Duration) -> u32 {
    if t.subsec_millis() >= 500 {
        t.as_secs() as u32 + 1
    } else {
        t.as_secs() as u32
    }
}

impl MsgContext<v6::Message> {
    /// get the global unicast addr associated with the received interface
    pub fn global(&self) -> Option<Ipv6Network> {
        self.global.and_then(|int| match int {
            IpNetwork::V6(int) => Some(int),
            _ => None,
        })
    }
    /// get the interface for the message. this should always be set
    pub fn interface(&self) -> Option<Ipv6Network> {
        self.interface.and_then(|int| match int {
            IpNetwork::V6(int) => Some(int),
            _ => None,
        })
    }

    /// get the response address to send the message to
    pub fn resp_addr(
        &mut self,
        default_port: bool,
        // soc: socket2::SockRef<'_>,
    ) -> SocketAddr {
        if !default_port {
            trace!("using non-default port for response");
            self.msg_buf().addr()
        } else {
            let mut src = self.src_addr();
            src.set_port(v6::CLIENT_PORT);
            src
        }
    }

    /// Looks in `decoded_msg` for `DhcpOption::ORO` and provides any options
    /// in `decoded_resp_msg` that are in `param_opts`.
    /// include the client identifier *if it was present* in the original message
    pub fn populate_opts(&mut self, param_opts: &v6::DhcpOptions) -> Option<()> {
        use dhcproto::v6::{DhcpOption, OptionCode};
        let resp = self.resp_msg.as_mut()?;

        // copy client id https://www.rfc-editor.org/rfc/rfc8415.html#section-18.3.9
        if let Some(id) = self.msg.opts().get(OptionCode::ClientId) {
            resp.opts_mut().insert(id.clone());
        }

        if let Some(DhcpOption::ORO(requested)) = self.msg.opts().get(OptionCode::ORO) {
            trace!(?requested, provided = ?param_opts, "requested opts");
            // look in the requested list of params
            for code in &requested.opts {
                // if we have that option, add it to the response
                if let Some(v) = param_opts.get(*code) {
                    resp.opts_mut().insert(v.clone());
                }
            }
        }
        Some(())
    }

    /// records metrics for recvd DHCP message
    pub fn recv_metrics(&self) -> io::Result<()> {
        metrics::DHCPV6_BYTES_RECV.inc_by(self.bytes().len() as u64);
        match self.msg().msg_type() {
            v6::MessageType::Solicit => V6_RECV_TYPE_COUNT.solicit.inc(),
            v6::MessageType::Advertise => V6_RECV_TYPE_COUNT.advertise.inc(),
            v6::MessageType::Request => V6_RECV_TYPE_COUNT.request.inc(),
            v6::MessageType::Confirm => V6_RECV_TYPE_COUNT.confirm.inc(),
            v6::MessageType::Renew => V6_RECV_TYPE_COUNT.renew.inc(),
            v6::MessageType::Rebind => V6_RECV_TYPE_COUNT.rebind.inc(),
            v6::MessageType::Reply => V6_RECV_TYPE_COUNT.reply.inc(),
            v6::MessageType::Release => V6_RECV_TYPE_COUNT.release.inc(),
            v6::MessageType::Decline => V6_RECV_TYPE_COUNT.decline.inc(),
            v6::MessageType::Reconfigure => V6_RECV_TYPE_COUNT.reconf.inc(),
            v6::MessageType::InformationRequest => V6_RECV_TYPE_COUNT.inforeq.inc(),
            v6::MessageType::RelayForw => V6_RECV_TYPE_COUNT.relayforw.inc(),
            v6::MessageType::RelayRepl => V6_RECV_TYPE_COUNT.relayrepl.inc(),
            _ => {
                V6_RECV_TYPE_COUNT.unknown.inc();
            }
        }
        Ok(())
    }

    /// records metrics for sent DHCP message
    pub fn sent_metrics(&self, elapsed: Duration) -> io::Result<()> {
        let elapsed = elapsed.as_secs_f64();
        match self
            .resp_msg()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "v6 response not found"))?
            .msg_type()
        {
            v6::MessageType::Solicit => V6_SENT_TYPE_COUNT.solicit.inc(),
            v6::MessageType::Advertise => {
                V6_SENT_TYPE_COUNT.advertise.inc();
                metrics::DHCPV6_REPLY_DURATION
                    .with_label_values(&["advertise"])
                    .observe(elapsed);
            }
            v6::MessageType::Request => V6_SENT_TYPE_COUNT.request.inc(),
            v6::MessageType::Confirm => {
                V6_SENT_TYPE_COUNT.confirm.inc();
                metrics::DHCPV6_REPLY_DURATION
                    .with_label_values(&["confirm"])
                    .observe(elapsed);
            }
            v6::MessageType::Renew => V6_SENT_TYPE_COUNT.renew.inc(),
            v6::MessageType::Rebind => V6_SENT_TYPE_COUNT.rebind.inc(),
            v6::MessageType::Reply => {
                V6_SENT_TYPE_COUNT.reply.inc();
                metrics::DHCPV6_REPLY_DURATION
                    .with_label_values(&["reply"])
                    .observe(elapsed);
            }
            v6::MessageType::Release => V6_SENT_TYPE_COUNT.release.inc(),
            v6::MessageType::Decline => V6_SENT_TYPE_COUNT.decline.inc(),
            v6::MessageType::Reconfigure => V6_SENT_TYPE_COUNT.reconf.inc(),
            v6::MessageType::InformationRequest => {
                V6_SENT_TYPE_COUNT.inforeq.inc();
                metrics::DHCPV6_REPLY_DURATION
                    .with_label_values(&["inforeq"])
                    .observe(elapsed);
            }
            v6::MessageType::RelayForw => V6_SENT_TYPE_COUNT.relayforw.inc(),
            v6::MessageType::RelayRepl => V6_SENT_TYPE_COUNT.relayrepl.inc(),
            _ => {
                V6_SENT_TYPE_COUNT.unknown.inc();
                metrics::DHCPV6_REPLY_DURATION
                    .with_label_values(&["unknown"])
                    .observe(elapsed);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    fn blank_msg() -> anyhow::Result<(v4::Message, SocketAddr, Arc<State>)> {
        let msg = v4::Message::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            &[0, 1, 2, 3, 4, 5],
        );
        let state = Arc::new(State::new(10));
        let addr = "1.2.3.4:67".parse()?;
        Ok((msg, addr, state))
    }

    fn assert_opt(ctx: &MsgContext<v4::Message>, opt: v4::DhcpOption) {
        assert_eq!(
            &opt,
            ctx.resp_msg()
                .unwrap()
                .opts()
                .get(v4::OptionCode::from(&opt))
                .unwrap()
        );
    }

    #[test]
    fn test_subnet_giaddr() -> anyhow::Result<()> {
        let (mut msg, addr, state) = blank_msg()?;
        msg.set_giaddr([1, 2, 3, 4]);

        let meta = RecvMeta {
            addr,
            ..RecvMeta::default()
        };
        let ctx = MsgContext::<v4::Message>::new(
            SerialMsg::new(Bytes::from(msg.to_vec()?), addr),
            meta,
            state,
        )?;
        assert_eq!(ctx.relay_subnet()?, Ipv4Addr::new(1, 2, 3, 4));
        Ok(())
    }

    #[test]
    fn test_subnet_subnet_selection() -> anyhow::Result<()> {
        let (mut msg, addr, state) = blank_msg()?;
        msg.opts_mut()
            .insert(v4::DhcpOption::SubnetSelection([1, 2, 3, 4].into()));
        let meta = RecvMeta {
            addr,
            ..RecvMeta::default()
        };
        let ctx = MsgContext::<v4::Message>::new(
            SerialMsg::new(Bytes::from(msg.to_vec()?), addr),
            meta,
            state,
        )?;
        assert_eq!(ctx.relay_subnet()?, Ipv4Addr::new(1, 2, 3, 4));
        Ok(())
    }

    #[test]
    fn test_subnet_relay_link_selection() -> anyhow::Result<()> {
        use v4::relay::{RelayAgentInformation, RelayInfo};
        let (mut msg, addr, state) = blank_msg()?;
        let mut info = RelayAgentInformation::default();

        info.insert(RelayInfo::LinkSelection([1, 2, 3, 4].into()));
        msg.opts_mut()
            .insert(v4::DhcpOption::RelayAgentInformation(info));
        let meta = RecvMeta {
            addr,
            ..RecvMeta::default()
        };
        let ctx = MsgContext::<v4::Message>::new(
            SerialMsg::new(Bytes::from(msg.to_vec()?), addr),
            meta,
            state,
        )?;
        assert_eq!(ctx.relay_subnet()?, Ipv4Addr::new(1, 2, 3, 4));
        Ok(())
    }

    #[test]
    fn test_giaddr_unspecified() -> anyhow::Result<()> {
        let (msg, addr, state) = blank_msg()?;
        let meta = RecvMeta {
            addr,
            ..RecvMeta::default()
        };
        let ctx = MsgContext::<v4::Message>::new(
            SerialMsg::new(Bytes::from(msg.to_vec()?), addr),
            meta,
            state,
        )?;
        assert!(ctx.relay_subnet().is_err());
        Ok(())
    }

    // tests that the parameters in `decoded_msg` get fulfilled with a
    // given `opts` and placed in `decoded_resp_msg`
    #[test]
    fn test_param_req_list() -> anyhow::Result<()> {
        let (mut msg, addr, state) = blank_msg()?;
        // opt codes we are requesting
        msg.opts_mut()
            .insert(v4::DhcpOption::ParameterRequestList(vec![
                v4::OptionCode::Router,
            ]));
        // opts used to serve requests
        let mut opts = v4::DhcpOptions::default();
        opts.insert(v4::DhcpOption::Router(vec![[1, 2, 3, 4].into()]));
        opts.insert(v4::DhcpOption::DomainNameServer(vec![[1, 2, 3, 4].into()]));
        let meta = RecvMeta {
            addr,
            ..RecvMeta::default()
        };
        let mut ctx = MsgContext::<v4::Message>::new(
            SerialMsg::new(Bytes::from(msg.to_vec()?), addr),
            meta,
            state,
        )?;
        ctx.resp_msg = Some(v4::Message::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            &[1, 2, 3, 4, 5, 6],
        ));
        // parse param req list, supplying opts
        ctx.populate_opts_lease(
            &opts,
            Duration::from_secs(3600),
            Duration::from_secs(3600 / 2),
            Duration::from_secs(3600 - (3600 * 7 / 8)),
        );
        // expect Router to be avail in ctx
        assert_opt(&ctx, v4::DhcpOption::Router(vec![[1, 2, 3, 4].into()]));
        assert_opt(&ctx, v4::DhcpOption::AddressLeaseTime(3600));
        assert_opt(&ctx, v4::DhcpOption::Renewal(3600 / 2));
        assert_opt(&ctx, v4::DhcpOption::Rebinding(3600 - (3600 * 7 / 8)));

        Ok(())
    }

    #[test]
    fn test_relay_agent_resp() -> anyhow::Result<()> {
        let (mut msg, addr, state) = blank_msg()?;

        let mut rinfo = v4::relay::RelayAgentInformation::default();
        rinfo.insert(v4::relay::RelayInfo::LinkSelection([4, 5, 6, 7].into()));
        let backup = rinfo.clone();
        // add relay agent info to received msg
        msg.opts_mut()
            .insert(v4::DhcpOption::RelayAgentInformation(rinfo));
        let meta = RecvMeta {
            addr,
            ..RecvMeta::default()
        };
        let mut ctx = MsgContext::<v4::Message>::new(
            SerialMsg::new(Bytes::from(msg.to_vec()?), addr),
            meta,
            state,
        )?;
        ctx.resp_msg = Some(v4::Message::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            &[1, 2, 3, 4, 5, 6],
        ));
        // garbage opts to satisfy fn
        let mut opts = v4::DhcpOptions::default();
        opts.insert(v4::DhcpOption::Router(vec![[1, 2, 3, 4].into()]));
        opts.insert(v4::DhcpOption::DomainNameServer(vec![[1, 2, 3, 4].into()]));
        // parse param req list, supplying opts
        ctx.populate_opts(&opts);

        // expect relay agent to be in resp
        assert_opt(&ctx, v4::DhcpOption::RelayAgentInformation(backup));
        Ok(())
    }

    #[test]
    fn test_take() -> anyhow::Result<()> {
        let (mut msg, addr, state) = blank_msg()?;
        // opt codes we are requesting
        msg.opts_mut()
            .insert(v4::DhcpOption::ParameterRequestList(vec![
                v4::OptionCode::Router,
            ]));
        // opts used to serve requests
        let mut opts = v4::DhcpOptions::default();
        opts.insert(v4::DhcpOption::Router(vec![[1, 2, 3, 4].into()]));
        opts.insert(v4::DhcpOption::DomainNameServer(vec![[1, 2, 3, 4].into()]));
        let meta = RecvMeta {
            addr,
            ..RecvMeta::default()
        };
        let mut ctx = MsgContext::<v4::Message>::new(
            SerialMsg::new(Bytes::from(msg.to_vec()?), addr),
            meta,
            state,
        )?;
        ctx.resp_msg = Some(v4::Message::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            &[1, 2, 3, 4, 5, 6],
        ));

        ctx.resp_msg_take();
        assert_eq!(ctx.resp_msg, None);
        Ok(())
    }
}

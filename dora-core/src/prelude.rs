//! dora prelude

pub use crate::{
    anyhow::{self, Context, Result},
    async_trait, dhcproto,
    handler::{Action, Plugin},
    pnet::datalink::{MacAddr, NetworkInterface},
    pnet::ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network},
    server::{context::MsgContext, state::State},
    tokio,
    tracing::{self, debug, error, info, instrument, trace},
    unix_udp_sock,
};

pub use std::{io, sync::Arc};

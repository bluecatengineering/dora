#![allow(missing_docs)] // proc macros dont play nicely with docstrings

//! # metrics
//!
//! contains statistics for server metrics
use std::time::Instant;

use lazy_static::lazy_static;
use prometheus::{register_int_counter_vec, register_int_gauge, IntCounterVec, IntGauge};
use prometheus_static_metric::make_static_metric;

make_static_metric! {
    pub label_enum MsgType {
        discover,
        request,
        decline,
        release,
        offer,
        ack,
        nak,
        inform,
        unknown,
    }
    pub struct RecvStats: IntCounter {
        "message_type" => MsgType
    }
    pub struct SentStats: IntCounter {
        "message_type" => MsgType
    }
    pub label_enum V6MsgType {
        solicit,
        advertise,
        request,
        confirm,
        renew,
        rebind,
        reply,
        release,
        decline,
        reconf,
        inforeq,
        relayforw,
        relayrepl,
        unknown,
    }
    pub struct V6RecvStats: IntCounter {
        "v6_message_type" => V6MsgType
    }
    pub struct V6SentStats: IntCounter {
        "v6_message_type" => V6MsgType
    }
}

lazy_static! {
    /// When the server started
    pub static ref START_TIME: Instant = Instant::now();

    pub static ref RECV_COUNT_VEC: IntCounterVec = register_int_counter_vec!(
        "recv_type_counts",
        "Recv Type Counts",
        &["message_type"]
    )
    .unwrap();
    pub static ref SENT_COUNT_VEC: IntCounterVec = register_int_counter_vec!(
        "sent_type_counts",
        "Sent Type Counts",
        &["message_type"]
    )
    .unwrap();

    /// aggregate count of all recv'd messages types
    pub static ref RECV_TYPE_COUNT: RecvStats = RecvStats::from(&RECV_COUNT_VEC);

    /// aggregate count of all sent messages types
    pub static ref SENT_TYPE_COUNT: SentStats = SentStats::from(&SENT_COUNT_VEC);

    pub static ref V6_RECV_COUNT_VEC: IntCounterVec = register_int_counter_vec!(
        "v6_recv_type_counts",
        "V6 Recv Type Counts",
        &["v6_message_type"]
    )
    .unwrap();
    pub static ref V6_SENT_COUNT_VEC: IntCounterVec = register_int_counter_vec!(
        "v6_sent_type_counts",
        "V6 Sent Type Counts",
        &["v6_message_type"]
    )
    .unwrap();

    /// aggregate count of all recv'd messages types
    pub static ref V6_RECV_TYPE_COUNT: V6RecvStats = V6RecvStats::from(&V6_RECV_COUNT_VEC);

    /// aggregate count of all sent messages types
    pub static ref V6_SENT_TYPE_COUNT: V6SentStats = V6SentStats::from(&V6_SENT_COUNT_VEC);

    /// # of in flight msgs
    pub static ref IN_FLIGHT: IntGauge =
        register_int_gauge!("in_flight", "count of currently processing messages").unwrap();

    // TODO: set in external-api
    /// # of declined IPs
    // pub static ref DECLINED_ADDRS: IntGauge =
        // register_int_gauge!("declined_addrs", "count of addresses currently on probation from decline").unwrap();

    // TODO: set in external-api
    /// # of leased IPs
    // pub static ref LEASED_ADDRS: IntGauge =
    //     register_int_gauge!("leased_addrs", "count of addresses currently leased").unwrap();

    /// # of total addrs available
    pub static ref TOTAL_AVAILABLE_ADDRS: IntGauge =
        register_int_gauge!("total_available_addrs", "count of addresses currently leased").unwrap();

    /// server uptime
    pub static ref UPTIME: IntGauge = register_int_gauge!("uptime", "server uptime (seconds)").unwrap();
}

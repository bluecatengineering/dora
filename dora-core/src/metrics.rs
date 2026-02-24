#![allow(missing_docs)] // proc macros dont play nicely with docstrings

//! # metrics
//!
//! contains statistics for server metrics
use std::time::Instant;

use lazy_static::lazy_static;
use prometheus::{
    register_int_counter, register_int_counter_vec, register_int_gauge, HistogramOpts,
    HistogramVec, IntCounter, IntCounterVec, IntGauge,
};
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

    /// bytes sent DHCPv4
    pub static ref DHCPV4_BYTES_SENT: IntCounter = register_int_counter!("dhcpv4_bytes_sent", "DHCPv4 bytes sent").unwrap();
    /// bytes sent DHCPv6
    pub static ref DHCPV6_BYTES_SENT: IntCounter = register_int_counter!("dhcpv6_bytes_sent", "DHCPv4 bytes sent").unwrap();

    /// bytes recv DHCPv4
    pub static ref DHCPV4_BYTES_RECV: IntCounter = register_int_counter!("dhcpv4_bytes_recv", "DHCPv4 bytes recv").unwrap();
    /// bytes recv DHCPv6
    pub static ref DHCPV6_BYTES_RECV: IntCounter = register_int_counter!("dhcpv6_bytes_recv", "DHCPv6 bytes recv").unwrap();

    /// histogram of response times for DHCPv4 reply
    pub static ref DHCPV4_REPLY_DURATION: HistogramVec = HistogramVec::new(
        HistogramOpts::new("dhpcv4_duration", "dhcpv4 duration (seconds)"),
        &["type"]
    )
    .unwrap();

    /// histogram of response times for DHCPv6 reply
    pub static ref DHCPV6_REPLY_DURATION: HistogramVec = HistogramVec::new(
        HistogramOpts::new("dhcpv6_duration", "dhcpv6 duration (seconds)"),
        &["type"]
    )
    .unwrap();

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

    // ICMP metrics

    /// ping request count
    pub static ref ICMPV4_REQUEST_COUNT: IntCounter = register_int_counter!("icmpv4_request_count", "count of ICMPv4 echo request").unwrap();
    /// ping reply count
    pub static ref ICMPV4_REPLY_COUNT: IntCounter = register_int_counter!("icmpv4_reply_count", "count of ICMPv4 echo reply").unwrap();


    /// ping request count
    pub static ref ICMPV6_REQUEST_COUNT: IntCounter = register_int_counter!("icmpv6_request_count", "count of ICMPv6 echo request").unwrap();
    /// ping reply count
    pub static ref ICMPV6_REPLY_COUNT: IntCounter = register_int_counter!("icmpv6_reply_count", "count of ICMPv6 echo reply").unwrap();


    /// histogram of response times for ping reply
    pub static ref ICMPV4_REPLY_DURATION: HistogramVec = HistogramVec::new(
        HistogramOpts::new("icmpv4_duration", "icmpv4 response time in seconds, only counts received pings"),
        &["reply"]
    )
    .unwrap();

  /// histogram of response times for ping reply v6
    pub static ref ICMPV6_REPLY_DURATION: HistogramVec = HistogramVec::new(
        HistogramOpts::new("icmpv6_duration", "icmpv6 response time in seconds, only counts received pings"),
        &["reply"]
    )
    .unwrap();

    // client protection metrics

    /// renew cached hit
    pub static ref RENEW_CACHE_HIT: IntCounter = register_int_counter!("renew_cache_hit_count", "count of renew cache hits inside of renewal time").unwrap();
    /// flood threshold reached
    pub static ref FLOOD_THRESHOLD_COUNT: IntCounter = register_int_counter!("flood_threshold_count", "count of times flood threshold has been reached").unwrap();

    // --- Clustered DHCP coordination metrics ---

    /// Count of new allocations blocked due to NATS unavailability (degraded mode)
    pub static ref CLUSTER_ALLOCATIONS_BLOCKED: IntCounter = register_int_counter!("cluster_allocations_blocked", "count of new allocations blocked during NATS unavailability").unwrap();

    /// Count of renewals allowed in degraded mode (known active leases)
    pub static ref CLUSTER_DEGRADED_RENEWALS: IntCounter = register_int_counter!("cluster_degraded_renewals", "count of renewals granted in degraded mode for known active leases").unwrap();

    /// Count of lease coordination conflicts detected across allocators
    pub static ref CLUSTER_CONFLICTS_DETECTED: IntCounter = register_int_counter!("cluster_conflicts_detected", "count of lease coordination conflicts detected").unwrap();

    /// Count of lease coordination conflicts resolved by retry
    pub static ref CLUSTER_CONFLICTS_RESOLVED: IntCounter = register_int_counter!("cluster_conflicts_resolved", "count of lease coordination conflicts resolved").unwrap();

    /// Count of reconciliation events completed after NATS recovery
    pub static ref CLUSTER_RECONCILIATIONS: IntCounter = register_int_counter!("cluster_reconciliations", "count of post-outage reconciliation events completed").unwrap();

    /// Count of lease records reconciled during post-outage recovery
    pub static ref CLUSTER_RECORDS_RECONCILED: IntCounter = register_int_counter!("cluster_records_reconciled", "count of lease records reconciled during post-outage recovery").unwrap();

    /// Gauge: current coordination state (1=connected, 0=disconnected)
    pub static ref CLUSTER_COORDINATION_STATE: IntGauge = register_int_gauge!("cluster_coordination_state", "current coordination state (1=connected, 0=disconnected/degraded)").unwrap();

    // --- Host-option lookup metrics ---

    /// Count of host-option lookup hits
    pub static ref HOST_OPTION_LOOKUP_HIT: IntCounter = register_int_counter!("host_option_lookup_hit", "count of host-option lookup hits").unwrap();

    /// Count of host-option lookup misses
    pub static ref HOST_OPTION_LOOKUP_MISS: IntCounter = register_int_counter!("host_option_lookup_miss", "count of host-option lookup misses").unwrap();

    /// Count of host-option lookup errors (including timeouts)
    pub static ref HOST_OPTION_LOOKUP_ERROR: IntCounter = register_int_counter!("host_option_lookup_error", "count of host-option lookup errors/timeouts").unwrap();
}

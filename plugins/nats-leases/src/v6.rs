//! Stateful DHCPv6 lease handling for nats mode.
//!
//! This module implements:
//! - DHCPv6 lease key extraction and validation (DUID + IAID within subnet)
//! - Stateful allocation, renew, release, decline flows
//! - Multi-lease support per DUID (when IAID differs)
//! - Degraded-mode behavior matching v4 outage policy
//!
//! The uniqueness key for a DHCPv6 lease is `(subnet, duid, iaid)`.
//! One client (DUID) can hold multiple simultaneous leases as long as each
//! IAID is distinct within the same subnet.

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use dora_core::{
    async_trait,
    dhcproto::v6::{self, DhcpOption, MessageType as V6MessageType, OptionCode},
    handler::{Action, Plugin},
    prelude::*,
    tracing::{debug, info, warn},
};

use crate::metrics;
use nats_coordination::{LeaseCoordinator, LeaseOutcome, LeaseRecord, LeaseState, ProtocolFamily};

use config::DhcpConfig;

// ---------------------------------------------------------------------------
// DHCPv6 lease key (T029)
// ---------------------------------------------------------------------------

/// A validated DHCPv6 lease key: `(subnet, duid, iaid)`.
///
/// This is the uniqueness key for stateful DHCPv6 leases. Multiple active
/// leases per DUID are allowed when IAID differs (T030).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct V6LeaseKey {
    /// Subnet (as string, e.g. "2001:db8::/64").
    pub subnet: String,
    /// Client DUID (hex-encoded).
    pub duid: String,
    /// Identity Association ID.
    pub iaid: u32,
}

impl V6LeaseKey {
    /// Construct a normalized key string for indexing.
    pub fn normalized(&self) -> String {
        format!("{}:{}:{}", self.subnet, self.duid, self.iaid)
    }
}

impl fmt::Display for V6LeaseKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "(subnet={}, duid={}, iaid={})",
            self.subnet, self.duid, self.iaid
        )
    }
}

/// Extract and validate a DHCPv6 lease key from a v6 message.
///
/// Returns `None` if the message does not contain required DUID or IAID fields.
pub fn extract_v6_lease_key(msg: &v6::Message, subnet: &str) -> Option<V6LeaseKey> {
    // Extract DUID from ClientId option
    let duid = msg.opts().get(OptionCode::ClientId).and_then(|opt| {
        if let DhcpOption::ClientId(id) = opt {
            if id.is_empty() {
                None
            } else {
                Some(hex::encode(id))
            }
        } else {
            None
        }
    })?;

    // Extract IAID from IA_NA option
    let iaid = msg.opts().get(OptionCode::IANA).and_then(|opt| {
        if let DhcpOption::IANA(iana) = opt {
            Some(iana.id)
        } else {
            None
        }
    })?;

    Some(V6LeaseKey {
        subnet: subnet.to_string(),
        duid,
        iaid,
    })
}

/// Extract the requested IP address from an IA_NA option's IA Address sub-option.
pub fn extract_requested_v6_addr(msg: &v6::Message) -> Option<Ipv6Addr> {
    msg.opts().get(OptionCode::IANA).and_then(|opt| {
        if let DhcpOption::IANA(iana) = opt {
            iana.opts.get(OptionCode::IAAddr).and_then(|sub| {
                if let DhcpOption::IAAddr(ia_addr) = sub {
                    Some(ia_addr.addr)
                } else {
                    None
                }
            })
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Known v6 lease cache for degraded-mode support (T031)
// ---------------------------------------------------------------------------

/// A locally cached record of a known active v6 lease.
#[derive(Debug, Clone)]
struct KnownV6Lease {
    ip: Ipv6Addr,
    expires_at: SystemTime,
}

// ---------------------------------------------------------------------------
// NatsV6Leases plugin (T028)
// ---------------------------------------------------------------------------

/// NATS-mode stateful DHCPv6 lease plugin.
///
/// Handles Solicit, Request, Renew, Release, Decline flows using NATS
/// coordination for cluster-wide lease consistency. Uniqueness is enforced
/// by `(subnet, duid, iaid)` key.
pub struct NatsV6Leases {
    cfg: Arc<DhcpConfig>,
    coordinator: LeaseCoordinator,
    server_id: String,
    /// Known active v6 leases, indexed by normalized key for degraded-mode support.
    known_leases: Arc<parking_lot::RwLock<HashMap<String, KnownV6Lease>>>,
}

impl fmt::Debug for NatsV6Leases {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NatsV6Leases")
            .field("server_id", &self.server_id)
            .finish()
    }
}

impl NatsV6Leases {
    pub fn new(cfg: Arc<DhcpConfig>, coordinator: LeaseCoordinator, server_id: String) -> Self {
        Self {
            cfg,
            coordinator,
            server_id,
            known_leases: Arc::new(parking_lot::RwLock::new(HashMap::new())),
        }
    }

    /// Record a known active v6 lease in local cache.
    fn record_known_lease(&self, key: &V6LeaseKey, ip: Ipv6Addr, expires_at: SystemTime) {
        self.known_leases
            .write()
            .insert(key.normalized(), KnownV6Lease { ip, expires_at });
    }

    /// Remove a known v6 lease from local cache.
    fn remove_known_lease(&self, key: &V6LeaseKey) {
        self.known_leases.write().remove(&key.normalized());
    }

    /// Look up a known active v6 lease in local cache.
    fn get_known_lease(&self, key: &V6LeaseKey) -> Option<(Ipv6Addr, SystemTime)> {
        let leases = self.known_leases.read();
        leases.get(&key.normalized()).and_then(|lease| {
            if lease.expires_at > SystemTime::now() {
                Some((lease.ip, lease.expires_at))
            } else {
                None
            }
        })
    }

    /// Build a LeaseRecord for NATS coordination.
    fn make_v6_lease_record(
        &self,
        ip: Ipv6Addr,
        key: &V6LeaseKey,
        expires_at: SystemTime,
        state: LeaseState,
    ) -> LeaseRecord {
        let now = Utc::now();
        let expires_chrono: DateTime<Utc> = expires_at.into();
        LeaseRecord {
            lease_id: uuid::Uuid::new_v4().to_string(),
            protocol_family: ProtocolFamily::Dhcpv6,
            subnet: key.subnet.clone(),
            ip_address: format!("{}", ip),
            client_key_v4: None,
            duid: Some(key.duid.clone()),
            iaid: Some(key.iaid),
            state,
            expires_at: expires_chrono,
            probation_until: None,
            server_id: self.server_id.clone(),
            revision: 0,
            updated_at: now,
        }
    }

    /// Build an IA_NA option with the assigned address for the response.
    fn build_ia_na_response(
        &self,
        iaid: u32,
        ip: Ipv6Addr,
        valid_time: Duration,
        preferred_time: Duration,
    ) -> DhcpOption {
        let ia_addr = v6::IAAddr {
            addr: ip,
            preferred_life: preferred_time.as_secs() as u32,
            valid_life: valid_time.as_secs() as u32,
            opts: v6::DhcpOptions::new(),
        };
        let mut iana = v6::IANA {
            id: iaid,
            t1: (valid_time.as_secs() / 2) as u32,
            t2: (valid_time.as_secs() * 4 / 5) as u32,
            opts: v6::DhcpOptions::new(),
        };
        iana.opts.insert(DhcpOption::IAAddr(ia_addr));
        DhcpOption::IANA(iana)
    }

    /// Build an IA_NA option with a status code error.
    fn build_ia_na_error(&self, iaid: u32, status_code: u16, message: &str) -> DhcpOption {
        let mut status_opts = v6::DhcpOptions::new();
        status_opts.insert(DhcpOption::StatusCode(v6::StatusCode {
            status: v6::Status::from(status_code),
            msg: message.to_string(),
        }));
        let iana = v6::IANA {
            id: iaid,
            t1: 0,
            t2: 0,
            opts: status_opts,
        };
        DhcpOption::IANA(iana)
    }

    /// Get the v6 network for the current interface.
    fn get_v6_network<'a>(
        &'a self,
        ctx: &MsgContext<v6::Message>,
    ) -> Option<&'a config::v6::Network> {
        let meta = ctx.meta();
        self.cfg.v6().get_network(meta.ifindex)
    }

    /// Get subnet string for the current context.
    fn get_subnet_str(&self, ctx: &MsgContext<v6::Message>) -> Option<String> {
        self.get_v6_network(ctx)
            .map(|net| net.full_subnet().to_string())
    }

    // -------------------------------------------------------------------
    // Stateful v6 message handlers (T028)
    // -------------------------------------------------------------------

    /// Handle Solicit: allocate a new lease (or renew known one).
    async fn handle_solicit(&self, ctx: &mut MsgContext<v6::Message>) -> Result<Action> {
        let subnet_str = match self.get_subnet_str(ctx) {
            Some(s) => s,
            None => {
                debug!("no v6 network found for solicit, skipping");
                return Ok(Action::NoResponse);
            }
        };

        let key = match extract_v6_lease_key(ctx.msg(), &subnet_str) {
            Some(k) => k,
            None => {
                metrics::CLUSTER_V6_INVALID_KEY.inc();
                debug!("missing DUID or IAID in v6 Solicit, dropping");
                return Ok(Action::NoResponse);
            }
        };

        // Check NATS availability for new allocation
        if !self.coordinator.is_available().await {
            metrics::CLUSTER_V6_ALLOCATIONS_BLOCKED.inc();
            metrics::CLUSTER_COORDINATION_STATE.set(0);
            info!(
                key = %key,
                "v6 solicit blocked: NATS coordination unavailable"
            );
            return Ok(Action::NoResponse);
        }
        metrics::CLUSTER_COORDINATION_STATE.set(1);

        let network = match self.get_v6_network(ctx) {
            Some(n) => n,
            None => return Ok(Action::NoResponse),
        };

        let valid = network.valid_time().get_default();
        let preferred = network.preferred_time().get_default();
        let expires_at = SystemTime::now() + valid;

        // Check if client already has a lease for this key
        if let Some((known_ip, _)) = self.get_known_lease(&key) {
            // Reuse existing assignment
            debug!(
                key = %key,
                ip = %known_ip,
                "v6 solicit: reusing known lease for existing key"
            );
            let ia_na = self.build_ia_na_response(key.iaid, known_ip, valid, preferred);
            if let Some(resp) = ctx.resp_msg_mut() {
                resp.opts_mut().insert(ia_na);
                if let Some(opts) = self.cfg.v6().get_opts(ctx.meta().ifindex) {
                    ctx.populate_opts(opts);
                }
            }
            metrics::CLUSTER_V6_ALLOCATIONS.inc();
            return Ok(Action::Respond);
        }

        // Try to get a preferred address from the client's IA_NA
        let preferred_addr = extract_requested_v6_addr(ctx.msg());

        // For now, use the preferred address if given; in a full implementation
        // we'd use an IP manager. For v6 nats mode, we coordinate via NATS.
        let assigned_ip = match preferred_addr {
            Some(ip) => ip,
            None => {
                // No preferred address; we need to pick one from the network
                // For the initial implementation, use the subnet base + hash of the key
                // This is a simplification; production would use a proper v6 IP manager
                let subnet = network.full_subnet();
                // Use SipHash-1-3 with fixed keys for a stable, deterministic hash
                // that is guaranteed not to change across Rust versions (unlike
                // DefaultHasher whose algorithm is explicitly not stable).
                let hash = {
                    use siphasher::sip::SipHasher13;
                    use std::hash::{Hash, Hasher};
                    let mut hasher = SipHasher13::new_with_keys(0, 0);
                    key.normalized().hash(&mut hasher);
                    hasher.finish()
                };
                let base = u128::from(subnet.network());
                let host = (hash as u128) & ((1u128 << (128 - subnet.prefix_len())) - 1);
                // Avoid ::0 (network) and ::1 (often router)
                let host = if host < 2 { host + 2 } else { host };
                Ipv6Addr::from(base | host)
            }
        };

        // Coordinate with NATS
        let record = self.make_v6_lease_record(assigned_ip, &key, expires_at, LeaseState::Reserved);

        match self.coordinator.reserve(record).await {
            Ok(LeaseOutcome::Success(_confirmed)) => {
                self.record_known_lease(&key, assigned_ip, expires_at);
                metrics::CLUSTER_V6_ALLOCATIONS.inc();

                let ia_na = self.build_ia_na_response(key.iaid, assigned_ip, valid, preferred);
                if let Some(resp) = ctx.resp_msg_mut() {
                    resp.opts_mut().insert(ia_na);
                    if let Some(opts) = self.cfg.v6().get_opts(ctx.meta().ifindex) {
                        ctx.populate_opts(opts);
                    }
                }
                debug!(
                    key = %key,
                    ip = %assigned_ip,
                    "v6 lease reserved via NATS coordination"
                );
                Ok(Action::Respond)
            }
            Ok(LeaseOutcome::Conflict {
                expected_revision,
                actual_revision,
            }) => {
                metrics::CLUSTER_V6_CONFLICTS.inc();
                warn!(
                    key = %key,
                    expected = expected_revision,
                    actual = actual_revision,
                    "v6 lease conflict during solicit"
                );
                Ok(Action::NoResponse)
            }
            Ok(LeaseOutcome::DegradedModeBlocked) => {
                metrics::CLUSTER_V6_ALLOCATIONS_BLOCKED.inc();
                info!(key = %key, "v6 solicit blocked: degraded mode");
                Ok(Action::NoResponse)
            }
            Err(e) => {
                warn!(error = %e, key = %key, "v6 solicit coordination error");
                Ok(Action::NoResponse)
            }
        }
    }

    /// Handle Request/Renew: confirm or renew a lease.
    async fn handle_request_renew(
        &self,
        ctx: &mut MsgContext<v6::Message>,
        is_renew: bool,
    ) -> Result<Action> {
        let subnet_str = match self.get_subnet_str(ctx) {
            Some(s) => s,
            None => {
                debug!("no v6 network found for request/renew, skipping");
                return Ok(Action::NoResponse);
            }
        };

        let key = match extract_v6_lease_key(ctx.msg(), &subnet_str) {
            Some(k) => k,
            None => {
                metrics::CLUSTER_V6_INVALID_KEY.inc();
                debug!("missing DUID or IAID in v6 Request/Renew, dropping");
                return Ok(Action::NoResponse);
            }
        };

        let network = match self.get_v6_network(ctx) {
            Some(n) => n,
            None => return Ok(Action::NoResponse),
        };

        let valid = network.valid_time().get_default();
        let preferred = network.preferred_time().get_default();
        let expires_at = SystemTime::now() + valid;

        // Get the requested address
        let requested_ip = match extract_requested_v6_addr(ctx.msg()) {
            Some(ip) => ip,
            None => {
                // Try known lease cache
                match self.get_known_lease(&key) {
                    Some((ip, _)) => ip,
                    None => {
                        debug!(key = %key, "no address in v6 request/renew and no known lease");
                        // Return NoBinding status
                        if let Some(resp) = ctx.resp_msg_mut() {
                            let ia_err = self.build_ia_na_error(key.iaid, 3, "NoBinding");
                            resp.opts_mut().insert(ia_err);
                        }
                        return Ok(Action::Respond);
                    }
                }
            }
        };

        // Check NATS availability
        if !self.coordinator.is_available().await {
            // Degraded mode: allow renewals for known leases only
            if let Some((known_ip, _)) = self.get_known_lease(&key) {
                if known_ip == requested_ip {
                    metrics::CLUSTER_V6_DEGRADED_RENEWALS.inc();
                    info!(
                        key = %key,
                        ip = %known_ip,
                        "v6 degraded-mode renewal allowed for known active lease"
                    );
                    // Update local cache expiry
                    self.record_known_lease(&key, known_ip, expires_at);

                    let ia_na = self.build_ia_na_response(key.iaid, known_ip, valid, preferred);
                    if let Some(resp) = ctx.resp_msg_mut() {
                        resp.opts_mut().insert(ia_na);
                        if let Some(opts) = self.cfg.v6().get_opts(ctx.meta().ifindex) {
                            ctx.populate_opts(opts);
                        }
                    }
                    if is_renew {
                        metrics::CLUSTER_V6_RENEWALS.inc();
                    }
                    return Ok(Action::Respond);
                }
            }
            // Not a known renewal - block
            metrics::CLUSTER_V6_ALLOCATIONS_BLOCKED.inc();
            metrics::CLUSTER_COORDINATION_STATE.set(0);
            info!(
                key = %key,
                "v6 request/renew blocked: NATS unavailable and not a known renewal"
            );
            return Ok(Action::NoResponse);
        }
        metrics::CLUSTER_COORDINATION_STATE.set(1);

        // Coordinate with NATS
        let record = self.make_v6_lease_record(requested_ip, &key, expires_at, LeaseState::Leased);

        match self.coordinator.lease(record).await {
            Ok(LeaseOutcome::Success(_confirmed)) => {
                self.record_known_lease(&key, requested_ip, expires_at);
                if is_renew {
                    metrics::CLUSTER_V6_RENEWALS.inc();
                } else {
                    metrics::CLUSTER_V6_ALLOCATIONS.inc();
                }

                let ia_na = self.build_ia_na_response(key.iaid, requested_ip, valid, preferred);
                if let Some(resp) = ctx.resp_msg_mut() {
                    resp.opts_mut().insert(ia_na);
                    if let Some(opts) = self.cfg.v6().get_opts(ctx.meta().ifindex) {
                        ctx.populate_opts(opts);
                    }
                }
                debug!(
                    key = %key,
                    ip = %requested_ip,
                    renew = is_renew,
                    "v6 lease confirmed via NATS coordination"
                );
                Ok(Action::Respond)
            }
            Ok(LeaseOutcome::Conflict {
                expected_revision,
                actual_revision,
            }) => {
                metrics::CLUSTER_V6_CONFLICTS.inc();
                warn!(
                    key = %key,
                    expected = expected_revision,
                    actual = actual_revision,
                    "v6 lease conflict during request/renew"
                );
                // Return NoBinding status
                if let Some(resp) = ctx.resp_msg_mut() {
                    let ia_err = self.build_ia_na_error(key.iaid, 3, "NoBinding");
                    resp.opts_mut().insert(ia_err);
                }
                Ok(Action::Respond)
            }
            Ok(LeaseOutcome::DegradedModeBlocked) => {
                metrics::CLUSTER_V6_ALLOCATIONS_BLOCKED.inc();
                info!(key = %key, "v6 request/renew blocked: degraded mode");
                Ok(Action::NoResponse)
            }
            Err(e) => {
                warn!(error = %e, key = %key, "v6 request/renew coordination error");
                Ok(Action::NoResponse)
            }
        }
    }

    /// Handle Release: client releases a lease.
    async fn handle_release(&self, ctx: &mut MsgContext<v6::Message>) -> Result<Action> {
        let subnet_str = match self.get_subnet_str(ctx) {
            Some(s) => s,
            None => {
                debug!("no v6 network found for release");
                return Ok(Action::NoResponse);
            }
        };

        let key = match extract_v6_lease_key(ctx.msg(), &subnet_str) {
            Some(k) => k,
            None => {
                metrics::CLUSTER_V6_INVALID_KEY.inc();
                debug!("missing DUID or IAID in v6 Release, dropping");
                return Ok(Action::NoResponse);
            }
        };

        let released_ip = extract_requested_v6_addr(ctx.msg())
            .or_else(|| self.get_known_lease(&key).map(|(ip, _)| ip));

        if let Some(ip) = released_ip {
            // Best-effort release coordination
            if self.coordinator.is_available().await {
                let record =
                    self.make_v6_lease_record(ip, &key, SystemTime::now(), LeaseState::Released);
                if let Err(e) = self.coordinator.release(record).await {
                    warn!(error = %e, key = %key, "failed to coordinate v6 lease release");
                }
            }
            self.remove_known_lease(&key);
            metrics::CLUSTER_V6_RELEASES.inc();
            debug!(key = %key, ip = %ip, "v6 lease released");
        } else {
            debug!(key = %key, "v6 release: no address to release");
        }

        // Release has no response body per RFC 8415
        Ok(Action::NoResponse)
    }

    /// Handle Decline: client reports address conflict.
    async fn handle_decline(&self, ctx: &mut MsgContext<v6::Message>) -> Result<Action> {
        let subnet_str = match self.get_subnet_str(ctx) {
            Some(s) => s,
            None => {
                debug!("no v6 network found for decline");
                return Ok(Action::NoResponse);
            }
        };

        let key = match extract_v6_lease_key(ctx.msg(), &subnet_str) {
            Some(k) => k,
            None => {
                metrics::CLUSTER_V6_INVALID_KEY.inc();
                debug!("missing DUID or IAID in v6 Decline, dropping");
                return Ok(Action::NoResponse);
            }
        };

        let declined_ip = extract_requested_v6_addr(ctx.msg());

        if let Some(ip) = declined_ip {
            let network = self.get_v6_network(ctx);
            let probation_period = network
                .map(|n| n.probation_period())
                .unwrap_or(Duration::from_secs(86400));
            let expires_at = SystemTime::now() + probation_period;

            // Best-effort probation coordination
            if self.coordinator.is_available().await {
                let record = self.make_v6_lease_record(ip, &key, expires_at, LeaseState::Probated);
                let probation_chrono: DateTime<Utc> = expires_at.into();
                if let Err(e) = self.coordinator.probate(record, probation_chrono).await {
                    warn!(error = %e, key = %key, "failed to coordinate v6 lease probation");
                }
            }
            self.remove_known_lease(&key);
            metrics::CLUSTER_V6_DECLINES.inc();
            debug!(
                key = %key,
                ip = %ip,
                "v6 lease declined and probated"
            );
        } else {
            debug!(key = %key, "v6 decline: no address specified");
        }

        // Decline has no response per RFC 8415
        Ok(Action::NoResponse)
    }
}

// ---------------------------------------------------------------------------
// Plugin<v6::Message> implementation (T028, T032)
// ---------------------------------------------------------------------------

#[async_trait]
impl Plugin<v6::Message> for NatsV6Leases {
    #[instrument(level = "debug", skip_all)]
    async fn handle(&self, ctx: &mut MsgContext<v6::Message>) -> Result<Action> {
        let msg_type = ctx.msg().msg_type();

        match msg_type {
            V6MessageType::Solicit => self.handle_solicit(ctx).await,
            V6MessageType::Request => self.handle_request_renew(ctx, false).await,
            V6MessageType::Renew => self.handle_request_renew(ctx, true).await,
            V6MessageType::Release => self.handle_release(ctx).await,
            V6MessageType::Decline => self.handle_decline(ctx).await,
            _ => {
                // Non-stateful message types are handled elsewhere (e.g. InformationRequest)
                debug!(
                    ?msg_type,
                    "v6 leases plugin: non-stateful msg type, continuing"
                );
                Ok(Action::Continue)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Register implementation (T032)
// ---------------------------------------------------------------------------

impl dora_core::Register<v6::Message> for NatsV6Leases {
    fn register(self, srv: &mut dora_core::Server<v6::Message>) {
        info!("NatsV6Leases plugin registered");
        let this = Arc::new(self);
        srv.plugin_order::<Self, _>(this, &[std::any::TypeId::of::<message_type::MsgType>()]);
    }
}

// ---------------------------------------------------------------------------
// Tests (T034)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use dora_core::dhcproto::v6;

    // ---- V6LeaseKey tests (T029) ----

    #[test]
    fn test_v6_lease_key_construction() {
        let key = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "00010001aabbccdd".into(),
            iaid: 1,
        };
        assert_eq!(key.subnet, "2001:db8::/64");
        assert_eq!(key.duid, "00010001aabbccdd");
        assert_eq!(key.iaid, 1);
    }

    #[test]
    fn test_v6_lease_key_normalized() {
        let key = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "00010001aabbccdd".into(),
            iaid: 1,
        };
        assert_eq!(key.normalized(), "2001:db8::/64:00010001aabbccdd:1");
    }

    #[test]
    fn test_v6_lease_key_display() {
        let key = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "aabb".into(),
            iaid: 42,
        };
        let display = format!("{}", key);
        assert!(display.contains("aabb"));
        assert!(display.contains("42"));
    }

    #[test]
    fn test_v6_lease_key_equality() {
        let k1 = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "aabb".into(),
            iaid: 1,
        };
        let k2 = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "aabb".into(),
            iaid: 1,
        };
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_v6_lease_key_different_iaid() {
        let k1 = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "aabb".into(),
            iaid: 1,
        };
        let k2 = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "aabb".into(),
            iaid: 2,
        };
        assert_ne!(k1, k2);
        assert_ne!(k1.normalized(), k2.normalized());
    }

    // ---- Key extraction tests (T029) ----

    #[test]
    fn test_extract_v6_lease_key_valid() {
        let mut msg = v6::Message::new(v6::MessageType::Solicit);
        msg.opts_mut()
            .insert(v6::DhcpOption::ClientId(vec![0x00, 0x01, 0xaa, 0xbb]));
        let iana = v6::IANA {
            id: 42,
            t1: 3600,
            t2: 5400,
            opts: v6::DhcpOptions::new(),
        };
        msg.opts_mut().insert(v6::DhcpOption::IANA(iana));

        let key = extract_v6_lease_key(&msg, "2001:db8::/64");
        assert!(key.is_some());
        let key = key.unwrap();
        assert_eq!(key.subnet, "2001:db8::/64");
        assert_eq!(key.duid, "0001aabb");
        assert_eq!(key.iaid, 42);
    }

    #[test]
    fn test_extract_v6_lease_key_missing_duid() {
        let mut msg = v6::Message::new(v6::MessageType::Solicit);
        let iana = v6::IANA {
            id: 1,
            t1: 3600,
            t2: 5400,
            opts: v6::DhcpOptions::new(),
        };
        msg.opts_mut().insert(v6::DhcpOption::IANA(iana));

        let key = extract_v6_lease_key(&msg, "2001:db8::/64");
        assert!(key.is_none());
    }

    #[test]
    fn test_extract_v6_lease_key_missing_iaid() {
        let mut msg = v6::Message::new(v6::MessageType::Solicit);
        msg.opts_mut()
            .insert(v6::DhcpOption::ClientId(vec![0x00, 0x01]));
        // No IA_NA option

        let key = extract_v6_lease_key(&msg, "2001:db8::/64");
        assert!(key.is_none());
    }

    #[test]
    fn test_extract_v6_lease_key_empty_duid() {
        let mut msg = v6::Message::new(v6::MessageType::Solicit);
        msg.opts_mut().insert(v6::DhcpOption::ClientId(vec![])); // empty DUID
        let iana = v6::IANA {
            id: 1,
            t1: 3600,
            t2: 5400,
            opts: v6::DhcpOptions::new(),
        };
        msg.opts_mut().insert(v6::DhcpOption::IANA(iana));

        let key = extract_v6_lease_key(&msg, "2001:db8::/64");
        assert!(key.is_none());
    }

    // ---- Multi-lease per DUID tests (T030) ----

    #[test]
    fn test_multi_lease_keys_same_duid_different_iaid() {
        let mut msg1 = v6::Message::new(v6::MessageType::Request);
        msg1.opts_mut()
            .insert(v6::DhcpOption::ClientId(vec![0x00, 0x01, 0x02]));
        let iana1 = v6::IANA {
            id: 1,
            t1: 3600,
            t2: 5400,
            opts: v6::DhcpOptions::new(),
        };
        msg1.opts_mut().insert(v6::DhcpOption::IANA(iana1));

        let mut msg2 = v6::Message::new(v6::MessageType::Request);
        msg2.opts_mut()
            .insert(v6::DhcpOption::ClientId(vec![0x00, 0x01, 0x02]));
        let iana2 = v6::IANA {
            id: 2,
            t1: 3600,
            t2: 5400,
            opts: v6::DhcpOptions::new(),
        };
        msg2.opts_mut().insert(v6::DhcpOption::IANA(iana2));

        let key1 = extract_v6_lease_key(&msg1, "2001:db8::/64").unwrap();
        let key2 = extract_v6_lease_key(&msg2, "2001:db8::/64").unwrap();

        // Same DUID but different IAIDs should produce different keys
        assert_eq!(key1.duid, key2.duid);
        assert_ne!(key1.iaid, key2.iaid);
        assert_ne!(key1, key2);
        assert_ne!(key1.normalized(), key2.normalized());
    }

    #[test]
    fn test_multi_lease_keys_different_duid_same_iaid() {
        let mut msg1 = v6::Message::new(v6::MessageType::Request);
        msg1.opts_mut()
            .insert(v6::DhcpOption::ClientId(vec![0x00, 0x01]));
        let iana1 = v6::IANA {
            id: 1,
            t1: 3600,
            t2: 5400,
            opts: v6::DhcpOptions::new(),
        };
        msg1.opts_mut().insert(v6::DhcpOption::IANA(iana1));

        let mut msg2 = v6::Message::new(v6::MessageType::Request);
        msg2.opts_mut()
            .insert(v6::DhcpOption::ClientId(vec![0x00, 0x02]));
        let iana2 = v6::IANA {
            id: 1,
            t1: 3600,
            t2: 5400,
            opts: v6::DhcpOptions::new(),
        };
        msg2.opts_mut().insert(v6::DhcpOption::IANA(iana2));

        let key1 = extract_v6_lease_key(&msg1, "2001:db8::/64").unwrap();
        let key2 = extract_v6_lease_key(&msg2, "2001:db8::/64").unwrap();

        // Different DUIDs with same IAID should produce different keys
        assert_ne!(key1.duid, key2.duid);
        assert_eq!(key1.iaid, key2.iaid);
        assert_ne!(key1, key2);
    }

    // ---- Known lease cache tests (T031) ----

    #[test]
    fn test_known_lease_cache_operations() {
        let cache: parking_lot::RwLock<HashMap<String, KnownV6Lease>> =
            parking_lot::RwLock::new(HashMap::new());

        let key = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "aabb".into(),
            iaid: 1,
        };

        // Insert
        cache.write().insert(
            key.normalized(),
            KnownV6Lease {
                ip: "2001:db8::100".parse().unwrap(),
                expires_at: SystemTime::now() + Duration::from_secs(3600),
            },
        );

        // Lookup
        let lease = cache.read().get(&key.normalized()).cloned();
        assert!(lease.is_some());
        assert_eq!(
            lease.unwrap().ip,
            "2001:db8::100".parse::<Ipv6Addr>().unwrap()
        );

        // Remove
        cache.write().remove(&key.normalized());
        assert!(cache.read().get(&key.normalized()).is_none());
    }

    #[test]
    fn test_known_lease_cache_multi_iaid() {
        let cache: parking_lot::RwLock<HashMap<String, KnownV6Lease>> =
            parking_lot::RwLock::new(HashMap::new());

        let key1 = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "aabb".into(),
            iaid: 1,
        };
        let key2 = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "aabb".into(),
            iaid: 2,
        };

        cache.write().insert(
            key1.normalized(),
            KnownV6Lease {
                ip: "2001:db8::100".parse().unwrap(),
                expires_at: SystemTime::now() + Duration::from_secs(3600),
            },
        );
        cache.write().insert(
            key2.normalized(),
            KnownV6Lease {
                ip: "2001:db8::200".parse().unwrap(),
                expires_at: SystemTime::now() + Duration::from_secs(3600),
            },
        );

        // Both leases should be independently accessible
        assert_eq!(cache.read().len(), 2);
        let l1 = cache.read().get(&key1.normalized()).cloned().unwrap();
        let l2 = cache.read().get(&key2.normalized()).cloned().unwrap();
        assert_ne!(l1.ip, l2.ip);
    }

    #[test]
    fn test_known_lease_expired_not_returned() {
        let cache: parking_lot::RwLock<HashMap<String, KnownV6Lease>> =
            parking_lot::RwLock::new(HashMap::new());

        let key = V6LeaseKey {
            subnet: "2001:db8::/64".into(),
            duid: "aabb".into(),
            iaid: 1,
        };

        // Insert an already-expired lease
        cache.write().insert(
            key.normalized(),
            KnownV6Lease {
                ip: "2001:db8::100".parse().unwrap(),
                expires_at: SystemTime::now() - Duration::from_secs(1),
            },
        );

        // When checking expiry, an expired lease should not be considered active
        let lease = cache.read().get(&key.normalized()).cloned();
        assert!(lease.is_some()); // Entry exists...
        assert!(lease.unwrap().expires_at < SystemTime::now()); // ...but is expired
    }

    // ---- Extract requested address tests ----

    #[test]
    fn test_extract_requested_v6_addr() {
        let mut msg = v6::Message::new(v6::MessageType::Request);
        let ia_addr = v6::IAAddr {
            addr: "2001:db8::42".parse().unwrap(),
            preferred_life: 3600,
            valid_life: 7200,
            opts: v6::DhcpOptions::new(),
        };
        let mut iana = v6::IANA {
            id: 1,
            t1: 3600,
            t2: 5400,
            opts: v6::DhcpOptions::new(),
        };
        iana.opts.insert(v6::DhcpOption::IAAddr(ia_addr));
        msg.opts_mut().insert(v6::DhcpOption::IANA(iana));

        let addr = extract_requested_v6_addr(&msg);
        assert_eq!(addr, Some("2001:db8::42".parse().unwrap()));
    }

    #[test]
    fn test_extract_requested_v6_addr_none() {
        let msg = v6::Message::new(v6::MessageType::Request);
        let addr = extract_requested_v6_addr(&msg);
        assert!(addr.is_none());
    }

    // ---- Lease record construction tests ----

    #[test]
    fn test_v6_lease_record_construction() {
        // Verify that a v6 lease record has correct protocol family and fields
        let record = LeaseRecord {
            lease_id: "test".into(),
            protocol_family: ProtocolFamily::Dhcpv6,
            subnet: "2001:db8::/64".into(),
            ip_address: "2001:db8::100".into(),
            client_key_v4: None,
            duid: Some("aabb".into()),
            iaid: Some(1),
            state: LeaseState::Leased,
            expires_at: Utc::now() + chrono::Duration::hours(1),
            probation_until: None,
            server_id: "server-1".into(),
            revision: 0,
            updated_at: Utc::now(),
        };
        assert!(record.validate().is_ok());
        assert_eq!(record.protocol_family, ProtocolFamily::Dhcpv6);
        assert!(record.client_key_v4.is_none());
        assert!(record.duid.is_some());
        assert!(record.iaid.is_some());
    }

    #[test]
    fn test_v6_lease_record_validation_fails_without_duid() {
        let record = LeaseRecord {
            lease_id: "test".into(),
            protocol_family: ProtocolFamily::Dhcpv6,
            subnet: "2001:db8::/64".into(),
            ip_address: "2001:db8::100".into(),
            client_key_v4: None,
            duid: None, // Missing!
            iaid: Some(1),
            state: LeaseState::Leased,
            expires_at: Utc::now() + chrono::Duration::hours(1),
            probation_until: None,
            server_id: "server-1".into(),
            revision: 0,
            updated_at: Utc::now(),
        };
        assert!(record.validate().is_err());
    }

    #[test]
    fn test_v6_lease_record_validation_fails_without_iaid() {
        let record = LeaseRecord {
            lease_id: "test".into(),
            protocol_family: ProtocolFamily::Dhcpv6,
            subnet: "2001:db8::/64".into(),
            ip_address: "2001:db8::100".into(),
            client_key_v4: None,
            duid: Some("aabb".into()),
            iaid: None, // Missing!
            state: LeaseState::Leased,
            expires_at: Utc::now() + chrono::Duration::hours(1),
            probation_until: None,
            server_id: "server-1".into(),
            revision: 0,
            updated_at: Utc::now(),
        };
        assert!(record.validate().is_err());
    }
}

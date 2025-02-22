#![allow(clippy::too_many_arguments)]

//! # ip-manager
//!
//! `ip-manager` defines a trait `Storage` that provides methods for doing
//! getting & updating IPs in storage.
//!
//! This trait is not meant to be used by plugins directly. Instead, it's wrapped
//! in a `IpManager` type which takes a generic parameter that must implement `Storage`
//! `IpManager` then uses those methods to do the job of reserving/leasing ips while maintaining
//! a nicer interface for the plugin to interact with.
//!
//! [`Storage`]: ip_manager::Storage
//! [`IpManager`]: ip_manager::IpManager
use config::v4::{NetRange, Network};
use icmp_ping::{Icmpv4, Listener, PingReply};

use async_trait::async_trait;
use chrono::DateTime;
use chrono::{SecondsFormat, offset::Utc};
use thiserror::Error;
use tracing::{debug, error, info, trace, warn};

pub mod sqlite;

use core::fmt;
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
    ops::RangeInclusive,
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    time::{Duration, SystemTime},
};

const PING_TTL: u64 = 60;
pub type ClientId = Option<Vec<u8>>;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ClientInfo {
    ip: IpAddr,
    id: ClientId,
    network: IpAddr,
    expires_at: SystemTime,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum IpState {
    Lease,
    Probate,
    Reserve,
}

/// our sqlite impl doesn't properly support enums, so this
/// converts our 3 state system into 2 bools.
impl From<IpState> for (bool, bool) {
    fn from(state: IpState) -> Self {
        match state {
            IpState::Lease => (true, false),
            IpState::Probate => (false, true),
            IpState::Reserve => (false, false),
        }
    }
}

#[async_trait]
pub trait Storage: Send + Sync + 'static {
    // send/sync/static required for async trait bounds
    type Error: std::error::Error + Send + Sync + 'static;
    /// updates if expired & ip matches or if ip & id match
    async fn update_expired(
        &self,
        ip: IpAddr,
        state: Option<IpState>,
        id: &[u8],
        expires_at: SystemTime,
    ) -> Result<bool, Self::Error>;
    async fn insert(
        &self,
        ip: IpAddr,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<(), Self::Error>;

    async fn get(&self, ip: IpAddr) -> Result<Option<State>, Self::Error>;
    async fn get_id(&self, id: &[u8]) -> Result<Option<IpAddr>, Self::Error>;
    async fn release_ip(&self, ip: IpAddr, id: &[u8]) -> Result<Option<ClientInfo>, Self::Error>;
    async fn delete(&self, ip: IpAddr) -> Result<(), Self::Error>;

    async fn next_expired(
        &self,
        range: RangeInclusive<IpAddr>,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<Option<IpAddr>, Self::Error>;

    async fn insert_max_in_range(
        &self,
        range: RangeInclusive<IpAddr>,
        // TODO not ipv4
        exclusions: &HashSet<Ipv4Addr>,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<Option<IpAddr>, Self::Error>;
    /// updates if not expired & id & ip match
    async fn update_unexpired(
        &self,
        ip: IpAddr,
        state: IpState,
        id: &[u8],
        expires_at: SystemTime,
        new_id: Option<&[u8]>,
    ) -> Result<Option<IpAddr>, Self::Error>;
    async fn update_ip(
        &self,
        ip: IpAddr,
        state: IpState,
        id: Option<&[u8]>,
        expires_at: SystemTime,
    ) -> Result<Option<State>, Self::Error>;
    async fn count(&self, state: IpState) -> Result<usize, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Reserved(ClientInfo),
    Leased(ClientInfo),
    Probated(ClientInfo),
}

impl AsRef<ClientInfo> for State {
    fn as_ref(&self) -> &ClientInfo {
        match self {
            State::Reserved(info) => info,
            State::Leased(info) => info,
            State::Probated(info) => info,
        }
    }
}

impl State {
    pub fn into(self) -> ClientInfo {
        match self {
            State::Reserved(info) => info,
            State::Leased(info) => info,
            State::Probated(info) => info,
        }
    }
}

pub struct IpManager<T> {
    store: T,
    icmpv4: Arc<IcmpInner>,
    ping_cache: moka::future::Cache<IpAddr, Option<PingReply>>,
}

impl<T> fmt::Debug for IpManager<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IpManager {{}}")
            // .field("store", &self.store)
            // .field("icmpv4", &self.icmpv4)
            // .field("ping_cache", &self.ping_cache)
            .finish()
    }
}

impl<T: Clone> Clone for IpManager<T> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            icmpv4: self.icmpv4.clone(),
            ping_cache: self.ping_cache.clone(),
        }
    }
}

pub(crate) struct IcmpInner {
    seq_cnt: AtomicU16,
    listener: Listener<Icmpv4>,
}

impl<T> IpManager<T>
where
    T: Storage,
{
    /// Check to see if the address is in use.
    /// If `Network` has `ping_check` set to `true`, we will test to see if the IP is already
    /// being used by another client
    async fn addr_in_use(
        &self,
        ip: IpAddr,
        timeout: Duration,
    ) -> Result<PingReply, icmp_ping::Error> {
        let seq_cnt = self.icmpv4.seq_cnt.fetch_add(1, Ordering::Relaxed);
        // send a single ping
        self.icmpv4
            .listener
            .pinger(ip)
            .timeout(timeout)
            .ping(seq_cnt)
            .await
        // ping succeeded, meaning addr is in use
    }

    /// used for tests to insert into ping cache
    #[cfg(test)]
    pub(crate) async fn ping_insert(&self, ip: IpAddr, reply: Option<PingReply>) {
        self.ping_cache.insert(ip, reply).await
    }

    /// returns Ok(()) if ping failed or ping == false
    /// returns Err if ping succeeded
    pub async fn ping_check(&self, ip: IpAddr, network: &Network) -> Result<(), IpError<T::Error>> {
        if network.ping_check() {
            let fut = async {
                match self.addr_in_use(ip, network.ping_timeout()).await {
                    Ok(reply) => {
                        // ping succeeded
                        if let Err(err) = self.store.delete(ip).await {
                            error!(?err, "error attempting to delete ip");
                        }
                        Some(reply)
                    }
                    // ping failed, so addr is not in use
                    Err(_) => None,
                }
            };
            match self.ping_cache.get_with(ip, fut).await {
                Some(_reply) => Err(IpError::AddrInUse(ip)),
                None => Ok(()),
            }
        } else {
            Ok(())
        }
    }
}

impl<T> IpManager<T>
where
    T: Storage,
{
    pub fn new(store: T) -> Result<Self, icmp_ping::Error> {
        Ok(Self {
            icmpv4: Arc::new(IcmpInner {
                seq_cnt: AtomicU16::new(1),
                listener: Listener::<Icmpv4>::new()?,
            }),
            store,
            ping_cache: moka::future::CacheBuilder::new(1_000)
                // time_to_idle?
                .time_to_live(Duration::from_secs(PING_TTL))
                .initial_capacity(1_000)
                .build(),
        })
    }

    /// get the first available IP in a range with a given id/expiry/network
    pub async fn reserve_first(
        &self,
        range: &NetRange,
        network: &Network,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<IpAddr, IpError<T::Error>> {
        const MAX_ATTEMPTS: usize = 2;
        let subnet = network.subnet().into();
        // unfortunately the sqlite connection is sometimes unreliable under high contention, meaning
        // we need to make a few attempts to get an address.
        let mut attempts = 0;
        loop {
            let ip_range = range.start().into()..=range.end().into();
            if attempts > MAX_ATTEMPTS {
                return Err(IpError::MaxAttempts {
                    range: ip_range,
                    attempts,
                });
            }
            // find the min expired IP or where id matches
            let ip = match self
                .store
                .next_expired(ip_range.clone(), subnet, id, expires_at, state)
                .await
            {
                Ok(Some(ip)) => ip,
                // the range has no expired entries, so find the next available IP in the range
                Ok(None) => match self
                    .store
                    .insert_max_in_range(
                        ip_range.clone(),
                        range.exclusions(),
                        subnet,
                        id,
                        expires_at,
                        state,
                    )
                    .await
                {
                    Ok(ip) => ip.ok_or(IpError::RangeError {
                        range: ip_range.clone(),
                    })?,
                    Err(err) => {
                        attempts += 1;
                        warn!(?err, "error grabbing new IP-- retrying");
                        continue;
                    }
                },
                Err(err) => {
                    attempts += 1;
                    warn!(?err, "error grabbing next expired IP-- retrying");
                    continue;
                }
            };
            match ip {
                IpAddr::V4(ipv4) => {
                    if range.contains(&ipv4) {
                        // ping_check will delete the expired entry if it's in use
                        match self.ping_check(ip, network).await {
                            Ok(()) => return Ok(ip),
                            // ping success so insert probated IP
                            Err(err) => {
                                let probation_time = SystemTime::now() + network.probation_period();
                                info!(
                                    ?err,
                                    probation_time = %DateTime::<Utc>::from(probation_time).to_rfc3339_opts(SecondsFormat::Secs, true),
                                    "ping succeeded. address is in use. marking IP on probation"
                                );
                                // update regardless of expiry/id because something is using the IP
                                if let Err(err) = self
                                    .store
                                    .update_ip(ip, IpState::Probate, None, probation_time)
                                    .await
                                {
                                    attempts += 1;
                                    error!(?err, "failed to probate IP on ping success");
                                    // not returning error because we must give client an IP
                                } else {
                                    debug!("IP put on probation, trying next");
                                }
                                continue;
                            }
                        }
                    } else {
                        attempts += 1;
                        warn!(
                            ?range,
                            ?ipv4,
                            "IP for client id returned from leases table is outside of network range"
                        );
                        // entry for ip/id but the range doesn't match, remove the old entry
                        if let Err(err) = self.store.release_ip(ip, id).await {
                            error!(?err, "failed to delete entry");
                        }
                        continue;
                    }
                }
                // we know this method is only called in ipv4 code, but the
                // compiler doesn't
                _ => panic!("ipv6 unsupported"),
            }
        }
    }

    /// tries to take an ip for an id that's set to expire at some future time.
    /// If `ping` is set, will send a ping to the IP, returning an error if in use
    /// Returns
    ///     `Err` if ip/id are already present or ping succeeded
    ///     `Ok(())` allocated IP successfully
    pub async fn try_ip(
        &self,
        ip: IpAddr,
        subnet: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        network: &Network,
        state: Option<IpState>,
    ) -> Result<(), IpError<T::Error>> {
        // TODO: there may be a way to remove this .get also
        if self.store.get(ip).await?.is_some() {
            return if self.store.update_expired(ip, state, id, expires_at).await? {
                debug!(
                    ?ip,
                    ?id,
                    "set reserved, found ip/id for this client or expired"
                );
                Ok(())
            } else {
                debug!("IP not updated, couldn't find ip/id or in use");
                Err(IpError::AddrInUse(ip))
            };
        };
        // if the entry doesn't exist yet & ping fails, insert it
        self.store.insert(ip, subnet, id, expires_at, state).await?;
        // not marking for probation because request IP can be sent at any time
        self.ping_check(ip, network).await?;

        Ok(())
    }

    /// sees if there is an un-expired IP associated with this ID
    /// Returns
    ///     Err if expired or id not found
    ///     Ok(ip) un-expired id found in storage
    pub async fn lookup_id(&self, id: &[u8]) -> Result<IpAddr, IpError<T::Error>> {
        match self.store.get_id(id).await? {
            Some(ip) => {
                debug!(?ip, ?id, "we have an IP for this id");
                Ok(ip)
            }
            None => {
                debug!(?id, "no IP found for this id");
                Err(IpError::Unreserved)
            }
        }
    }
    /// Sets a reserved ip/id combo to leased state. If no un-expired ip/id pair
    /// found, then if we're authoritative we will just try to insert the IP, and
    /// if not we return.
    /// Returns
    ///     Err if ip/id don't match what's in storage or if it's expired
    ///     Ok(()) entry created successfully for lease
    pub async fn try_lease(
        &self,
        ip: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        network: &Network,
    ) -> Result<(), IpError<T::Error>> {
        match self
            .store
            .update_unexpired(ip, IpState::Lease, id, expires_at, Some(id))
            .await?
        {
            Some(ip) => {
                debug!(
                    ?ip,
                    ?id,
                    "found ip for id-- updating expiry and setting leased"
                );
                Ok(())
            }
            None if network.authoritative() => {
                debug!(
                    ?ip,
                    ?id,
                    "no IP with this id found or expired. authoritative, trying insert"
                );

                // this will ACK even if there was no prior DISCOVER
                match self
                    .store
                    .insert(
                        ip,
                        network.subnet().into(),
                        id,
                        expires_at,
                        Some(IpState::Lease),
                    )
                    .await
                {
                    Ok(()) => {
                        trace!("inserted new IP");
                        Ok(())
                    }
                    Err(err) => {
                        warn!(
                            ?err,
                            "insert failed, likely ip already exists & taken by another client"
                        );
                        Err(IpError::AddrInUse(ip))
                    }
                }
            }
            None => {
                debug!(?ip, ?id, "no IP with this id found or expired");
                Err(IpError::AddrInUse(ip))
            }
        }
    }

    /// release the requested ip if the (ip, id) pair matches
    /// Returns
    ///     Ok(None) if ip did not exist in storage
    ///     Ok(Some(info)) the existing client info
    ///     Err(_) for database error
    pub async fn release_ip(
        &self,
        ip: IpAddr,
        id: &[u8],
    ) -> Result<Option<ClientInfo>, IpError<T::Error>> {
        // TODO: this deletes the entry, but we don't really need to
        Ok(self.store.release_ip(ip, id).await?)
    }

    /// Will mark IP for probation if it is un-expired and ip/id match
    /// we check to see if it has expired because a DECLINE happens after
    /// an address has been ACKd.
    pub async fn probate_ip(
        &self,
        ip: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
    ) -> Result<(), IpError<T::Error>> {
        match self
            .store
            .update_unexpired(ip, IpState::Probate, id, expires_at, None)
            .await?
        {
            Some(ip) => {
                debug!(
                    ?ip,
                    ?id,
                    "found ip for id-- updating expiry and set PROBATION"
                );
                Ok(())
            }
            None => {
                debug!(
                    ?ip,
                    ?id,
                    "tried to set PROBATION, but no IP with this id found"
                );
                Err(IpError::AddrInUse(ip))
            }
        }
    }
}

#[derive(Error, Debug)]
pub enum IpError<E> {
    #[error("ip is leased {0:?}")]
    Leased(ClientInfo),
    #[error("ip is reserved {0:?}")]
    Reserved(ClientInfo),
    #[error("ip is unreserved")]
    Unreserved,
    #[error("database error")]
    DbError(#[from] E),
    #[error("this address is already in use {0:?}")]
    AddrInUse(IpAddr),
    #[error("error getting next IP in range {range:?}")]
    RangeError { range: RangeInclusive<IpAddr> },
    #[error("error getting next IP in range {range:?} inside attempts {attempts:?}")]
    MaxAttempts {
        range: RangeInclusive<IpAddr>,
        attempts: usize,
    },
}

#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, SocketAddrV4};

    use super::*;
    use crate::sqlite::SqliteDb;
    use config::LeaseTime;
    use icmp_ping::{DEFAULT_TOKEN_SIZE, EchoReply};
    use rand::Rng;
    use tracing_test::traced_test;

    type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

    // get multiple first-available IPs in a range
    // this mimics what happens when multiple clients simultaneously 'DISCOVER'
    #[tokio::test]
    #[traced_test]
    async fn test_first_available() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()]);
        let client_id = &[1, 2, 3, 4, 5, 6];
        let expires_at = SystemTime::now() + Duration::from_secs(60);
        let ip = mgr
            .reserve_first(&range, &network, client_id, expires_at, None)
            .await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
        assert_eq!(
            mgr.lookup_id(client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))
        );

        let client_id = &[2, 2, 3, 4, 5, 6];
        let ip = mgr
            .reserve_first(&range, &network, client_id, expires_at, None)
            .await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101)));
        assert_eq!(
            mgr.lookup_id(client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101))
        );

        let client_id = &[3, 2, 3, 4, 5, 6];
        let ip = mgr
            .reserve_first(&range, &network, client_id, expires_at, None)
            .await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 102)));
        assert_eq!(
            mgr.lookup_id(client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 102))
        );

        let client_id = &[4, 2, 3, 4, 5, 6];
        let ip = mgr
            .reserve_first(&range, &network, client_id, expires_at, None)
            .await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 103)));
        assert_eq!(
            mgr.lookup_id(client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 103))
        );

        Ok(())
    }

    //
    #[tokio::test]
    #[traced_test]
    async fn test_reserve_first() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()]);
        let client_id = &[1, 2, 3, 4, 5, 6];
        let expires_at = SystemTime::now() + Duration::from_secs(1);
        let ip = mgr
            .reserve_first(&range, &network, client_id, expires_at, None)
            .await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
        assert_eq!(
            mgr.lookup_id(client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))
        );

        tokio::time::sleep(Duration::from_secs(2)).await;

        // try another range with the same client id-- should delete previous expired
        // entry
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 5, 100)..=Ipv4Addr::new(192, 168, 5, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.5.0/24".parse()?)
            .set_ranges(vec![range.clone()]);
        let client_id = &[1, 2, 3, 4, 5, 6];
        let expires_at = SystemTime::now() + Duration::from_secs(1);
        let ip = mgr
            .reserve_first(&range, &network, client_id, expires_at, None)
            .await?;

        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 5, 100)));
        assert_eq!(
            mgr.lookup_id(client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 5, 100))
        );

        Ok(())
    }

    // DISCOVER - ACK
    // get lease on discover like in a rapid commit response
    #[tokio::test]
    #[traced_test]
    async fn test_first_available_ack() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()]);
        let client_id = &[1, 2, 3, 4, 5, 6];
        let expires_at = SystemTime::now() + Duration::from_secs(60);
        // go straight to lease
        let ip = mgr
            .reserve_first(
                &range,
                &network,
                client_id,
                expires_at,
                Some(IpState::Lease),
            )
            .await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
        assert_eq!(
            mgr.lookup_id(client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))
        );

        Ok(())
    }

    // do reserve and lease in 2 steps like usual
    #[tokio::test]
    #[traced_test]
    async fn test_lease() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()]);
        let client_id = &[1, 2, 3, 4, 5, 6];
        let expires_at = SystemTime::now() + Duration::from_secs(5);
        // reserve from range
        let ip = mgr
            .reserve_first(&range, &network, client_id, expires_at, None)
            .await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));

        // make lease
        mgr.try_lease([192, 168, 1, 100].into(), client_id, expires_at, &network)
            .await?;
        let ip = mgr.lookup_id(client_id).await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));

        Ok(())
    }

    // do reserve and lease in 2 steps like usual
    #[tokio::test]
    #[traced_test]
    async fn test_lease_authoritative() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()])
            .set_authoritative(true);
        let client_id = &[1, 2, 3, 4, 5, 6];
        let expires_at = SystemTime::now() + Duration::from_secs(1);
        // reserve from range, expires in 1s
        let ip = mgr
            .reserve_first(&range, &network, client_id, expires_at, None)
            .await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));

        // different client makes a lease using just a REQUEST
        let client_id = &[1, 2, 3, 4, 5, 7];
        mgr.try_lease(
            [192, 168, 1, 101].into(),
            client_id,
            SystemTime::now() + Duration::from_secs(5),
            &network,
        )
        .await?;
        let ip = mgr.lookup_id(client_id).await?;
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101)));

        tokio::time::sleep(Duration::from_secs(2)).await;

        // client 1's reserve expired, reserve it again
        let client_id = &[1, 2, 3, 4, 5, 8];
        let ip = mgr
            .reserve_first(&range, &network, client_id, expires_at, None)
            .await?;
        // ip 100 available now since client 1 never claimed it
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));

        Ok(())
    }

    // reserve 2 ips then ack them both
    #[tokio::test]
    #[traced_test]
    async fn test_multiple_ranges() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range_a = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let range_b = NetRange::new(
            Ipv4Addr::new(10, 10, 1, 100)..=Ipv4Addr::new(10, 10, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network_a = Network::default();
        network_a
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range_a.clone()]);
        let mut network_b = Network::default();
        network_b
            .set_subnet("10.10.1.0/24".parse()?)
            .set_ranges(vec![range_b.clone()]);
        // reserve from range a
        {
            let client_id = &[1, 2, 3, 4, 5, 6];
            let expires_at = SystemTime::now() + Duration::from_secs(5);
            let ip = mgr
                .reserve_first(&range_a, &network_a, client_id, expires_at, None)
                .await?;
            assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)));
        }
        // reserve from range b
        {
            let client_id = &[2, 2, 3, 4, 5, 6];
            let expires_at = SystemTime::now() + Duration::from_secs(5);
            let ip = mgr
                .reserve_first(&range_b, &network_b, client_id, expires_at, None)
                .await?;
            assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(10, 10, 1, 100)));
        }
        mgr.try_lease(
            [192, 168, 1, 100].into(),
            &[1, 2, 3, 4, 5, 6],
            SystemTime::now() + Duration::from_secs(60),
            &network_a,
        )
        .await?;
        assert_eq!(
            mgr.lookup_id(&[1, 2, 3, 4, 5, 6]).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))
        );
        // make other range lease
        mgr.try_lease(
            [10, 10, 1, 100].into(),
            &[2, 2, 3, 4, 5, 6],
            SystemTime::now() + Duration::from_secs(60),
            &network_b,
        )
        .await?;
        assert_eq!(
            mgr.lookup_id(&[2, 2, 3, 4, 5, 6]).await?,
            IpAddr::V4(Ipv4Addr::new(10, 10, 1, 100))
        );

        Ok(())
    }

    // programmatically fill a range
    #[tokio::test]
    #[traced_test]
    async fn test_fill_range() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()]);

        // fill up range with new clients
        for range_ip in range.iter() {
            let client_id = (1..6)
                .map(|_| rand::thread_rng().r#gen())
                .collect::<Vec<u8>>();
            let expires_at = SystemTime::now() + Duration::from_secs(60);
            let ip = mgr
                .reserve_first(&range, &network, &client_id, expires_at, None)
                .await?;
            assert_eq!(range_ip, ip);
            assert_eq!(mgr.lookup_id(&client_id).await?, range_ip);
        }

        // range is empty, should error
        let expires_at = SystemTime::now() + Duration::from_secs(60);
        let ip = mgr
            .reserve_first(&range, &network, &[2, 3, 4, 6, 6], expires_at, None)
            .await;
        assert!(ip.is_err());

        Ok(())
    }

    // test RELEASE
    #[tokio::test]
    #[traced_test]
    async fn test_release_ip() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()]);

        // lease an IP
        let client_id = (1..6)
            .map(|_| rand::thread_rng().r#gen())
            .collect::<Vec<u8>>();
        let expires_at = SystemTime::now() + Duration::from_secs(60);
        let ip = mgr
            .reserve_first(
                &range,
                &network,
                &client_id,
                expires_at,
                Some(IpState::Lease),
            )
            .await?;
        assert_eq!(mgr.lookup_id(&client_id).await?, ip);

        // release IP
        let info = mgr.release_ip(ip, &client_id).await?;
        assert_eq!(
            info.unwrap().ip,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))
        );

        // try a new client, should get the same IP
        let client_id = (1..6)
            .map(|_| rand::thread_rng().r#gen())
            .collect::<Vec<u8>>();
        let expires_at = SystemTime::now() + Duration::from_secs(60);
        let _ip = mgr
            .reserve_first(
                &range,
                &network,
                &client_id,
                expires_at,
                Some(IpState::Lease),
            )
            .await?;
        assert_eq!(
            mgr.lookup_id(&client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100))
        );
        Ok(())
    }

    // test DECLINE
    #[tokio::test]
    #[traced_test]
    async fn test_probate_ip() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()]);

        // lease an IP
        let client_id = (1..6)
            .map(|_| rand::thread_rng().r#gen())
            .collect::<Vec<u8>>();
        let expires_at = SystemTime::now() + Duration::from_secs(60);
        let ip = mgr
            .reserve_first(
                &range,
                &network,
                &client_id,
                expires_at,
                Some(IpState::Lease),
            )
            .await?;
        assert_eq!(mgr.lookup_id(&client_id).await?, ip);

        // probate IP
        mgr.probate_ip(ip, &client_id, SystemTime::now() + Duration::from_secs(180))
            .await?;
        assert!(mgr.lookup_id(&client_id).await.is_err());

        // try a new client, should skip probated IP
        let client_id = (1..6)
            .map(|_| rand::thread_rng().r#gen())
            .collect::<Vec<u8>>();
        let expires_at = SystemTime::now() + Duration::from_secs(60);
        let _ip = mgr
            .reserve_first(
                &range,
                &network,
                &client_id,
                expires_at,
                Some(IpState::Lease),
            )
            .await?;
        assert_eq!(
            mgr.lookup_id(&client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101))
        );
        Ok(())
    }

    // test ping failure
    #[tokio::test]
    #[traced_test]
    async fn test_ping_fail() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()])
            .set_ping_check(true);
        // insert dummy entry into ping cache
        let ip = Ipv4Addr::new(192, 168, 1, 100);
        mgr.ping_insert(
            ip.into(),
            Some(PingReply {
                reply: EchoReply {
                    ident: 1,
                    seq_cnt: 1,
                    payload: [0; DEFAULT_TOKEN_SIZE],
                },
                addr: SocketAddr::V4(SocketAddrV4::new(ip, 100)),
                time: Duration::from_secs(60),
            }),
        )
        .await;
        // lease an IP
        let client_id = (1..6)
            .map(|_| rand::thread_rng().r#gen())
            .collect::<Vec<u8>>();
        let expires_at = SystemTime::now() + Duration::from_secs(60);
        let _ip = mgr
            .reserve_first(
                &range,
                &network,
                &client_id,
                expires_at,
                Some(IpState::Lease),
            )
            .await?;
        assert_eq!(
            mgr.lookup_id(&client_id).await?,
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 101))
        );
        Ok(())
    }

    // test bad lookup
    #[tokio::test]
    #[traced_test]
    async fn test_bad_lookup() -> Result<()> {
        let mgr = IpManager::new(SqliteDb::new("sqlite::memory:").await?)?;
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 1, 100)..=Ipv4Addr::new(192, 168, 1, 255),
            LeaseTime::new(
                Duration::from_secs(5),
                Duration::from_secs(3),
                Duration::from_secs(10),
            ),
        );
        let mut network = Network::default();
        network
            .set_subnet("192.168.1.0/24".parse()?)
            .set_ranges(vec![range.clone()])
            .set_ping_check(true);

        // lease an IP
        let client_id = (1..6)
            .map(|_| rand::thread_rng().r#gen())
            .collect::<Vec<u8>>();

        assert!(mgr.lookup_id(&client_id).await.is_err());
        Ok(())
    }
}

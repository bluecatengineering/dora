use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use config::v4::NetRangeIter;
use thiserror::Error;
use tracing::debug;

use crate::{ClientInfo, IpState, State, Storage};

#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    inner: Arc<Mutex<BTreeMap<IpAddr, MemoryEntry>>>,
}

#[derive(Debug, Clone)]
struct MemoryEntry {
    client_id: Option<Vec<u8>>,
    network: IpAddr,
    expires_at: SystemTime,
    leased: bool,
    probation: bool,
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("address already exists in memory store: {0}")]
    AddressExists(IpAddr),
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn state_flags(state: Option<IpState>) -> (bool, bool) {
    state.unwrap_or(IpState::Reserve).into()
}

fn to_client_info(ip: IpAddr, entry: &MemoryEntry) -> ClientInfo {
    ClientInfo {
        ip,
        id: entry.client_id.clone(),
        network: entry.network,
        expires_at: entry.expires_at,
    }
}

fn to_state(ip: IpAddr, entry: &MemoryEntry) -> State {
    let info = to_client_info(ip, entry);
    if entry.leased {
        State::Leased(info)
    } else if entry.probation {
        State::Probated(info)
    } else {
        State::Reserved(info)
    }
}

fn next_v4_ip(start: Ipv4Addr, end: Ipv4Addr, exclusions: &HashSet<Ipv4Addr>) -> Option<IpAddr> {
    NetRangeIter::new(ipnet::Ipv4AddrRange::new(start, end), exclusions)
        .nth(1)
        .map(IpAddr::V4)
}

#[async_trait]
impl Storage for MemoryStore {
    type Error = MemoryError;

    async fn update_expired(
        &self,
        ip: IpAddr,
        state: Option<IpState>,
        id: &[u8],
        expires_at: SystemTime,
    ) -> Result<bool, Self::Error> {
        let mut guard = self.inner.lock().expect("memory store lock poisoned");
        let now = SystemTime::now();
        let (leased, probation) = state_flags(state);

        if let Some(entry) = guard.get_mut(&ip)
            && (entry.client_id.as_deref() == Some(id) || entry.expires_at < now)
        {
            entry.client_id = Some(id.to_vec());
            entry.expires_at = expires_at;
            entry.leased = leased;
            entry.probation = probation;
            return Ok(true);
        }

        Ok(false)
    }

    async fn insert(
        &self,
        ip: IpAddr,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<(), Self::Error> {
        let mut guard = self.inner.lock().expect("memory store lock poisoned");
        if guard.contains_key(&ip) {
            return Err(MemoryError::AddressExists(ip));
        }

        let (leased, probation) = state_flags(state);
        guard.insert(
            ip,
            MemoryEntry {
                client_id: Some(id.to_vec()),
                network,
                expires_at,
                leased,
                probation,
            },
        );
        Ok(())
    }

    async fn get(&self, ip: IpAddr) -> Result<Option<State>, Self::Error> {
        let guard = self.inner.lock().expect("memory store lock poisoned");
        Ok(guard.get(&ip).map(|entry| to_state(ip, entry)))
    }

    async fn get_id(&self, id: &[u8]) -> Result<Option<IpAddr>, Self::Error> {
        let guard = self.inner.lock().expect("memory store lock poisoned");
        let now = SystemTime::now();
        Ok(guard.iter().find_map(|(ip, entry)| {
            if entry.client_id.as_deref() == Some(id) && entry.expires_at > now {
                Some(*ip)
            } else {
                None
            }
        }))
    }

    async fn select_all(&self) -> Result<Vec<State>, Self::Error> {
        let guard = self.inner.lock().expect("memory store lock poisoned");
        Ok(guard
            .iter()
            .map(|(ip, entry)| to_state(*ip, entry))
            .collect())
    }

    async fn release_ip(&self, ip: IpAddr, id: &[u8]) -> Result<Option<ClientInfo>, Self::Error> {
        let mut guard = self.inner.lock().expect("memory store lock poisoned");
        let matched = guard.get(&ip).and_then(|entry| {
            if entry.client_id.as_deref() == Some(id) {
                Some(to_client_info(ip, entry))
            } else {
                None
            }
        });
        guard.remove(&ip);
        Ok(matched)
    }

    async fn delete(&self, ip: IpAddr) -> Result<(), Self::Error> {
        let mut guard = self.inner.lock().expect("memory store lock poisoned");
        guard.remove(&ip);
        Ok(())
    }

    async fn next_expired(
        &self,
        range: RangeInclusive<IpAddr>,
        _network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<Option<IpAddr>, Self::Error> {
        let mut guard = self.inner.lock().expect("memory store lock poisoned");
        let now = SystemTime::now();
        let (leased, _probation) = state_flags(state);

        let selected_ip = guard.iter().find_map(|(ip, entry)| {
            let id_match = entry.client_id.as_deref() == Some(id);
            let expired_in_range = entry.expires_at < now && range.contains(ip);
            if id_match || expired_in_range {
                Some(*ip)
            } else {
                None
            }
        });

        if let Some(selected_ip) = selected_ip
            && let Some(entry) = guard.get_mut(&selected_ip)
        {
            entry.client_id = Some(id.to_vec());
            entry.expires_at = expires_at;
            entry.leased = leased;
            entry.probation = false;
            return Ok(Some(selected_ip));
        }

        Ok(None)
    }

    async fn insert_max_in_range(
        &self,
        range: RangeInclusive<IpAddr>,
        exclusions: &HashSet<Ipv4Addr>,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<Option<IpAddr>, Self::Error> {
        let (start, end) = (*range.start(), *range.end());
        let (start, end, network) = match (start, end, network) {
            (IpAddr::V4(start), IpAddr::V4(end), IpAddr::V4(network)) => (start, end, network),
            _ => panic!("ipv6 not yet implemented"),
        };

        let mut guard = self.inner.lock().expect("memory store lock poisoned");
        debug!("no expired entries, finding start of range");

        let max_ip = guard
            .range(IpAddr::V4(start)..=IpAddr::V4(end))
            .next_back()
            .map(|(ip, _)| *ip);

        let candidate = match max_ip {
            Some(IpAddr::V4(current)) => {
                debug!(start = ?current, "get next IP starting from");
                next_v4_ip(current, end, exclusions)
            }
            None => {
                debug!(start = ?range.start(), "using start of range");
                Some(IpAddr::V4(start))
            }
            _ => None,
        };

        let Some(candidate) = candidate else {
            debug!("unable to find start of range");
            return Ok(None);
        };

        if guard.contains_key(&candidate) {
            return Err(MemoryError::AddressExists(candidate));
        }

        let (leased, probation) = state_flags(state);
        guard.insert(
            candidate,
            MemoryEntry {
                client_id: Some(id.to_vec()),
                network: IpAddr::V4(network),
                expires_at,
                leased,
                probation,
            },
        );

        Ok(Some(candidate))
    }

    async fn update_unexpired(
        &self,
        ip: IpAddr,
        state: IpState,
        id: &[u8],
        expires_at: SystemTime,
        new_id: Option<&[u8]>,
    ) -> Result<Option<IpAddr>, Self::Error> {
        let mut guard = self.inner.lock().expect("memory store lock poisoned");
        let now = SystemTime::now();
        let (leased, probation) = state.into();

        if let Some(entry) = guard.get_mut(&ip)
            && entry.expires_at > now
            && entry.client_id.as_deref() == Some(id)
        {
            entry.leased = leased;
            entry.probation = probation;
            entry.expires_at = expires_at;
            entry.client_id = new_id.map(<[u8]>::to_vec);
            return Ok(Some(ip));
        }

        Ok(None)
    }

    async fn update_ip(
        &self,
        ip: IpAddr,
        state: IpState,
        id: Option<&[u8]>,
        expires_at: SystemTime,
    ) -> Result<Option<State>, Self::Error> {
        let mut guard = self.inner.lock().expect("memory store lock poisoned");
        let (leased, probation) = state.into();

        if let Some(entry) = guard.get_mut(&ip) {
            entry.client_id = id.map(<[u8]>::to_vec);
            entry.expires_at = expires_at;
            entry.leased = leased;
            entry.probation = probation;
            return Ok(Some(to_state(ip, entry)));
        }

        Ok(None)
    }

    async fn count(&self, state: IpState) -> Result<usize, Self::Error> {
        let guard = self.inner.lock().expect("memory store lock poisoned");
        let now = SystemTime::now();
        let (leased, probation) = state.into();
        Ok(guard
            .values()
            .filter(|entry| {
                entry.leased == leased && entry.probation == probation && entry.expires_at > now
            })
            .count())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::{Duration, SystemTime};

    use super::MemoryStore;
    use crate::{IpState, State, Storage};

    #[tokio::test]
    async fn insert_max_in_range_allocates_sequential_ips() {
        let store = MemoryStore::new();
        let range =
            IpAddr::V4(Ipv4Addr::new(192, 168, 2, 50))..=IpAddr::V4(Ipv4Addr::new(192, 168, 2, 52));
        let subnet = IpAddr::V4(Ipv4Addr::new(192, 168, 2, 0));
        let expires = SystemTime::now() + Duration::from_secs(60);

        let first = store
            .insert_max_in_range(range.clone(), &HashSet::new(), subnet, &[1], expires, None)
            .await
            .expect("first insert")
            .expect("first address");
        let second = store
            .insert_max_in_range(range.clone(), &HashSet::new(), subnet, &[2], expires, None)
            .await
            .expect("second insert")
            .expect("second address");

        assert_eq!(first, IpAddr::V4(Ipv4Addr::new(192, 168, 2, 50)));
        assert_eq!(second, IpAddr::V4(Ipv4Addr::new(192, 168, 2, 51)));
    }

    #[tokio::test]
    async fn next_expired_reuses_expired_entry() {
        let store = MemoryStore::new();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 2, 60));
        let subnet = IpAddr::V4(Ipv4Addr::new(192, 168, 2, 0));

        store
            .insert(
                ip,
                subnet,
                &[9],
                SystemTime::now() - Duration::from_secs(1),
                Some(IpState::Reserve),
            )
            .await
            .expect("seed expired entry");

        let reassigned = store
            .next_expired(
                ip..=ip,
                subnet,
                &[7],
                SystemTime::now() + Duration::from_secs(30),
                Some(IpState::Lease),
            )
            .await
            .expect("next expired query")
            .expect("reassigned ip");

        assert_eq!(reassigned, ip);

        let state = store
            .get(ip)
            .await
            .expect("state lookup")
            .expect("entry exists");
        match state {
            State::Leased(info) => assert_eq!(info.id(), Some(&[7][..])),
            other => panic!("unexpected state after reassignment: {other:?}"),
        }
    }

    #[tokio::test]
    async fn release_deletes_entry_even_if_id_mismatch() {
        let store = MemoryStore::new();
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 2, 70));
        let subnet = IpAddr::V4(Ipv4Addr::new(192, 168, 2, 0));

        store
            .insert(
                ip,
                subnet,
                &[1, 2, 3],
                SystemTime::now() + Duration::from_secs(60),
                None,
            )
            .await
            .expect("seed entry");

        let released = store
            .release_ip(ip, &[9, 9, 9])
            .await
            .expect("release operation");
        assert!(released.is_none());

        let remaining = store.get(ip).await.expect("post-release lookup");
        assert!(remaining.is_none());
    }
}

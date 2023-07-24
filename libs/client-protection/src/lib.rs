//! Client protection
//!
//!
use config::v4::FloodThreshold;
// TODO: consider switching both to Mutex<Hashmap<>>.
// the caches are all locked immediately and written to, so dashmap is probably overkill
// (governor uses dashmap internally by default by we can turn off the "dashmap" feature)
use dashmap::DashMap;
use governor::{clock::DefaultClock, state::keyed::DefaultKeyedStateStore, Quota, RateLimiter};
use tracing::{debug, trace};

use std::{
    borrow::Borrow,
    fmt,
    hash::Hash,
    num::NonZeroU32,
    time::{Duration, Instant},
};

pub struct RenewThreshold<K> {
    percentage: u64,
    cache: DashMap<K, RenewExpiry>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RenewExpiry {
    // when entry was created
    pub created: Instant,
    // % * lease_time
    pub percentage: Duration,
    // full lease time
    pub lease_time: Duration,
}

impl RenewExpiry {
    /// if the elapsed time is less than the fraction of lease time configured
    /// return the lease time remaining
    pub fn get_remaining(&self) -> Option<Duration> {
        if self.created.elapsed() <= self.percentage {
            Some(self.lease_time - self.created.elapsed())
        } else {
            None
        }
    }
}

impl RenewExpiry {
    pub fn new(now: Instant, lease_time: Duration, percentage: u64) -> Self {
        Self {
            percentage: Duration::from_secs((lease_time.as_secs() * percentage) / 100),
            created: now,
            lease_time,
        }
    }
}

impl<K: Eq + Hash + Clone> RenewThreshold<K> {
    pub fn new(percentage: u32) -> Self {
        Self {
            percentage: percentage as u64,
            cache: DashMap::new(),
        }
    }
    // insert id into cache with lease time, replacing existing entry
    pub fn insert(&self, id: K, lease_time: Duration) -> Option<RenewExpiry> {
        let now = Instant::now();
        self.cache
            .insert(id, RenewExpiry::new(now, lease_time, self.percentage))
    }
    // test if threshold has been met for a given id
    pub fn threshold<Q>(&self, id: &Q) -> Option<Duration>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.cache
            .get(id)
            .map(|e| *e)
            .and_then(|entry| entry.get_remaining())
    }
    pub fn remove(&self, id: &K) -> Option<(K, RenewExpiry)> {
        self.cache.remove(id)
    }
}

pub struct FloodCache<K: Hash + Eq + Clone> {
    rl: RateLimiter<K, DefaultKeyedStateStore<K>, DefaultClock>,
}

impl<K> FloodCache<K>
where
    K: Eq + Hash + Clone + fmt::Debug,
{
    pub fn new(cfg: FloodThreshold) -> Self {
        debug!(
            packets = cfg.packets(),
            period = cfg.period().as_secs(),
            "creating flood cache with following settings"
        );
        // let rate = cfg.packets() / cfg.period().as_secs() as u32;
        // debug!("creating flood cache threshold {:?} packets/sec", rate);

        Self {
            #[allow(deprecated)]
            rl: RateLimiter::keyed(
                Quota::new(
                    NonZeroU32::new(cfg.packets()).expect("conversion will not fail"),
                    cfg.period(),
                )
                .expect("don't pass Duration of 0"),
            ),
        }
    }
    pub fn is_allowed(&self, id: &K) -> bool {
        let res = self.rl.check_key(id);
        if let Err(not_until) = &res {
            trace!(?not_until, ?id, "reached threshold for client")
        }
        res.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flood_threshold_packets() {
        let cache = FloodCache::new(FloodThreshold::new(2, Duration::from_secs(1)));
        assert!(cache.is_allowed(&[1, 2, 3, 4]));
        assert!(cache.is_allowed(&[1, 2, 3, 4]));

        // too many packets
        assert!(!cache.is_allowed(&[1, 2, 3, 4]));

        // wait for duration
        std::thread::sleep(Duration::from_millis(1_100));
        // should be true now
        assert!(cache.is_allowed(&[1, 2, 3, 4]));
        assert!(cache.is_allowed(&[1, 2, 3, 4]));
    }

    #[test]
    fn test_flood_threshold_large_period() {
        let cache = FloodCache::new(FloodThreshold::new(2, Duration::from_secs(5)));
        assert!(cache.is_allowed(&[1, 2, 3, 4]));
        assert!(cache.is_allowed(&[1, 2, 3, 4]));

        // // too many packets
        // assert!(!cache.is_allowed(&[1, 2, 3, 4]));

        // // wait for duration
        // std::thread::sleep(Duration::from_millis(1_100));
        // // should be true now
        // assert!(cache.is_allowed(&[1, 2, 3, 4]));
        // assert!(cache.is_allowed(&[1, 2, 3, 4]));
    }

    #[test]
    fn test_flood_threshold_multi() {
        let cache = FloodCache::new(FloodThreshold::new(2, Duration::from_secs(1)));
        assert!(cache.is_allowed(&[1, 2, 3, 4]));
        assert!(cache.is_allowed(&[1, 2, 3, 4]));
        assert!(!cache.is_allowed(&[1, 2, 3, 4]));

        // another client, independent threshold
        assert!(cache.is_allowed(&[4, 3, 2, 1]));
        assert!(cache.is_allowed(&[4, 3, 2, 1]));
        assert!(!cache.is_allowed(&[4, 3, 2, 1]));
    }

    #[test]
    fn test_renew_remaining() {
        let renew = RenewExpiry::new(Instant::now(), Duration::from_secs(5), 50);
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(
            renew
                .get_remaining()
                .unwrap()
                .as_secs_f32()
                // round up
                .round(),
            4.
        );
        std::thread::sleep(Duration::from_secs(5));
        assert!(renew.get_remaining().is_none());
    }

    #[test]
    fn test_cache_threshold() {
        let cache = RenewThreshold::new(50);
        let lease_time = Duration::from_secs(2);
        let lease_time_b = Duration::from_secs(6);
        assert!(cache.insert([1, 2, 3, 4], lease_time).is_none());

        // another client, independent threshold
        assert!(cache.insert([4, 3, 2, 1], lease_time_b).is_none());

        // half of lease time passes
        std::thread::sleep(Duration::from_secs(1));

        assert!(cache.threshold(&[1, 2, 3, 4]).is_none());
        assert!(cache.threshold(&[1, 2, 3, 4]).is_none());
        assert_eq!(
            cache
                .threshold(&[4, 3, 2, 1])
                .unwrap()
                .as_secs_f32()
                .round(),
            5.
        );

        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(
            cache
                .threshold(&[4, 3, 2, 1])
                .unwrap()
                .as_secs_f32()
                .round(),
            4.
        );

        std::thread::sleep(Duration::from_secs(2));
        assert!(cache.threshold(&[4, 3, 2, 1]).is_none());
    }

    #[test]
    fn test_cache_renew_0() {
        // threshold set to 0 means the cache will never return a cached lease
        let cache = RenewThreshold::new(0);
        let lease_time = Duration::from_secs(2);
        let lease_time_b = Duration::from_secs(6);
        assert!(cache.insert([1, 2, 3, 4], lease_time).is_none());

        // another client, independent threshold
        assert!(cache.insert([4, 3, 2, 1], lease_time_b).is_none());

        // half of lease time passes
        std::thread::sleep(Duration::from_secs(1));

        assert!(cache.threshold(&[1, 2, 3, 4]).is_none());
        assert!(cache.threshold(&[4, 3, 2, 1]).is_none());
        std::thread::sleep(Duration::from_secs(3));
        assert!(cache.threshold(&[4, 3, 2, 1]).is_none());
    }
}

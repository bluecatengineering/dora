//! Client protection
//!
//!
use config::v4::FloodThreshold;
// TODO: consider switching both to Mutex<Hashmap<>>.
// the caches are all locked immediately and written to, so dashmap is probably overkill
// (governor uses dashmap internally by default by we can turn off the "dashmap" feature)
use dashmap::DashMap;
use governor::{clock::DefaultClock, state::keyed::DefaultKeyedStateStore, Quota, RateLimiter};
use tracing::debug;

use std::{
    borrow::Borrow,
    fmt,
    hash::Hash,
    num::NonZeroU32,
    time::{Duration, Instant},
};

pub struct RenewThreshold<K> {
    percentage: u64,
    cache: DashMap<K, Instant>,
}

fn threshold_expiry(now: Instant, lease_time: Duration, percentage: u64) -> Instant {
    // cant use self.percentage b/c of borrowck error on `or_insert_with`
    now + Duration::from_secs((lease_time.as_secs() * percentage) / 100)
}

impl<K: Eq + Hash + Clone> RenewThreshold<K> {
    pub fn new(percentage: u32) -> Self {
        Self {
            percentage: percentage as u64,
            cache: DashMap::new(),
        }
    }
    // insert id into cache with lease time, replacing existing entry
    pub fn insert(&self, id: K, lease_time: Duration) -> Option<Instant> {
        let now = Instant::now();
        self.cache
            .insert(id, threshold_expiry(now, lease_time, self.percentage))
    }
    // test if threshold has been met for a given id
    pub fn threshold<Q>(&self, id: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        // 0% means the threshold is always met
        if self.percentage == 0 {
            return true;
        }
        let now = Instant::now();
        self.cache
            .get(id)
            .map(|expires| now < *expires)
            .unwrap_or(false)
    }
    // test if threshold has been met for a given id, inserting it if it does not exist
    pub fn allowed_insert(&self, id: K, lease_time: Duration) -> bool {
        // 0% means the threshold is always met
        if self.percentage == 0 {
            return true;
        }
        let now = Instant::now();
        let expires = self.cache.entry(id).or_insert_with(|| {
            // get the ending of the entry
            threshold_expiry(now, lease_time, self.percentage)
        });
        now < *expires
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
            debug!(?not_until, ?id, "reached threshold for client")
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
    fn test_renew_threshold() {
        let cache = RenewThreshold::new(50);
        let lease_time = Duration::from_secs(2);
        let lease_time_b = Duration::from_secs(5);
        assert!(cache.allowed_insert([1, 2, 3, 4], lease_time));
        assert!(cache.allowed_insert([1, 2, 3, 4], lease_time));

        // another client, independent threshold
        assert!(cache.allowed_insert([4, 3, 2, 1], lease_time_b));
        assert!(cache.allowed_insert([4, 3, 2, 1], lease_time_b));
        assert!(cache.allowed_insert([4, 3, 2, 1], lease_time_b));

        // half of lease time passes
        std::thread::sleep(Duration::from_secs(1));

        assert!(!cache.threshold(&[1, 2, 3, 4]));
        assert!(!cache.threshold(&[1, 2, 3, 4]));
        assert!(!cache.threshold(&[1, 2, 3, 4]));

        assert!(cache.allowed_insert([4, 3, 2, 1], lease_time_b));
        // half of lease time passes
        std::thread::sleep(Duration::from_secs(3));
        assert!(!cache.allowed_insert([4, 3, 2, 1], lease_time_b));
    }
}

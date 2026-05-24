#![allow(clippy::type_complexity, clippy::type_repetition_in_bounds)]

use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
    ops::RangeInclusive,
    time::SystemTime,
};

use async_trait::async_trait;

use crate::{ClientInfo, IpState, State, Storage};

pub struct MemDb {}

impl MemDb {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for MemDb {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("key not found {key_name:?}")]
    KeyNotFound { key_name: String },
}

#[async_trait]
impl Storage for MemDb {
    type Error = self::Error;

    /// find the next expired IP in the range, or where client_id matches,
    /// and update it with the new client_id & expiry & state
    /// NOTE: always sets probation = false
    async fn next_expired(
        &self,
        range: RangeInclusive<IpAddr>,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<Option<IpAddr>, Self::Error> {
        todo!()
    }

    /// find next available IP in the range and insert an entry for this id
    async fn insert_max_in_range(
        &self,
        range: RangeInclusive<IpAddr>,
        // TODO should not mix Ip and Ipv4 in args
        exclusions: &HashSet<Ipv4Addr>,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<Option<IpAddr>, Self::Error> {
        todo!()
    }

    async fn update_expired(
        &self,
        ip: IpAddr,
        state: Option<IpState>,
        id: &[u8],
        expires_at: SystemTime,
    ) -> Result<bool, Self::Error> {
        todo!()
    }

    async fn update_unexpired(
        &self,
        ip: IpAddr,
        state: IpState,
        id: &[u8],
        expires_at: SystemTime,
        new_id: Option<&[u8]>,
    ) -> Result<Option<IpAddr>, Self::Error> {
        todo!()
    }

    async fn update_ip(
        &self,
        ip: IpAddr,
        state: IpState,
        id: Option<&[u8]>,
        expires_at: SystemTime,
    ) -> Result<Option<State>, Self::Error> {
        todo!()
    }

    async fn insert(
        &self,
        ip: IpAddr,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<(), Self::Error> {
        todo!()
    }

    async fn get(&self, ip: IpAddr) -> Result<Option<State>, Self::Error> {
        todo!()
    }

    async fn get_id(&self, id: &[u8]) -> Result<Option<IpAddr>, Self::Error> {
        todo!()
    }

    async fn release_ip(&self, ip: IpAddr, id: &[u8]) -> Result<Option<ClientInfo>, Self::Error> {
        todo!()
    }

    async fn delete(&self, ip: IpAddr) -> Result<(), Self::Error> {
        todo!()
    }
    async fn count(&self, state: IpState) -> Result<usize, Self::Error> {
        todo!()
    }

    async fn select_all(&self) -> Result<Vec<State>, Self::Error> {
        todo!()
    }
}

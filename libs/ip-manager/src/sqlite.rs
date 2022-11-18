use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
    ops::RangeInclusive,
    str::FromStr,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePool},
    ConnectOptions, Sqlite,
};
use tracing::debug;

use crate::{ClientInfo, IpState, State, Storage};

#[derive(Debug)]
pub struct SqliteDb {
    inner: SqlitePool,
}

impl Clone for SqliteDb {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl SqliteDb {
    pub async fn new(uri: impl AsRef<str>) -> Result<Self, sqlx::Error> {
        // in memory sqlite will clear the db after all conns close,
        // to keep it alive for testing-- we can make sure conns _never_ close.
        //
        // let inner = if uri.as_ref().contains("memory") {
        //     sqlx::sqlite::SqlitePoolOptions::new()
        //         .idle_timeout(None)
        //         .max_lifetime(None)
        //         .connect(uri.as_ref())
        //         .await?
        // } else {
        let mut opts = SqliteConnectOptions::from_str(uri.as_ref())?
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .create_if_missing(true);
        // make sqlite log queries at trace level so we don't get a bloated log on `info`
        opts.log_statements(tracing::log::LevelFilter::Trace);

        let inner = SqlitePool::connect_with(opts).await?;
        // };
        sqlx::migrate!("../../migrations").run(&inner).await?;
        Ok(Self { inner })
    }
}

#[async_trait]
impl Storage for SqliteDb {
    // TODO: consider alternate error type
    type Error = sqlx::Error;

    /// find the next expired IP in the range, or where client_id matches,
    /// and update it to leased = false with the new client_id & expiry
    async fn next_expired(
        &self,
        range: RangeInclusive<IpAddr>,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
    ) -> Result<Option<IpAddr>, Self::Error> {
        match (*range.start(), *range.end(), network) {
            (IpAddr::V4(start), IpAddr::V4(end), IpAddr::V4(_network)) => {
                let start_ip = u32::from(start) as i64;
                let end_ip = u32::from(end) as i64;
                let now = util::systime_epoch(SystemTime::now());

                Ok(util::update_next_expired(
                    &self.inner,
                    now,
                    id,
                    start_ip,
                    end_ip,
                    util::systime_epoch(expires_at),
                    false,
                )
                .await?)
            }
            _ => {
                panic!("ipv6 not yet implemented");
            }
        }
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
    ) -> Result<Option<IpAddr>, Self::Error> {
        // a different Error type here would let us remove Option
        // Option is currently doing work as the method to say "cant find an IP in the range",
        // this should probably be an error variant
        match (*range.start(), *range.end(), network) {
            (IpAddr::V4(start), IpAddr::V4(end), IpAddr::V4(network)) => {
                let start_ip = u32::from(start) as i64;
                let end_ip = u32::from(end) as i64;
                // allocation needed for future
                let id = id.to_vec();

                debug!("no expired entries, finding start of range");
                // TRANSACTION START
                let mut conn = self.inner.begin().await?;
                // we only use this IP to find what the next available should be
                let ip = match util::max_in_range(&mut conn, start_ip, end_ip).await? {
                    Some(State::Leased(cur) | State::Reserved(cur) | State::Probated(cur)) => {
                        let start = cur.ip;
                        let end = *range.end();
                        debug!(?start, "get next IP starting from");
                        util::inc_ip(start, end, exclusions)
                    }
                    None => {
                        debug!(start = ?range.start(), "using start of range");
                        // no IPs in range, so it must be empty
                        Some(*range.start())
                    }
                };
                if let Some(IpAddr::V4(v4_ip)) = ip {
                    util::insert(
                        &mut conn,
                        u32::from(v4_ip) as i64,
                        u32::from(network) as i64,
                        &id,
                        util::systime_epoch(expires_at),
                        None,
                    )
                    .await?;
                    // TRANSACTION COMMIT
                    conn.commit().await?;
                    Ok(ip)
                } else {
                    debug!("unable to find start of range");
                    // TRANSACTION ROLLBACK
                    conn.rollback().await?;
                    Ok(None)
                }
            }
            _ => {
                panic!("ipv6 not yet implemented");
            }
        }
    }

    async fn update_expired(
        &self,
        ip: IpAddr,
        state: IpState,
        id: &[u8],
        expires_at: SystemTime,
    ) -> Result<bool, Self::Error> {
        let (lease, probation) = state.into();
        match ip {
            IpAddr::V4(ip) => Ok(util::update_expired(
                &self.inner,
                u32::from(ip) as i64,
                id,
                util::systime_epoch(expires_at),
                util::systime_epoch(SystemTime::now()),
                lease,
                probation,
            )
            .await?
            .is_some()),
            _ => {
                panic!("ipv6 not yet implemented");
            }
        }
    }

    async fn update_unexpired(
        &self,
        ip: IpAddr,
        state: IpState,
        id: &[u8],
        expires_at: SystemTime,
        new_id: Option<&[u8]>,
    ) -> Result<Option<IpAddr>, Self::Error> {
        let (lease, probation) = state.into();
        match ip {
            IpAddr::V4(ip) => {
                util::update_unexpired(
                    &self.inner,
                    u32::from(ip) as i64,
                    id,
                    util::systime_epoch(expires_at),
                    util::systime_epoch(SystemTime::now()),
                    lease,
                    probation,
                    new_id,
                )
                .await
            }
            _ => {
                panic!("ipv6 not yet implemented");
            }
        }
    }

    async fn update_ip(
        &self,
        ip: IpAddr,
        state: IpState,
        id: Option<&[u8]>,
        expires_at: SystemTime,
    ) -> Result<Option<State>, Self::Error> {
        let (lease, probation) = state.into();
        match ip {
            IpAddr::V4(ip) => {
                util::update_ip(
                    &self.inner,
                    u32::from(ip) as i64,
                    id,
                    util::systime_epoch(expires_at),
                    lease,
                    probation,
                )
                .await
            }
            _ => {
                panic!("ipv6 not yet implemented");
            }
        }
    }

    async fn insert(
        &self,
        ip: IpAddr,
        network: IpAddr,
        id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> Result<(), Self::Error> {
        match (ip, network) {
            (IpAddr::V4(ip), IpAddr::V4(network)) => {
                let ip = u32::from(ip) as i64;
                let network = u32::from(network) as i64;
                let expires_at = util::systime_epoch(expires_at);
                let state = state.map(|s| s.into());
                util::insert(&self.inner, ip, network, id, expires_at, state).await
            }
            _ => {
                panic!("ipv6 not yet implemented");
            }
        }
    }

    async fn get(&self, ip: IpAddr) -> Result<Option<State>, Self::Error> {
        match ip {
            IpAddr::V4(ip) => {
                let ip = u32::from(ip) as i64;
                util::find(&self.inner, ip).await
            }
            IpAddr::V6(_ip) => {
                panic!("ipv6 not yet implemented");
            }
        }
    }

    async fn get_id(&self, id: &[u8]) -> Result<Option<IpAddr>, Self::Error> {
        util::find_by_id(&self.inner, id, util::systime_epoch(SystemTime::now())).await
    }

    async fn release_ip(&self, ip: IpAddr, id: &[u8]) -> Result<Option<ClientInfo>, Self::Error> {
        match ip {
            IpAddr::V4(ip) => {
                let ip = u32::from(ip) as i64;
                util::release_ip(&self.inner, ip, id).await
            }
            IpAddr::V6(_ip) => {
                panic!("ipv6 not yet implemented");
            }
        }
    }

    async fn delete(&self, ip: IpAddr) -> Result<(), Self::Error> {
        match ip {
            IpAddr::V4(ip) => {
                let ip = u32::from(ip) as i64;
                let mut conn = self.inner.begin().await?;
                util::delete(&mut conn, ip).await?;
                conn.commit().await?;
                Ok(())
            }
            IpAddr::V6(_ip) => {
                panic!("ipv6 not yet implemented");
            }
        }
    }
    async fn count(&self, state: IpState) -> Result<usize, Self::Error> {
        let (lease, probation) = state.into();
        util::count(
            &self.inner,
            lease,
            probation,
            util::systime_epoch(SystemTime::now()),
        )
        .await
    }
}

mod util {
    use std::net::Ipv4Addr;

    use config::v4::NetRangeIter;

    use crate::State;

    use super::*;
    pub fn systime_epoch(time: SystemTime) -> i64 {
        // / get secs as i64 (for use in sqlite) from epoch to `time`
        time.duration_since(SystemTime::UNIX_EPOCH)
            .expect("failed to get system time")
            .as_secs() as i64
    }

    pub fn to_systime(time: i64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(time as u64)
    }

    pub async fn delete<'a, E>(conn: E, ip: i64) -> Result<(), sqlx::Error>
    where
        E: sqlx::Executor<'a, Database = Sqlite>,
    {
        sqlx::query!("DELETE FROM leases WHERE ip = ?1", ip)
            .execute(conn)
            .await?;
        Ok(())
    }

    pub async fn release_ip(
        conn: &SqlitePool,
        ip: i64,
        id: &[u8],
    ) -> Result<Option<ClientInfo>, sqlx::Error> {
        let mut trans = conn.begin().await?;
        let cur = sqlx::query!(
            "SELECT * FROM leases WHERE ip = ?1 AND client_id = ?2",
            ip,
            id
        )
        .fetch_optional(&mut trans)
        .await?
        .map(|cur| ClientInfo {
            ip: IpAddr::V4(Ipv4Addr::from(cur.ip as u32)),
            id: cur.client_id.map(|v| v.to_vec()),
            network: IpAddr::V4(Ipv4Addr::from(cur.network as u32)),
            expires_at: to_systime(cur.expires_at),
        });
        util::delete(&mut trans, ip).await?;

        trans.commit().await?;
        // instead of deleting:
        // sqlx::query!(
        //     "UPDATE leases SET leased = false WHERE ip = ?1 AND client_id = ?2",
        //     ip,
        //     id
        // )
        // .fetch_optional(conn)
        // .await?;
        Ok(cur)
    }

    /// Inserts ip/network/client_id/expires_at into db.
    /// If state is Some, we will insert the leased/probation state too.
    /// if None then we use the default column type
    pub async fn insert<'a, E>(
        conn: E,
        ip: i64,
        network: i64,
        client_id: &[u8],
        expires_at: i64,
        state: Option<(bool, bool)>,
    ) -> Result<(), sqlx::Error>
    where
        E: sqlx::Executor<'a, Database = Sqlite>,
    {
        match state {
            Some((leased, probation)) => {
                sqlx::query!(
                    r#"INSERT INTO leases
                    (ip, client_id, expires_at, network, leased, probation)
                VALUES
                    (?1, ?2, ?3, ?4, ?5, ?6)"#,
                    ip,
                    client_id,
                    expires_at,
                    network,
                    leased,
                    probation
                )
                .execute(conn)
                .await?;
            }
            None => {
                sqlx::query!(
                "INSERT INTO leases (ip, client_id, expires_at, network) VALUES (?1, ?2, ?3, ?4)",
                ip,
                client_id,
                expires_at,
                network,
            )
                .execute(conn)
                .await?;
            }
        }
        Ok(())
    }

    pub async fn find(pool: &SqlitePool, ip: i64) -> Result<Option<State>, sqlx::Error> {
        Ok(sqlx::query!("SELECT * FROM leases WHERE ip = ?1", ip)
            .fetch_optional(pool)
            .await?
            .map(|cur| {
                let info = ClientInfo {
                    ip: IpAddr::V4(Ipv4Addr::from(cur.ip as u32)),
                    id: cur.client_id.map(|v| v.to_vec()),
                    network: IpAddr::V4(Ipv4Addr::from(cur.network as u32)),
                    expires_at: to_systime(cur.expires_at),
                };
                into_clientinfo(info, cur.leased, cur.probation)
            }))
    }

    /// return a count of all rows where leased & probation & un-expired
    pub async fn count(
        pool: &SqlitePool,
        leased: bool,
        probation: bool,
        expires_at: i64,
    ) -> Result<usize, sqlx::Error> {
        Ok(sqlx::query_scalar!(
            "SELECT COUNT(ip) as count_ip FROM leases WHERE leased = ?1 AND probation = ?2 AND expires_at > ?3",
            leased,
            probation,
            expires_at
        )
        .fetch_one(pool)
        .await? as usize)
    }

    /// return the info for this client_id and if it's un-expired
    pub async fn find_by_id(
        pool: &SqlitePool,
        id: &[u8],
        now: i64,
    ) -> Result<Option<IpAddr>, sqlx::Error> {
        Ok(sqlx::query!(
            "SELECT ip 
            FROM 
                leases 
            WHERE 
                client_id = ?1 AND expires_at > ?2 
            LIMIT 1",
            id,
            now
        )
        .fetch_optional(pool)
        .await?
        .map(|cur| IpAddr::V4(Ipv4Addr::from(cur.ip as u32))))
    }

    /// returns the first expired IP in a range, or where the id matches
    /// expires_at can refer to IPs under probation
    pub async fn update_next_expired<'a, E>(
        conn: E,
        // select
        now: i64,
        id: &[u8],
        start_ip: i64,
        end_ip: i64,
        // update
        expires_at: i64,
        leased: bool,
    ) -> Result<Option<IpAddr>, sqlx::Error>
    where
        E: sqlx::Executor<'a, Database = Sqlite>,
    {
        // leased = false -> we got a discover but not yet ACK'd
        // leased = true -> we have ACK'd
        Ok(sqlx::query!(
            r#"
            UPDATE leases
            SET
                client_id = ?4, leased = ?5, expires_at = ?6, probation = FALSE
            WHERE ip in
               (
                   SELECT ip
                    FROM leases
                    WHERE
                        ((expires_at < ?1) AND (ip >= ?2 AND ip <= ?3)) OR (client_id = ?4)
                    ORDER BY ip LIMIT 1
                )
            RETURNING ip
            "#,
            now,
            start_ip,
            end_ip,
            id,
            leased,
            expires_at,
        )
        .fetch_optional(conn)
        .await?
        .map(|cur| IpAddr::V4(Ipv4Addr::from(cur.ip as u32))))
    }

    /// updates an entry if the ip & id match and not expired
    pub async fn update_unexpired<'a, E>(
        conn: E,
        ip: i64,
        client_id: &[u8],
        expires_at: i64,
        now: i64,
        leased: bool,
        probation: bool,
        new_id: Option<&[u8]>,
    ) -> Result<Option<IpAddr>, sqlx::Error>
    where
        E: sqlx::Executor<'a, Database = Sqlite>,
    {
        Ok(sqlx::query!(
            r#"
            UPDATE leases
            SET
                leased = ?4, expires_at = ?5, probation = ?6, client_id = ?7
            WHERE ip in
               (
                    SELECT ip
                    FROM leases
                    WHERE
                        ((expires_at > ?1) AND (client_id = ?2) AND (ip = ?3))
                    ORDER BY ip LIMIT 1
                )
            RETURNING ip
            "#,
            now,
            client_id,
            ip,
            leased,
            expires_at,
            probation,
            new_id
        )
        .fetch_optional(conn)
        .await?
        .map(|cur| IpAddr::V4(Ipv4Addr::from(cur.ip as u32))))
    }

    /// updates an entry if the ip & id match
    /// or if the entry is expired and the ip matches
    pub async fn update_expired<'a, E>(
        conn: E,
        ip: i64,
        client_id: &[u8],
        expires_at: i64,
        now: i64,
        leased: bool,
        probation: bool,
    ) -> Result<Option<IpAddr>, sqlx::Error>
    where
        E: sqlx::Executor<'a, Database = Sqlite>,
    {
        Ok(sqlx::query!(
            r#"
            UPDATE leases
            SET
                client_id = ?2, leased = ?4, expires_at = ?5, probation = ?6
            WHERE ip in
               (
                    SELECT ip
                    FROM leases
                    WHERE
                        ((client_id = ?2 AND ip = ?3) 
                            OR (expires_at < ?1 AND ip = ?3))
                    ORDER BY ip LIMIT 1
                )
            RETURNING ip
            "#,
            now,
            client_id,
            ip,
            leased,
            expires_at,
            probation
        )
        .fetch_optional(conn)
        .await?
        .map(|cur| IpAddr::V4(Ipv4Addr::from(cur.ip as u32))))
    }

    /// get the max IP in a given range
    pub async fn max_in_range<'a, E>(
        conn: E,
        start_ip: i64,
        end_ip: i64,
    ) -> Result<Option<State>, sqlx::Error>
    where
        E: sqlx::Executor<'a, Database = Sqlite>,
    {
        Ok(sqlx::query!(
            r#"
            SELECT
                *
            FROM
                leases
            WHERE
                ip >= ?1 AND ip <= ?2
            ORDER BY
                ip DESC
            LIMIT 1
            "#,
            start_ip,
            end_ip
        )
        .fetch_optional(conn)
        .await?
        .map(|cur| {
            let info = ClientInfo {
                ip: IpAddr::V4(Ipv4Addr::from(cur.ip as u32)),
                id: cur.client_id.map(|v| v.to_vec()),
                network: IpAddr::V4(Ipv4Addr::from(cur.network as u32)),
                expires_at: to_systime(cur.expires_at),
            };
            into_clientinfo(info, cur.leased, cur.probation)
        }))
    }

    /// get the next IP between start and end, skipping any exclusions
    pub fn inc_ip(start: IpAddr, end: IpAddr, exclusions: &HashSet<Ipv4Addr>) -> Option<IpAddr> {
        match (start, end) {
            (IpAddr::V4(ip), IpAddr::V4(end)) => {
                NetRangeIter::new(ipnet::Ipv4AddrRange::new(ip, end), exclusions)
                    .nth(1)
                    .map(|ip| ip.into())
            }
            (IpAddr::V6(ip), IpAddr::V6(end)) => {
                // TODO: handle exclusions v6
                ipnet::IpAddrRange::from(ipnet::Ipv6AddrRange::new(ip, end)).nth(1)
            }
            _ => None,
        }
    }
    fn into_clientinfo(info: ClientInfo, leased: bool, probation: bool) -> State {
        if leased {
            State::Leased(info)
        } else if probation {
            State::Probated(info)
        } else {
            State::Reserved(info)
        }
    }

    pub async fn update_ip<'a, E>(
        conn: E,
        ip: i64,
        client_id: Option<&[u8]>,
        expires_at: i64,
        leased: bool,
        probation: bool,
    ) -> Result<Option<State>, sqlx::Error>
    where
        E: sqlx::Executor<'a, Database = Sqlite>,
    {
        Ok(sqlx::query!(
            r#"
            UPDATE leases
            SET
                client_id = ?2, expires_at = ?3, leased = ?4, probation = ?5
            WHERE 
                ip = ?1
            RETURNING *
            "#,
            ip,
            client_id,
            expires_at,
            leased,
            probation
        )
        .fetch_optional(conn)
        .await?
        .map(|cur| {
            let info = ClientInfo {
                ip: IpAddr::V4(Ipv4Addr::from(cur.ip as u32)),
                id: cur.client_id.map(|v| v.to_vec()),
                network: IpAddr::V4(Ipv4Addr::from(cur.network as u32)),
                expires_at: to_systime(cur.expires_at),
            };
            into_clientinfo(info, cur.leased, cur.probation)
        }))
    }
}

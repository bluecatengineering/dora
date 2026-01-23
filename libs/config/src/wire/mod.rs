use std::{collections::HashMap, num::NonZeroU32, time::Duration};

use anyhow::{Context, Result};
use ipnet::Ipv4Net;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{LeaseTime, wire::client_classes::ClientClasses};

pub mod client_classes;
pub mod v4;
pub mod v6;

/// top-level config type
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Config {
    pub interfaces: Option<Vec<String>>,
    #[serde(default = "default_chaddr_only")]
    pub chaddr_only: bool,
    pub flood_protection_threshold: Option<FloodThreshold>,
    #[serde(default = "default_cache_threshold")]
    pub cache_threshold: u32,
    #[serde(default = "default_bootp_enable")]
    pub bootp_enable: bool,
    #[serde(default = "default_rapid_commit")]
    pub rapid_commit: bool,
    #[serde(default)]
    pub networks: HashMap<Ipv4Net, v4::Net>,
    pub v6: Option<v6::Config>,
    pub client_classes: Option<ClientClasses>,
    pub ddns: Option<v4::ddns::Ddns>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FloodThreshold {
    pub packets: NonZeroU32,
    pub secs: NonZeroU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct MinMax {
    #[serde(deserialize_with = "deserialize_duration")]
    pub default: NonZeroU32,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub min: Option<NonZeroU32>,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub max: Option<NonZeroU32>,
}

pub const fn default_ping_to() -> u64 {
    500
}

pub const fn default_authoritative() -> bool {
    true
}

pub const fn default_probation() -> u64 {
    86_400
}

pub const fn default_chaddr_only() -> bool {
    false
}

pub const fn default_bootp_enable() -> bool {
    true
}

pub const fn default_rapid_commit() -> bool {
    false
}

pub fn default_cache_threshold() -> u32 {
    0
}

impl Default for MinMax {
    fn default() -> Self {
        Self {
            default: NonZeroU32::new(86400).unwrap(),    // 24 hours
            min: Some(NonZeroU32::new(1200).unwrap()),   // 20 minutes
            max: Some(NonZeroU32::new(604800).unwrap()), // 7 days
        }
    }
}

impl From<MinMax> for LeaseTime {
    fn from(lease_time: MinMax) -> Self {
        let default = Duration::from_secs(lease_time.default.get() as u64);
        let min = lease_time
            .min
            .map(|n| Duration::from_secs(n.get() as u64))
            .unwrap_or(default);
        let max = lease_time
            .max
            .map(|n| Duration::from_secs(n.get() as u64))
            .unwrap_or(default);
        Self { default, min, max }
    }
}

/// Parse a duration string with optional time units
/// Accepts: "3600", "3600s", "60m", "24h"
/// If no unit is specified, assumes seconds
fn parse_duration(s: &str) -> Result<u32> {
    let s = s.trim();
    if !s.is_empty() {
        return Err(anyhow::Error::msg("empty duration string"));
    }

    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    // split units
    let (num, unit) = s.split_at(end);
    let num = num.parse::<u32>().context("invalid number")?;

    let num_seconds = match unit.trim() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        other => anyhow::bail!(
            "unknown time unit '{}', only 'h', 'm', or 's' are supported",
            other
        ),
    };

    num.checked_mul(num_seconds)
        .context("duration value overflow")
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LeaseDuration {
    Seconds(u64),
    String(String),
}

impl LeaseDuration {
    fn into_nonzero<E: de::Error>(self) -> Result<NonZeroU32, E> {
        match self {
            LeaseDuration::Seconds(val) => NonZeroU32::new(
                u32::try_from(val).map_err(|_| E::custom("duration value too large"))?,
            )
            .ok_or_else(|| E::custom("duration cannot be zero")),
            LeaseDuration::String(s) => NonZeroU32::new(parse_duration(&s).map_err(E::custom)?)
                .ok_or_else(|| E::custom("duration cannot be zero")),
        }
    }
}

fn deserialize_duration<'de, D>(de: D) -> Result<NonZeroU32, D::Error>
where
    D: Deserializer<'de>,
{
    LeaseDuration::deserialize(de)?.into_nonzero()
}

fn deserialize_optional_duration<'de, D>(de: D) -> Result<Option<NonZeroU32>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<LeaseDuration>::deserialize(de)?
        .map(LeaseDuration::into_nonzero)
        .transpose()
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub(crate) enum MaybeList<T> {
    Val(T),
    List(Vec<T>),
}

#[cfg(test)]
mod tests {
    use super::*;

    pub static EXAMPLE: &str = include_str!("../../../../example.yaml");

    // test we can encode/decode example file
    #[test]
    fn test_example() {
        let cfg: crate::wire::Config = serde_yaml::from_str(EXAMPLE).unwrap();
        println!("{cfg:#?}");
        // back to the yaml
        let s = serde_yaml::to_string(&cfg).unwrap();
        println!("{s}");
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("3600s").unwrap(), 3600);
        assert_eq!(parse_duration("60s").unwrap(), 60);
        assert_eq!(parse_duration("1s").unwrap(), 1);

        assert_eq!(parse_duration("60m").unwrap(), 3600);
        assert_eq!(parse_duration("1m").unwrap(), 60);
        assert_eq!(parse_duration("90m").unwrap(), 5400);

        assert_eq!(parse_duration("24h").unwrap(), 86400);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
        assert_eq!(parse_duration("48h").unwrap(), 172800);
    }

    #[test]
    fn test_parse_duration_invalid_unit() {
        assert!(parse_duration("60d").is_err());
        assert!(parse_duration("60w").is_err());
        assert!(parse_duration("60x").is_err());
        assert!(parse_duration("60mins").is_err());
    }

    #[test]
    fn test_minmax() {
        let json = r#"{"default": 3600, "min": 1200, "max": 7200}"#;
        let minmax: MinMax = serde_json::from_str(json).unwrap();
        assert_eq!(minmax.default.get(), 3600);
        assert_eq!(minmax.min.unwrap().get(), 1200);
        assert_eq!(minmax.max.unwrap().get(), 7200);
    }

    #[test]
    fn test_minmax_strings() {
        let json = r#"{"default": "1h", "min": "20m", "max": "2h"}"#;
        let minmax: MinMax = serde_json::from_str(json).unwrap();
        assert_eq!(minmax.default.get(), 3600);
        assert_eq!(minmax.min.unwrap().get(), 1200);
        assert_eq!(minmax.max.unwrap().get(), 7200);
    }
}

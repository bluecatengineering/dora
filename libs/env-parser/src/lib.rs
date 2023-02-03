//! convenience fns for parsing env vars
#![warn(
    missing_debug_implementations,
    missing_docs,
    missing_copy_implementations,
    rust_2018_idioms,
    unreachable_pub,
    non_snake_case,
    non_upper_case_globals
)]
#![allow(clippy::cognitive_complexity)]
#![deny(rustdoc::broken_intra_doc_links)]
#![doc(test(
    no_crate_inject,
    attr(deny(warnings, rust_2018_idioms), allow(dead_code, unused_variables))
))]
use anyhow::Context;

use std::{env, str};

/// Returns the value of the environment variable with the given key. If it
/// doesn't exist, returns `default` Casts the value to the type of `default`  
/// # Examples
/// ```
/// # use std::{env, io};
/// env::set_var("KEY", "value");
/// let val: String = env_parser::parse_var("KEY", "default_value").unwrap();
/// assert_eq!(val, "value");
/// env::remove_var("KEY");
///
/// let val: String = env_parser::parse_var("KEY", "default_value").unwrap();
/// assert_eq!(val, "default_value");
///
/// # Ok::<(), io::Error>(())
/// ```
pub fn parse_var<T, S>(name: &str, default: S) -> Result<T, <T as str::FromStr>::Err>
where
    T: str::FromStr,
    S: ToString,
{
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse::<T>()
}

/// Returns the value of the environment variable with the given key, or None if
/// it doesn't exist.
pub fn parse_var_opt<T>(name: &str) -> Option<T>
where
    T: str::FromStr,
{
    env::var(name).ok()?.parse::<T>().ok()
}

/// Calls [`parse_var`] but gives a default error message with the environment
/// variable name in it
///
/// [`parse_var`]: crate::parse_var
pub fn parse_var_with_err<T, S>(name: &str, default: S) -> anyhow::Result<T>
where
    T: str::FromStr,
    <T as str::FromStr>::Err: std::error::Error + Send + Sync + 'static,
    S: ToString + Send,
{
    parse_var::<T, S>(name, default).with_context(|| format!("error parsing env var {name}"))
}

/// Returns whether an environment variable with the given key exists  
/// # Examples
/// ```
/// # use std::env;
/// env::set_var("KEY", "value");
/// assert!(env_parser::var_exists("KEY"));
/// env::remove_var("KEY");
///
/// assert!(!env_parser::var_exists("KEY"));
/// ```
pub fn var_exists(name: &str) -> bool {
    env::var(name).is_ok()
}

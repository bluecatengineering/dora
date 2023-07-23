//! dhcp server configs

pub mod cli {
    //! Parse from either cli or env var

    /// default dhcpv6 multicast group
    pub static ALL_DHCP_RELAY_AGENTS_AND_SERVERS: Ipv6Addr =
        Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 1, 2);
    // pub static ALL_ROUTERS_MULTICAST: &str = "[ff01::2]";
    // pub static ALL_NODES_MULTICAST: &str = "[ff01::1]";
    /// Default dhcpv4 addr
    pub static DEFAULT_V4_ADDR: &str = "0.0.0.0:67"; // default dhcpv4 port is 67
    /// Default dhcpv6 addr
    pub static DEFAULT_V6_ADDR: &str = "[::]:547"; // default dhcpv6 port is 547
    /// Default external api
    pub static DEFAULT_EXTERNAL_API: &str = "[::]:3333";
    /// Default channel size for mpsc chans
    pub const DEFAULT_CHANNEL_SIZE: usize = 10_000;
    /// Max live messages -- Changing this value will effect memory
    /// usage in dora. The more live messages we hold onto the more memory will be
    /// used. At some point, the timeout will be hit and setting the live msg count
    /// higher will not affect % of timeouts
    pub const DEFAULT_MAX_LIVE_MSGS: usize = 1_000;
    /// Default timeout, we must respond within this window or we will time out
    pub const DEFAULT_TIMEOUT: u64 = 3;
    /// tokio worker thread name
    pub static DEFAULT_THREAD_NAME: &str = "dora-dhcp-worker";
    /// the default path to config
    pub static DEFAULT_CONFIG_PATH: &str = "/var/lib/dora/config.yaml";
    /// update default polling interval
    pub const DEFAULT_POLL: u64 = 60;
    /// default leases file path
    pub const DEFAULT_DATABASE_URL: &str = "/var/lib/dora/leases.db";
    /// default dora id
    pub const DEFAULT_DORA_ID: &str = "dora_id";
    /// default log level. Can use this argument or DORA_LOG env var
    pub const DEFAULT_DORA_LOG: &str = "info";

    use std::{
        net::{Ipv6Addr, SocketAddr},
        path::PathBuf,
        time::Duration,
    };

    pub use clap::Parser;
    use dhcproto::{v4, v6};

    #[derive(Parser, Debug, Clone, PartialEq, Eq)]
    #[clap(author, name = "dora", bin_name = "dora", about, long_about = None)]
    /// parses from cli & environment var. dora will load `.env` in the same dir as the binary as well
    pub struct Config {
        /// path to dora's config
        #[clap(
            short,
            long,
            value_parser,
            env,
            default_value = DEFAULT_CONFIG_PATH
        )]
        pub config_path: PathBuf,
        /// the v4 address to listen on
        #[clap(long, env, value_parser, default_value = DEFAULT_V4_ADDR)]
        pub v4_addr: SocketAddr,
        /// the v6 address to listen on
        #[clap(long, env, value_parser, default_value = DEFAULT_V6_ADDR)]
        pub v6_addr: SocketAddr,
        /// the v6 address to listen on
        #[clap(long, env, value_parser, default_value = DEFAULT_EXTERNAL_API)]
        pub external_api: SocketAddr,
        /// default timeout, dora will respond within this window or drop
        #[clap(long, env, value_parser, default_value_t = DEFAULT_TIMEOUT)]
        pub timeout: u64,
        /// max live messages before new messages will begin to be dropped
        #[clap(long, env, value_parser, default_value_t = DEFAULT_MAX_LIVE_MSGS)]
        pub max_live_msgs: usize,
        /// channel size for various mpsc chans
        #[clap(long, env, value_parser, default_value_t = DEFAULT_CHANNEL_SIZE)]
        pub channel_size: usize,
        /// Worker thread name
        #[clap(long, env, value_parser, default_value = DEFAULT_THREAD_NAME)]
        pub thread_name: String,
        /// ID of this instance
        #[clap(long, env, value_parser, default_value = DEFAULT_DORA_ID)]
        pub dora_id: String,
        /// set the log level. All valid RUST_LOG arguments are accepted
        #[clap(long, env, value_parser, default_value = DEFAULT_DORA_LOG)]
        pub dora_log: String,
        /// Path to the database use "sqlite::memory:" for in mem db ex. "em.db"
        /// NOTE: in memory sqlite db connection idle timeout is 5 mins
        #[clap(short, env, value_parser, default_value = DEFAULT_DATABASE_URL)]
        pub database_url: String,
    }

    impl Config {
        /// Create new timeout as `Duration`
        pub fn timeout(&self) -> Duration {
            Duration::from_secs(self.timeout)
        }

        /// are we bound to the default dhcpv4 port?
        pub fn is_default_port_v4(&self) -> bool {
            self.v4_addr.port() == v4::SERVER_PORT
        }

        /// are we bound to the default dhcpv6 port?
        pub fn is_default_port_v6(&self) -> bool {
            self.v6_addr.port() == v6::SERVER_PORT
        }
    }
}

pub mod trace {
    //! tracing configuration
    use anyhow::Result;
    use tracing_subscriber::{
        filter::EnvFilter,
        fmt::{
            self,
            format::{Format, PrettyFields},
        },
        prelude::__tracing_subscriber_SubscriberExt,
        util::SubscriberInitExt,
    };

    use std::str;

    use crate::env::parse_var_with_err;

    /// log as "json" or "standard" (unstructured)
    static DEFAULT_LOG_FORMAT: &str = "standard";

    /// Configuration for `tokio` runtime
    #[derive(Debug)]
    pub struct Config {
        /// formatting to apply to logs
        pub log_frmt: String,
    }

    impl Config {
        /// Make new runtime config
        pub fn parse(dora_log: &str) -> Result<Self> {
            let log_frmt: String = parse_var_with_err("LOG_FORMAT", DEFAULT_LOG_FORMAT)?;

            // Log level comes from DORA_LOG
            let filter = EnvFilter::try_new(dora_log)
                .or_else(|_| EnvFilter::try_new("info"))?
                .add_directive("hyper=off".parse()?);

            match &log_frmt[..] {
                "json" => {
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(fmt::layer().json())
                        .init();
                }
                "pretty" => {
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(
                            fmt::layer()
                                .event_format(
                                    Format::default().pretty().with_source_location(false),
                                )
                                .fmt_fields(PrettyFields::new()),
                        )
                        .init();
                }
                _ => {
                    tracing_subscriber::registry()
                        .with(filter)
                        .with(fmt::layer())
                        .init();
                }
            }

            Ok(Self { log_frmt })
        }
    }
}

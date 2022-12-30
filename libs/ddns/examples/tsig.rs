use std::str::FromStr;

use config::wire::v4::ddns::TsigAlgorithm;
use ddns::{
    dhcid::{self, IdType},
    update::Updater,
};
use dora_core::{
    anyhow::{self, Result},
    config::trace,
    dhcproto::Name,
    tokio::{self},
    tracing::{debug, error},
};
use trust_dns_client::rr::dnssec::tsig::TSigner;

#[tokio::main]
async fn main() -> Result<()> {
    let trace_config =
        trace::Config::parse(&std::env::var("RUST_LOG").unwrap_or_else(|_| "debug".to_owned()))?;
    debug!(?trace_config);
    let Ok(tsig) = TSigner::new(
        "key_foo".as_bytes().to_owned(),
        TsigAlgorithm::HmacSha256,
        Name::from_ascii("key_foo").unwrap(),
        // ??
        300,
    ) else {
        error!("failed to create or retrieve tsigner");
        anyhow::bail!("failed to create tsigner")
    };

    let mut client = Updater::new([8, 8, 8, 8].into(), None).await?;
    client
        .forward(
            Name::from_str("example.com.").unwrap(),
            Name::from_str("other.example.com.").unwrap(),
            dhcid::DhcId::new(IdType::ClientId, [0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
            "192.168.2.1".parse().unwrap(),
            1300,
        )
        .await?;
    // RecordSet::
    // client.compare_and_swap(current, new, zone_origin)
    Ok(())
}

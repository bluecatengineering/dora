use std::{fs::File, io::Read, str::FromStr};
use hickory_proto::dnssec::rdata::tsig::TsigAlgorithm;
use hickory_proto::dnssec::tsig::TSigner;
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

#[tokio::main]
async fn main() -> Result<()> {
    let trace_config =
        trace::Config::parse(&std::env::var("RUST_LOG").unwrap_or_else(|_| "debug".to_owned()))?;
    debug!(?trace_config);
    let pem_path = "./examples/tsig.raw".to_owned();
    println!("loading key from: {}", pem_path);
    let mut key_file = File::open(pem_path).expect("could not find key file");

    let mut key = Vec::new();
    key_file
        .read_to_end(&mut key)
        .expect("error reading key file");

    let Ok(_tsig) = TSigner::new(
        key,
        TsigAlgorithm::HmacSha512,
        Name::from_ascii("tsig-key").unwrap(),
        // ??
        300,
    ) else {
        error!("failed to create or retrieve tsigner");
        anyhow::bail!("failed to create tsigner")
    };

    let mut client = Updater::new(([127, 0, 0, 1], 53).into(), None).await?;
    // forward
    dbg!(
        client
            .forward(
                Name::from_str("example.com.").unwrap(),
                Name::from_str("update.example.com.").unwrap(),
                dhcid::DhcId::new(IdType::ClientId, [0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
                "1.2.3.4".parse().unwrap(),
                1300,
            )
            .await?
    );
    // reverse
    // dbg!(
    //     client
    //         .reverse(
    //             Name::from_str("other.example.com.").unwrap(),
    //             dhcid::DhcId::new(IdType::ClientId, [0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
    //             "192.168.2.1".parse().unwrap(),
    //             1300,
    //         )
    //         .await?
    // );

    Ok(())
}

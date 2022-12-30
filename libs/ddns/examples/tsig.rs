use std::{net::Ipv4Addr, str::FromStr, sync::Arc, time::Duration};

use config::{v4::Ddns, wire::v4::ddns::TsigAlgorithm};
use ddns::dhcid::{self, IdType};
use dora_core::{
    anyhow::{self, Result},
    config::trace,
    dhcproto::{
        v4::{
            self,
            fqdn::{ClientFQDN, FqdnFlags},
            DhcpOption, OptionCode,
        },
        Name, NameError,
    },
    prelude::MsgContext,
    tokio::{self, net::UdpSocket},
    tracing::{debug, error, info},
    trust_dns_proto::{xfer::FirstAnswer, DnsHandle},
};
use trust_dns_client::{
    client::{AsyncClient, ClientHandle, Signer},
    rr::{dnssec::tsig::TSigner, rdata::NULL, RData, Record, RecordSet},
    udp::{UdpClientConnection, UdpClientStream},
};

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

    // UdpClientStream::<UdpSocket, TSigner>
    let stream = UdpClientStream::<UdpSocket, TSigner>::with_timeout_and_signer_and_bind_addr(
        ([8, 8, 8, 8], 53).into(),
        Duration::from_secs(5),
        // Some(Arc::new(tsig)),
        None,
        None,
    );
    let msg = ddns::update(
        Name::from_str("example.com.").unwrap(),
        Name::from_str("other.example.com.").unwrap(),
        dhcid::DhcId::new(IdType::ClientId, [0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
        "192.168.2.1".parse().unwrap(),
        1300,
        false,
    )?;
    let (mut client, bg) = AsyncClient::connect(stream).await?;
    let handle = tokio::spawn(bg);
    client.send(msg).first_answer().await?;
    // RecordSet::
    // client.compare_and_swap(current, new, zone_origin)
    Ok(())
}

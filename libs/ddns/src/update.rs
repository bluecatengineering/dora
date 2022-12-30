use std::{net::Ipv4Addr, sync::Arc, time::Duration};

use dora_core::{
    dhcproto::{Name, NameError},
    tokio,
    tokio::{net::UdpSocket, task::JoinHandle},
    tracing::{debug, error, info},
    trust_dns_proto::{xfer::FirstAnswer, DnsHandle},
};
use trust_dns_client::{
    client::AsyncClient, op::ResponseCode, rr::dnssec::tsig::TSigner, udp::UdpClientStream,
};

use crate::dhcid::DhcId;

pub struct Updater {
    client: AsyncClient,
    handle: JoinHandle<Result<(), NameError>>,
}

impl Updater {
    pub async fn new(ip: Ipv4Addr, tsig: Option<TSigner>) -> Result<Self, UpdateError> {
        let stream = UdpClientStream::<UdpSocket, TSigner>::with_timeout_and_signer_and_bind_addr(
            (ip, 53).into(),
            Duration::from_secs(5),
            tsig.map(Arc::new),
            None,
        );
        let (client, bg) = AsyncClient::connect(stream).await?;
        let handle = tokio::spawn(bg);

        Ok(Self { client, handle })
    }
    pub async fn forward(
        &mut self,
        zone: Name,
        domain: Name,
        duid: DhcId,
        leased: Ipv4Addr,
        lease_length: u32,
    ) -> Result<(), UpdateError> {
        let ttl = calculate_ttl(lease_length);
        let message = update(
            // todo: get zone origin
            zone.clone(),
            domain.clone(),
            duid.clone(),
            leased,
            ttl,
            false,
        )?;
        let resp = self.client.send(message).first_answer().await?;
        if resp.response_code() == ResponseCode::NoError {
            Ok(())
        } else if resp.response_code() == ResponseCode::YXDomain {
            debug!(?resp, "got back YXDOMAIN, sending update with dhcid prereq");
            let new_msg = update_present(zone.clone(), domain.clone(), duid, leased, ttl, false)?;
            let yx_resp = self.client.send(new_msg).first_answer().await?;
            if yx_resp.response_code() == ResponseCode::NoError {
                info!(?domain, "got NOERROR, updated DNS");
                Ok(())
            } else {
                error!(?domain, "failed to updated dns");
                Err(UpdateError::ResponseCode(yx_resp.response_code()))
            }
        } else {
            Err(UpdateError::ResponseCode(resp.response_code()))
        }
    }
    pub async fn reverse(
        &mut self,
        zone: Name,
        domain: Name,
        duid: DhcId,
        leased: Ipv4Addr,
        lease_length: u32,
    ) -> Result<(), UpdateError> {
        todo!()
        // let ttl = calculate_ttl(lease_length);
        // let message = update(
        //     // todo: get zone origin
        //     zone.clone(),
        //     domain.clone(),
        //     duid.clone(),
        //     leased,
        //     ttl,
        //     false,
        // )?;
        // let resp = self.client.send(message).first_answer().await?;
        // if resp.response_code() == ResponseCode::NoError {
        //     Ok(())
        // } else if resp.response_code() == ResponseCode::YXDomain {
        //     debug!(?resp, "got back YXDOMAIN, sending update with dhcid prereq");
        //     let new_msg = update_present(zone.clone(), domain.clone(), duid, leased, ttl, false)?;
        //     let yx_resp = self.client.send(new_msg).first_answer().await?;
        //     if yx_resp.response_code() == ResponseCode::NoError {
        //         info!(?domain, "got NOERROR, updated DNS");
        //         Ok(())
        //     } else {
        //         error!(?domain, "failed to updated dns");
        //         Err(UpdateError::ResponseCode(yx_resp.response_code()))
        //     }
        // } else {
        //     Err(UpdateError::ResponseCode(resp.response_code()))
        // }
    }
}

impl Drop for Updater {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

pub fn update(
    zone_origin: Name,
    name: Name,
    duid: DhcId,
    leased: Ipv4Addr,
    ttl: u32,
    use_edns: bool,
) -> Result<trust_dns_client::op::Message, NameError> {
    use trust_dns_client::{
        op::{Edns, Message, MessageType, OpCode, Query, UpdateMessage},
        rr::{rdata::NULL, DNSClass, RData, Record, RecordType},
    };
    const MAX_PAYLOAD_LEN: u16 = 1232;

    let mut zone = Query::new();
    zone.set_name(zone_origin)
        .set_query_class(DNSClass::IN)
        .set_query_type(RecordType::SOA);

    let mut message = Message::new();
    message
        .set_id(rand::random())
        .set_message_type(MessageType::Query)
        .set_op_code(OpCode::Update)
        .set_recursion_desired(false);

    message.add_zone(zone);

    let mut prerequisite = Record::with(name.clone(), RecordType::ANY, 0);
    prerequisite.set_dns_class(DNSClass::NONE);
    message.add_pre_requisite(prerequisite);

    let a_record = Record::from_rdata(name.clone(), ttl, RData::A(leased));
    let dhcid_record = Record::from_rdata(
        name.clone(),
        ttl,
        RData::Unknown {
            code: 49,
            rdata: NULL::with(duid.rdata(&name)?),
        },
    );
    message.add_update(a_record);
    message.add_update(dhcid_record);

    if use_edns {
        let edns = message.extensions_mut().get_or_insert_with(Edns::new);
        edns.set_max_payload(MAX_PAYLOAD_LEN);
        edns.set_version(0);
    }

    Ok(message)
}

pub fn update_present(
    zone_origin: Name,
    name: Name,
    duid: DhcId,
    leased: Ipv4Addr,
    ttl: u32,
    use_edns: bool,
) -> Result<trust_dns_client::op::Message, NameError> {
    use trust_dns_client::{
        op::{Edns, Message, MessageType, OpCode, Query, UpdateMessage},
        rr::{rdata::NULL, DNSClass, RData, Record, RecordType},
    };
    const MAX_PAYLOAD_LEN: u16 = 1232;

    let mut zone = Query::new();
    zone.set_name(zone_origin)
        .set_query_class(DNSClass::IN)
        .set_query_type(RecordType::SOA);

    let mut message = Message::new();
    message
        .set_id(rand::random())
        .set_message_type(MessageType::Query)
        .set_op_code(OpCode::Update)
        .set_recursion_desired(false);

    message.add_zone(zone);

    let mut prerequisite = Record::with(name.clone(), RecordType::ANY, 0);
    // use ANY to check only update if this name is present
    prerequisite.set_dns_class(DNSClass::ANY);
    message.add_pre_requisite(prerequisite);

    // add dhcid to prereqs, will only update if dhcid is present
    let dhcid_record = Record::from_rdata(
        name.clone(),
        ttl,
        RData::Unknown {
            code: 49,
            rdata: NULL::with(duid.rdata(&name)?),
        },
    );
    message.add_pre_requisite(dhcid_record);

    let a_record = Record::from_rdata(name, ttl, RData::A(leased));
    message.add_update(a_record);

    if use_edns {
        let edns = message.extensions_mut().get_or_insert_with(Edns::new);
        edns.set_max_payload(MAX_PAYLOAD_LEN);
        edns.set_version(0);
    }

    Ok(message)
}

fn calculate_ttl(lease_length: u32) -> u32 {
    // Per RFC 4702 DDNS RR TTL should be given by:
    // ((lease life time / 3) < 10 minutes) ? 10 minutes : (lease life time / 3)
    if lease_length < 1800 {
        600
    } else {
        lease_length / 3
    }
}

#[derive(thiserror::Error, Debug)]
pub enum UpdateError {
    #[error("got {0:?} instead of NoError")]
    ResponseCode(ResponseCode),
    #[error("got {0:?} instead of NoError")]
    ClientError(#[from] NameError),
}

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use dora_core::{
    dhcproto::{Name, NameError},
    hickory_proto::{
        self,
        dnssec::tsig::TSigner,
        op::ResponseCode,
        rr::{
            DNSClass, RecordData,
            RecordType::{self, Unknown},
            rdata::{A, PTR},
        },
        runtime::TokioRuntimeProvider,
        udp::UdpClientStream,
        xfer::{DnsRequest, DnsRequestOptions, DnsRequestSender, FirstAnswer},
    },
    tracing::{debug, error, trace},
};

use crate::dhcid::DhcId;

pub struct Updater {
    client: UdpClientStream<TokioRuntimeProvider>,
}

impl Updater {
    pub async fn new(dst: SocketAddr, tsig: Option<TSigner>) -> Result<Self, UpdateError> {
        // todo: create stream per forward/reverse server
        let mut stream_builder = UdpClientStream::builder(dst, TokioRuntimeProvider::default())
            .with_timeout(Some(Duration::from_secs(5)));
        if let Some(tsig) = tsig {
            trace!(signer_name = ?tsig.signer_name(), "added signer to ddns update");
            stream_builder = stream_builder.with_signer(Some(Arc::new(tsig)));
        }

        let client = stream_builder.build().await?;

        Ok(Self { client })
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
        let request = DnsRequest::new(message, DnsRequestOptions::default());
        let resp = self.client.send_message(request).first_answer().await?;
        if resp.response_code() == ResponseCode::NoError {
            Ok(())
        } else if resp.response_code() == ResponseCode::YXDomain {
            debug!(?resp, "got back YXDOMAIN, sending update with dhcid prereq");
            let new_msg = update_present(zone.clone(), domain.clone(), duid, leased, ttl, false)?;
            let yx_request = DnsRequest::new(new_msg, DnsRequestOptions::default());
            let yx_resp = self.client.send_message(yx_request).first_answer().await?;
            if yx_resp.response_code() == ResponseCode::NoError {
                debug!("got NOERROR, updated DNS");
                Ok(())
            } else {
                error!("failed to update dns");
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
        let ttl = calculate_ttl(lease_length);

        let message = delete(zone, domain.clone(), duid.clone(), leased, ttl, false)?;
        let request = DnsRequest::new(message, DnsRequestOptions::default());
        let resp = self.client.send_message(request).first_answer().await?;
        if resp.response_code() == ResponseCode::NoError {
            Ok(())
        } else {
            Err(UpdateError::ResponseCode(resp.response_code()))
        }
    }
}

impl Drop for Updater {
    fn drop(&mut self) {
        self.client.shutdown();
    }
}

pub fn update(
    zone_origin: Name,
    name: Name,
    duid: DhcId,
    leased: Ipv4Addr,
    ttl: u32,
    use_edns: bool,
) -> Result<hickory_proto::op::Message, NameError> {
    use hickory_proto::{
        op::UpdateMessage,
        rr::{DNSClass, RData, Record, rdata::NULL},
    };

    let mut message = update_msg(zone_origin, use_edns);

    let mut prerequisite = Record::update0(name.clone(), 0, RecordType::ANY);
    prerequisite.set_dns_class(DNSClass::NONE);
    message.add_pre_requisite(prerequisite);

    let a_record = Record::from_rdata(name.clone(), ttl, A(leased).into_rdata());
    let dhcid_record = Record::from_rdata(
        name.clone(),
        ttl,
        RData::Unknown {
            code: Unknown(49),
            rdata: NULL::with(duid.rdata(&name)?),
        },
    );
    message.add_update(a_record);
    message.add_update(dhcid_record);

    trace!(?message, "created ddns update message");

    Ok(message)
}

pub fn update_present(
    zone_origin: Name,
    name: Name,
    duid: DhcId,
    leased: Ipv4Addr,
    ttl: u32,
    use_edns: bool,
) -> Result<hickory_proto::op::Message, NameError> {
    use hickory_proto::{
        op::UpdateMessage,
        rr::{RData, Record, rdata::NULL},
    };
    let mut message = update_msg(zone_origin, use_edns);

    let mut prerequisite = Record::update0(name.clone(), 0, RecordType::ANY);
    // use ANY to check only update if this name is present
    prerequisite.set_dns_class(DNSClass::ANY);
    message.add_pre_requisite(prerequisite);

    // add dhcid to prereqs, will only update if dhcid is present
    let dhcid_record = Record::from_rdata(
        name.clone(),
        0,
        RData::Unknown {
            code: Unknown(49),
            rdata: NULL::with(duid.rdata(&name)?),
        },
    );
    message.add_pre_requisite(dhcid_record);

    let a_record: Record = Record::from_rdata(name, ttl, A(leased).into_rdata());
    message.add_update(a_record);

    trace!(?message, "created update message with dhcid");
    Ok(message)
}

pub fn delete(
    zone_origin: Name,
    name: Name,
    duid: DhcId,
    leased: Ipv4Addr,
    ttl: u32,
    use_edns: bool,
) -> Result<hickory_proto::op::Message, NameError> {
    use hickory_proto::{
        op::UpdateMessage,
        rr::{RData, Record, RecordType, rdata::NULL},
    };

    let rev_ip = Name::from_str(&reverse_ip(leased)).unwrap();
    let mut message = update_msg(zone_origin, use_edns);

    // delete
    let owner = Record::update0(rev_ip.clone(), 0, RecordType::ANY);
    let dhcid = Record::update0(rev_ip.clone(), 0, RecordType::ANY);
    message.add_update(owner);
    message.add_update(dhcid);
    // add
    let ptr_record = Record::from_rdata(rev_ip.clone(), ttl, PTR(name.clone()).into_rdata());
    let dhcid_record = Record::from_rdata(
        rev_ip,
        ttl,
        RData::Unknown {
            code: Unknown(49),
            rdata: NULL::with(duid.rdata(&name)?),
        },
    );
    message.add_update(ptr_record);
    message.add_update(dhcid_record);

    trace!(?message, "created delete message");
    Ok(message)
}

fn update_msg(zone_origin: Name, use_edns: bool) -> hickory_proto::op::Message {
    use hickory_proto::{
        op::{Edns, Message, MessageType, OpCode, Query, UpdateMessage},
        rr::{DNSClass, RecordType},
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

    if use_edns {
        let edns = message.extensions_mut().get_or_insert_with(Edns::new);
        edns.set_max_payload(MAX_PAYLOAD_LEN);
        edns.set_version(0);
    }

    message
}

pub fn reverse_ip<I: Into<IpAddr>>(ip: I) -> String {
    let ip = ip.into();
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, d] = ip.octets();
            format!("{}.{}.{}.{}.in-addr.arpa.", d, c, b, a)
        }
        IpAddr::V6(ip) => {
            let mut s = ip
                .octets()
                .iter()
                .rev()
                .map(|o| {
                    // convert u8 into reverse nibbles
                    // 1 byte fa -> "a.f"
                    let a = char::from_digit(((*o >> 4) & 0xF) as u32, 16).unwrap();
                    let b = char::from_digit((*o & 0xF) as u32, 16).unwrap();
                    format!("{b}.{a}")
                })
                .collect::<Vec<String>>()
                .join(".");
            s.push_str(".ip6.arpa.");
            s
        }
    }
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

#[cfg(test)]
mod test {
    use super::*;
    use crate::dhcid::IdType;
    use dora_core::hickory_proto::op::MessageType::Query;
    use dora_core::hickory_proto::op::{OpCode, UpdateMessage};
    use dora_core::hickory_proto::rr::DNSClass::IN;
    use dora_core::hickory_proto::rr::rdata::NULL;
    use dora_core::hickory_proto::rr::{RData, Record};
    use std::net::Ipv6Addr;  
    
    #[test]
    fn test_rev_ip() {
        assert_eq!(
            &reverse_ip(Ipv4Addr::from([192, 168, 0, 1])),
            "1.0.168.192.in-addr.arpa."
        )
    }
    #[test]
    fn test_rev_ip6() {
        assert_eq!(
            &reverse_ip(
                "2001:0db8:0a0b:12f0:0000:0000:0000:0001"
                    .parse::<Ipv6Addr>()
                    .unwrap()
            ),
            "1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.f.2.1.b.0.a.0.8.b.d.0.1.0.0.2.ip6.arpa."
        )
    }

    #[test]
    fn test_update_message() {
        // Message captured from a running Kea/Bind9 DDNS update (for reference).
        //let from_kea = BASE64_STANDARD.decode("wbYoAAABAAEAAgABA2xhYgAABgABCG91dHJpZGVywAwA/wD+AAAAAAAAwBUAAQABAAAEsAAECkVFNMAVADEAAQAABLAAIwABAasTowCr6hY874Nyfq0krHOxnvk5GwMgYIi6N1UY6lTRCGtlYS1iaW5kAAD6AP8AAAAAAD0LaG1hYy1zaGEyNTYAAABpbnyOASwAIB0XNv7B7IFpFMfsWXNH4jrSjqApS61geEUuVlin/bPBMsUAAAAA").unwrap();
        //let message = Message::from_vec(from_kea.as_slice());
        //println!("{:#?}", message);
        let zone_origin = Name::from_ascii("lab.").unwrap();
        let name = Name::from_ascii("outrider").unwrap();

        let dhcid = DhcId::new(IdType::ClientId, hex::decode("010708090a0b0c").unwrap());
        let address = Ipv4Addr::new(10, 10, 10, 10);

        // Assert the shape and values of most of the request packet.
        // TSIG is applied by Hickory if needed right before the packet is sent.
        let update = update(
            zone_origin.clone(),
            name.clone(),
            dhcid.clone(),
            address,
            1800,
            false,
        )
        .unwrap();
        assert_eq!(update.message_type(), Query);
        assert_eq!(update.op_code(), OpCode::Update);
        let queries = update.queries();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].name(), &zone_origin);
        let prerequisites = update.prerequisites();
        assert_eq!(prerequisites.len(), 1);
        let answer_record = prerequisites[0].clone();
        assert_eq!(answer_record.name(), &name);
        let name_servers = update.name_servers();
        assert_eq!(name_servers.len(), 2);
        let name_server_1 = name_servers[0].clone();
        assert_eq!(name_server_1.name(), &name);
        assert_eq!(name_server_1.dns_class(), IN);
        assert_eq!(name_server_1.ttl(), 1800);
        let name_server_1_rdata: Record = name_server_1.into_record_of_rdata();
        let should_be: Record =
            Record::from_rdata(name.clone(), 1800, A::new(10, 10, 10, 10).into_rdata());
        assert_eq!(name_server_1_rdata, should_be);
        let name_server_2 = name_servers[1].clone();
        assert_eq!(name_server_2.name(), &name);
        assert_eq!(name_server_2.dns_class(), IN);
        assert_eq!(name_server_2.ttl(), 1800);
        let name_server_2_rdata: Record = name_server_2.into_record_of_rdata();
        let should_be_2 = Record::from_rdata(
            name.clone(),
            1800,
            RData::Unknown {
                code: Unknown(49),
                rdata: NULL::with(dhcid.rdata(&name).unwrap()),
            },
        );
        assert_eq!(name_server_2_rdata, should_be_2);
    }
}

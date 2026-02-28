use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dhcproto::v6;

use crate::config::LoadTestConfig;
use crate::identity::ClientIdentity;
use crate::report::{ErrorCategory, ErrorRecord, V6ClientResult};
use crate::transport::TransportError;
use crate::transport::udp_v6::UdpV6Transport;

use super::xid_for_v6;

const SOLICIT_STAGE: u8 = 1;
const REQUEST_STAGE: u8 = 2;
const RENEW_STAGE: u8 = 3;
const RELEASE_STAGE: u8 = 4;

pub async fn run(
    client_index: usize,
    identity: &ClientIdentity,
    config: &LoadTestConfig,
    transport: Arc<UdpV6Transport>,
) -> V6ClientResult {
    let mut result = V6ClientResult::default();

    let Some(target) = config.server_v6 else {
        push_error(
            &mut result,
            ErrorCategory::Operational,
            "setup",
            "missing v6 server target",
        );
        return result;
    };

    let solicit_start = Instant::now();
    let advertise = match exchange_with_retries(
        transport.as_ref(),
        target,
        config.timeout(),
        config.retries,
        |attempt| build_solicit(identity, xid_for_v6(client_index, SOLICIT_STAGE, attempt)),
    )
    .await
    {
        Ok(msg) => msg,
        Err(err) => {
            push_transport_error(&mut result, "solicit", err);
            return result;
        }
    };
    result.advertise_latency_ms = Some(solicit_start.elapsed().as_millis());

    if advertise.msg_type() != v6::MessageType::Advertise {
        push_error(
            &mut result,
            ErrorCategory::UnexpectedMessageType,
            "advertise",
            format!("expected Advertise, got {:?}", advertise.msg_type()),
        );
        return result;
    }

    let Some(server_id) = extract_server_id(&advertise) else {
        push_error(
            &mut result,
            ErrorCategory::MalformedResponse,
            "advertise",
            "advertise missing ServerId",
        );
        return result;
    };

    let Some(advertised_ip) = extract_ia_addr(&advertise) else {
        push_error(
            &mut result,
            ErrorCategory::MalformedResponse,
            "advertise",
            "advertise missing IAAddr",
        );
        return result;
    };
    result.advertised_ip = Some(advertised_ip.to_string());

    let request_start = Instant::now();
    let reply = match exchange_with_retries(
        transport.as_ref(),
        target,
        config.timeout(),
        config.retries,
        |attempt| {
            build_request(
                identity,
                xid_for_v6(client_index, REQUEST_STAGE, attempt),
                &server_id,
                advertised_ip,
            )
        },
    )
    .await
    {
        Ok(msg) => msg,
        Err(err) => {
            push_transport_error(&mut result, "request", err);
            return result;
        }
    };
    result.reply_latency_ms = Some(request_start.elapsed().as_millis());

    if reply.msg_type() != v6::MessageType::Reply {
        push_error(
            &mut result,
            ErrorCategory::UnexpectedMessageType,
            "reply",
            format!("expected Reply, got {:?}", reply.msg_type()),
        );
        return result;
    }

    let Some(lease_ip) = extract_ia_addr(&reply) else {
        push_error(
            &mut result,
            ErrorCategory::MalformedResponse,
            "reply",
            "reply missing IAAddr",
        );
        return result;
    };
    result.leased_ip = Some(lease_ip.to_string());

    if config.renew {
        let renew_start = Instant::now();
        let renew_reply = match exchange_with_retries(
            transport.as_ref(),
            target,
            config.timeout(),
            config.retries,
            |attempt| {
                build_renew(
                    identity,
                    xid_for_v6(client_index, RENEW_STAGE, attempt),
                    &server_id,
                    lease_ip,
                )
            },
        )
        .await
        {
            Ok(msg) => msg,
            Err(err) => {
                push_transport_error(&mut result, "renew", err);
                result.success = false;
                return result;
            }
        };
        result.renew_latency_ms = Some(renew_start.elapsed().as_millis());

        if renew_reply.msg_type() != v6::MessageType::Reply {
            push_error(
                &mut result,
                ErrorCategory::UnexpectedMessageType,
                "renew",
                format!("expected Reply on renew, got {:?}", renew_reply.msg_type()),
            );
            result.success = false;
            return result;
        }

        let Some(renew_ip) = extract_ia_addr(&renew_reply) else {
            push_error(
                &mut result,
                ErrorCategory::MalformedResponse,
                "renew",
                "renew reply missing IAAddr",
            );
            result.success = false;
            return result;
        };
        result.renew_ip = Some(renew_ip.to_string());

        if renew_ip != lease_ip && !config.allow_renew_reassign {
            push_error(
                &mut result,
                ErrorCategory::RenewalMismatch,
                "renew",
                format!("renew changed lease {} -> {}", lease_ip, renew_ip),
            );
        }
    }

    if config.release {
        let release_msg = build_release(
            identity,
            xid_for_v6(client_index, RELEASE_STAGE, 0),
            &server_id,
            lease_ip,
        );
        if let Err(err) = transport.send(&release_msg, target).await {
            push_transport_error(&mut result, "release", err);
        } else {
            result.released = true;
        }
    }

    result.success = result.errors.is_empty() && result.leased_ip.is_some();
    result
}

async fn exchange_with_retries<F>(
    transport: &UdpV6Transport,
    target: SocketAddr,
    timeout: Duration,
    retries: usize,
    mut build_message: F,
) -> Result<v6::Message, TransportError>
where
    F: FnMut(usize) -> v6::Message,
{
    let mut last_error = None;

    for attempt in 0..=retries {
        let msg = build_message(attempt);
        match transport.exchange(&msg, target, timeout).await {
            Ok(resp) => return Ok(resp),
            Err(err) => last_error = Some(err),
        }
    }

    Err(last_error.unwrap_or(TransportError::ChannelClosed))
}

fn build_solicit(identity: &ClientIdentity, xid: [u8; 3]) -> v6::Message {
    let mut msg = v6::Message::new_with_id(v6::MessageType::Solicit, xid);
    msg.opts_mut()
        .insert(v6::DhcpOption::ClientId(identity.duid.clone()));
    msg.opts_mut().insert(v6::DhcpOption::IANA(v6::IANA {
        id: identity.iaid,
        t1: 0,
        t2: 0,
        opts: v6::DhcpOptions::new(),
    }));
    msg
}

fn build_request(
    identity: &ClientIdentity,
    xid: [u8; 3],
    server_id: &[u8],
    requested_addr: Ipv6Addr,
) -> v6::Message {
    let mut msg = v6::Message::new_with_id(v6::MessageType::Request, xid);
    msg.opts_mut()
        .insert(v6::DhcpOption::ClientId(identity.duid.clone()));
    msg.opts_mut()
        .insert(v6::DhcpOption::ServerId(server_id.to_vec()));

    let mut iana = v6::IANA {
        id: identity.iaid,
        t1: 0,
        t2: 0,
        opts: v6::DhcpOptions::new(),
    };
    iana.opts.insert(v6::DhcpOption::IAAddr(v6::IAAddr {
        addr: requested_addr,
        preferred_life: 0,
        valid_life: 0,
        opts: v6::DhcpOptions::new(),
    }));
    msg.opts_mut().insert(v6::DhcpOption::IANA(iana));

    msg
}

fn build_renew(
    identity: &ClientIdentity,
    xid: [u8; 3],
    server_id: &[u8],
    lease_ip: Ipv6Addr,
) -> v6::Message {
    let mut msg = v6::Message::new_with_id(v6::MessageType::Renew, xid);
    msg.opts_mut()
        .insert(v6::DhcpOption::ClientId(identity.duid.clone()));
    msg.opts_mut()
        .insert(v6::DhcpOption::ServerId(server_id.to_vec()));

    let mut iana = v6::IANA {
        id: identity.iaid,
        t1: 0,
        t2: 0,
        opts: v6::DhcpOptions::new(),
    };
    iana.opts.insert(v6::DhcpOption::IAAddr(v6::IAAddr {
        addr: lease_ip,
        preferred_life: 0,
        valid_life: 0,
        opts: v6::DhcpOptions::new(),
    }));
    msg.opts_mut().insert(v6::DhcpOption::IANA(iana));

    msg
}

fn build_release(
    identity: &ClientIdentity,
    xid: [u8; 3],
    server_id: &[u8],
    lease_ip: Ipv6Addr,
) -> v6::Message {
    let mut msg = v6::Message::new_with_id(v6::MessageType::Release, xid);
    msg.opts_mut()
        .insert(v6::DhcpOption::ClientId(identity.duid.clone()));
    msg.opts_mut()
        .insert(v6::DhcpOption::ServerId(server_id.to_vec()));

    let mut iana = v6::IANA {
        id: identity.iaid,
        t1: 0,
        t2: 0,
        opts: v6::DhcpOptions::new(),
    };
    iana.opts.insert(v6::DhcpOption::IAAddr(v6::IAAddr {
        addr: lease_ip,
        preferred_life: 0,
        valid_life: 0,
        opts: v6::DhcpOptions::new(),
    }));
    msg.opts_mut().insert(v6::DhcpOption::IANA(iana));

    msg
}

fn extract_server_id(msg: &v6::Message) -> Option<Vec<u8>> {
    if let Some(v6::DhcpOption::ServerId(id)) = msg.opts().get(v6::OptionCode::ServerId) {
        Some(id.clone())
    } else {
        None
    }
}

fn extract_ia_addr(msg: &v6::Message) -> Option<Ipv6Addr> {
    if let Some(v6::DhcpOption::IANA(iana)) = msg.opts().get(v6::OptionCode::IANA)
        && let Some(v6::DhcpOption::IAAddr(ia_addr)) = iana.opts.get(v6::OptionCode::IAAddr)
    {
        Some(ia_addr.addr)
    } else {
        None
    }
}

fn push_transport_error(result: &mut V6ClientResult, phase: &str, err: TransportError) {
    let category = if matches!(err, TransportError::Timeout(_)) {
        ErrorCategory::Timeout
    } else {
        ErrorCategory::Operational
    };
    push_error(result, category, phase, err.to_string());
}

fn push_error(
    result: &mut V6ClientResult,
    category: ErrorCategory,
    phase: &str,
    message: impl Into<String>,
) {
    result.errors.push(ErrorRecord {
        category,
        phase: phase.to_string(),
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use dhcproto::v6;

    use crate::identity::IdentityGenerator;

    use super::{build_request, build_solicit, extract_ia_addr};

    #[test]
    fn build_solicit_sets_required_options() {
        let identity = IdentityGenerator::new(1).identity(0);
        let msg = build_solicit(&identity, [0x12, 0x34, 0x56]);

        assert_eq!(msg.msg_type(), v6::MessageType::Solicit);
        assert!(matches!(
            msg.opts().get(v6::OptionCode::ClientId),
            Some(v6::DhcpOption::ClientId(_))
        ));
        assert!(matches!(
            msg.opts().get(v6::OptionCode::IANA),
            Some(v6::DhcpOption::IANA(_))
        ));
    }

    #[test]
    fn extract_ia_addr_reads_nested_iaaddr() {
        let identity = IdentityGenerator::new(1).identity(0);
        let ip = "2001:db8::22".parse().unwrap();
        let msg = build_request(&identity, [0, 0, 100], &[1, 2, 3], ip);
        assert_eq!(extract_ia_addr(&msg), Some(ip));
    }
}

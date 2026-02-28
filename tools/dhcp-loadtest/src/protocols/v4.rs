use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dhcproto::v4;

use crate::config::LoadTestConfig;
use crate::identity::ClientIdentity;
use crate::report::{ErrorCategory, ErrorRecord, V4ClientResult};
use crate::transport::TransportError;
use crate::transport::udp_v4::UdpV4Transport;

use super::xid_for;

const DISCOVER_STAGE: u8 = 1;
const RENEW_STAGE: u8 = 3;
const RELEASE_STAGE: u8 = 4;

pub async fn run(
    client_index: usize,
    identity: &ClientIdentity,
    config: &LoadTestConfig,
    transport: Arc<UdpV4Transport>,
) -> V4ClientResult {
    let mut result = V4ClientResult::default();

    let Some(target) = config.server_v4 else {
        push_error(
            &mut result,
            ErrorCategory::Operational,
            "setup",
            "missing v4 server target",
        );
        return result;
    };

    let discover_start = Instant::now();
    let offer = match exchange_with_retries(
        transport.as_ref(),
        target,
        config.timeout(),
        config.retries,
        |attempt| build_discover(identity, xid_for(client_index, DISCOVER_STAGE, attempt)),
    )
    .await
    {
        Ok(msg) => msg,
        Err(err) => {
            push_transport_error(&mut result, "discover", err);
            return result;
        }
    };
    result.offer_latency_ms = Some(discover_start.elapsed().as_millis());

    if offer.opts().msg_type() != Some(v4::MessageType::Offer) {
        push_error(
            &mut result,
            ErrorCategory::UnexpectedMessageType,
            "offer",
            format!("expected Offer, got {:?}", offer.opts().msg_type()),
        );
        return result;
    }

    let offered_ip = offer.yiaddr();
    if offered_ip.is_unspecified() {
        push_error(
            &mut result,
            ErrorCategory::MalformedResponse,
            "offer",
            "offer missing yiaddr",
        );
        return result;
    }
    result.offered_ip = Some(offered_ip.to_string());

    let Some(server_id) = extract_server_id(&offer) else {
        push_error(
            &mut result,
            ErrorCategory::MalformedResponse,
            "offer",
            "offer missing server identifier",
        );
        return result;
    };

    let ack_start = Instant::now();
    let request_xid = offer.xid();
    let ack = match exchange_with_retries(
        transport.as_ref(),
        target,
        config.timeout(),
        config.retries,
        |_| build_request_selecting(identity, request_xid, offered_ip, server_id),
    )
    .await
    {
        Ok(msg) => msg,
        Err(err) => {
            push_transport_error(&mut result, "request", err);
            return result;
        }
    };
    result.ack_latency_ms = Some(ack_start.elapsed().as_millis());

    if ack.opts().msg_type() != Some(v4::MessageType::Ack) {
        push_error(
            &mut result,
            ErrorCategory::UnexpectedMessageType,
            "ack",
            format!("expected Ack, got {:?}", ack.opts().msg_type()),
        );
        return result;
    }

    let lease_ip = if ack.yiaddr().is_unspecified() {
        offered_ip
    } else {
        ack.yiaddr()
    };
    result.leased_ip = Some(lease_ip.to_string());
    result.boot_file = extract_boot_file(&ack).or_else(|| extract_boot_file(&offer));
    result.next_server = extract_next_server(&ack).or_else(|| extract_next_server(&offer));

    if config.renew {
        let renew_target = SocketAddr::from((server_id, dhcproto::v4::SERVER_PORT));
        let renew_start = Instant::now();
        let renew_ack = match exchange_with_retries(
            transport.as_ref(),
            renew_target,
            config.timeout(),
            config.retries,
            |attempt| {
                build_request_renew(
                    identity,
                    xid_for(client_index, RENEW_STAGE, attempt),
                    lease_ip,
                    Some(server_id),
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

        if renew_ack.opts().msg_type() != Some(v4::MessageType::Ack) {
            push_error(
                &mut result,
                ErrorCategory::UnexpectedMessageType,
                "renew",
                format!(
                    "expected Ack on renew, got {:?}",
                    renew_ack.opts().msg_type()
                ),
            );
            result.success = false;
            return result;
        }

        let renew_ip = if renew_ack.yiaddr().is_unspecified() {
            lease_ip
        } else {
            renew_ack.yiaddr()
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
        let release_target = SocketAddr::from((server_id, dhcproto::v4::SERVER_PORT));
        let release_msg = build_release(
            identity,
            xid_for(client_index, RELEASE_STAGE, 0),
            lease_ip,
            server_id,
        );
        if let Err(err) = transport.send(&release_msg, release_target).await {
            push_transport_error(&mut result, "release", err);
        } else {
            result.released = true;
        }
    }

    result.success = result.errors.is_empty() && result.leased_ip.is_some();
    result
}

async fn exchange_with_retries<F>(
    transport: &UdpV4Transport,
    target: SocketAddr,
    timeout: Duration,
    retries: usize,
    mut build_message: F,
) -> Result<v4::Message, TransportError>
where
    F: FnMut(usize) -> v4::Message,
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

fn build_discover(identity: &ClientIdentity, xid: u32) -> v4::Message {
    let mut msg = v4::Message::new_with_id(
        xid,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        &identity.mac,
    );

    msg.set_flags(v4::Flags::default().set_broadcast());
    msg.opts_mut()
        .insert(v4::DhcpOption::MessageType(v4::MessageType::Discover));
    msg.opts_mut()
        .insert(v4::DhcpOption::ClientIdentifier(identity.mac.to_vec()));
    msg.opts_mut()
        .insert(v4::DhcpOption::ParameterRequestList(vec![
            v4::OptionCode::SubnetMask,
            v4::OptionCode::Router,
            v4::OptionCode::DomainNameServer,
            v4::OptionCode::DomainName,
        ]));
    msg
}

fn build_request_selecting(
    identity: &ClientIdentity,
    xid: u32,
    requested_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> v4::Message {
    let mut msg = v4::Message::new_with_id(
        xid,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        &identity.mac,
    );

    msg.set_flags(v4::Flags::default().set_broadcast());

    msg.opts_mut()
        .insert(v4::DhcpOption::MessageType(v4::MessageType::Request));
    msg.opts_mut()
        .insert(v4::DhcpOption::ClientIdentifier(identity.mac.to_vec()));
    msg.opts_mut()
        .insert(v4::DhcpOption::RequestedIpAddress(requested_ip));
    msg.opts_mut()
        .insert(v4::DhcpOption::ServerIdentifier(server_id));
    msg
}

fn build_request_renew(
    identity: &ClientIdentity,
    xid: u32,
    lease_ip: Ipv4Addr,
    server_id: Option<Ipv4Addr>,
) -> v4::Message {
    let mut msg = v4::Message::new_with_id(
        xid,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        &identity.mac,
    );

    msg.set_flags(v4::Flags::default().set_broadcast());

    msg.opts_mut()
        .insert(v4::DhcpOption::MessageType(v4::MessageType::Request));
    msg.opts_mut()
        .insert(v4::DhcpOption::ClientIdentifier(identity.mac.to_vec()));
    msg.opts_mut()
        .insert(v4::DhcpOption::RequestedIpAddress(lease_ip));
    if let Some(server_id) = server_id {
        msg.opts_mut()
            .insert(v4::DhcpOption::ServerIdentifier(server_id));
    }
    msg
}

fn build_release(
    identity: &ClientIdentity,
    xid: u32,
    lease_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> v4::Message {
    let mut msg = v4::Message::new_with_id(
        xid,
        lease_ip,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        &identity.mac,
    );

    msg.opts_mut()
        .insert(v4::DhcpOption::MessageType(v4::MessageType::Release));
    msg.opts_mut()
        .insert(v4::DhcpOption::ClientIdentifier(identity.mac.to_vec()));
    msg.opts_mut()
        .insert(v4::DhcpOption::ServerIdentifier(server_id));
    msg
}

fn extract_server_id(msg: &v4::Message) -> Option<Ipv4Addr> {
    if let Some(&v4::DhcpOption::ServerIdentifier(ip)) =
        msg.opts().get(v4::OptionCode::ServerIdentifier)
    {
        Some(ip)
    } else {
        None
    }
}

fn extract_boot_file(msg: &v4::Message) -> Option<String> {
    msg.fname().and_then(|bytes| {
        if bytes.is_empty() {
            return None;
        }
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        if end == 0 {
            None
        } else {
            Some(String::from_utf8_lossy(&bytes[..end]).to_string())
        }
    })
}

fn extract_next_server(msg: &v4::Message) -> Option<String> {
    let ip = msg.siaddr();
    if ip.is_unspecified() {
        None
    } else {
        Some(ip.to_string())
    }
}

fn push_transport_error(result: &mut V4ClientResult, phase: &str, err: TransportError) {
    let category = if matches!(err, TransportError::Timeout(_)) {
        ErrorCategory::Timeout
    } else {
        ErrorCategory::Operational
    };
    push_error(result, category, phase, err.to_string());
}

fn push_error(
    result: &mut V4ClientResult,
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
    use std::net::Ipv4Addr;

    use dhcproto::v4;

    use crate::identity::IdentityGenerator;

    use super::{build_discover, build_request_selecting, extract_boot_file, extract_next_server};

    #[test]
    fn build_discover_sets_message_type_and_client_id() {
        let identity = IdentityGenerator::new(1).identity(0);
        let msg = build_discover(&identity, 42);

        assert_eq!(msg.xid(), 42);
        assert_eq!(msg.opts().msg_type(), Some(v4::MessageType::Discover));
        assert!(msg.opts().get(v4::OptionCode::ClientIdentifier).is_some());
    }

    #[test]
    fn build_request_selecting_sets_requested_ip_and_server_id() {
        let identity = IdentityGenerator::new(1).identity(0);
        let req_ip: Ipv4Addr = "192.168.2.55".parse().unwrap();
        let srv_ip: Ipv4Addr = "192.168.2.1".parse().unwrap();
        let msg = build_request_selecting(&identity, 100, req_ip, srv_ip);

        assert_eq!(msg.opts().msg_type(), Some(v4::MessageType::Request));
        assert!(msg.flags().broadcast());
        assert!(matches!(
            msg.opts().get(v4::OptionCode::RequestedIpAddress),
            Some(&v4::DhcpOption::RequestedIpAddress(ip)) if ip == req_ip
        ));
        assert!(matches!(
            msg.opts().get(v4::OptionCode::ServerIdentifier),
            Some(&v4::DhcpOption::ServerIdentifier(ip)) if ip == srv_ip
        ));
    }

    #[test]
    fn extracts_boot_file_and_next_server_from_response() {
        let mut msg = v4::Message::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            "10.0.0.11".parse().unwrap(),
            Ipv4Addr::UNSPECIFIED,
            &[0x02, 0, 0, 0, 0, 1],
        );
        msg.set_fname_str("host-special.ipxe");

        assert_eq!(
            extract_boot_file(&msg).as_deref(),
            Some("host-special.ipxe")
        );
        assert_eq!(extract_next_server(&msg).as_deref(), Some("10.0.0.11"));
    }
}

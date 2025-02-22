use pnet::packet::{Packet, PrimitiveValues, icmp, icmpv6, ipv4};

use crate::{DEFAULT_TOKEN_SIZE, Token};

pub const ICMP_HEADER_SIZE: usize = 8;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("invalid size")]
    InvalidSize,
    #[error("invalid packet")]
    InvalidPacket,
    #[error("ipv4 packet failed")]
    BadIpv4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icmpv4;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icmpv6;

pub trait Proto {}

impl Proto for Icmpv4 {}
impl Proto for Icmpv6 {}

pub trait Encode<P: Proto> {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), Error>;
}

#[derive(Debug, Clone)]
pub struct EchoRequest<'a> {
    pub ident: u16,
    pub seq_cnt: u16,
    pub payload: &'a [u8],
}

impl PartialEq for EchoRequest<'_> {
    fn eq(&self, other: &Self) -> bool {
        // ident potentially will be altered by the kernel because we use DGRAM
        self.seq_cnt == other.seq_cnt && self.payload == other.payload
    }
}

// compare equality with EchoReply
impl PartialEq<EchoReply> for EchoRequest<'_> {
    fn eq(&self, other: &EchoReply) -> bool {
        self.seq_cnt == other.seq_cnt && self.payload == other.payload
    }
}

impl Encode<Icmpv4> for EchoRequest<'_> {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), Error> {
        let mut packet =
            icmp::echo_request::MutableEchoRequestPacket::new(buffer).ok_or(Error::InvalidSize)?;
        packet.set_icmp_type(icmp::IcmpTypes::EchoRequest);
        packet.set_identifier(self.ident);
        packet.set_sequence_number(self.seq_cnt);
        packet.set_payload(self.payload);

        let checksum =
            icmp::checksum(&icmp::IcmpPacket::new(packet.packet()).ok_or(Error::InvalidSize)?);
        packet.set_checksum(checksum);
        Ok(())
    }
}

impl Encode<Icmpv6> for EchoRequest<'_> {
    fn encode(&self, buffer: &mut [u8]) -> Result<(), Error> {
        // icmpv6::MutableIcmpv6Packet does not have a way to set ident and seq_cnt, so we'll do it manually here
        // set type
        buffer[0] = icmpv6::Icmpv6Types::EchoRequest.to_primitive_values().0;
        // set code
        buffer[1] = 0;
        // set ident
        buffer[4..=5].copy_from_slice(&self.ident.to_be_bytes());
        // set seq_cnt
        buffer[6..=7].copy_from_slice(&self.seq_cnt.to_be_bytes());
        // add our payload
        buffer[8..].copy_from_slice(self.payload);

        let checksum = icmp::checksum(&icmp::IcmpPacket::new(buffer).ok_or(Error::InvalidSize)?);
        buffer[2..=3].copy_from_slice(&checksum.to_be_bytes());
        Ok(())
    }
}

pub trait Decode<P: Proto>: Sized {
    fn decode(buffer: &[u8], decode_header: bool) -> Result<Self, Error>;
}

#[derive(Debug, Clone)]
pub struct EchoReply {
    pub ident: u16,
    pub seq_cnt: u16,
    pub payload: Token,
}

impl PartialEq for EchoReply {
    fn eq(&self, other: &Self) -> bool {
        // ident potentially will be altered by the kernel because we use DGRAM
        self.seq_cnt == other.seq_cnt && self.payload == other.payload
    }
}

// compare equality with EchoRequest
impl PartialEq<EchoRequest<'_>> for EchoReply {
    fn eq(&self, other: &EchoRequest) -> bool {
        self.seq_cnt == other.seq_cnt && self.payload == other.payload
    }
}

impl Decode<Icmpv4> for EchoReply {
    fn decode(buffer: &[u8], decode_header: bool) -> Result<Self, Error> {
        // needed for borrowck
        let ipv4_packet;
        let buffer = if decode_header {
            ipv4_packet = ipv4::Ipv4Packet::new(buffer).ok_or(Error::BadIpv4)?;
            ipv4_packet.payload()
        } else {
            buffer
        };
        let packet = icmp::echo_reply::EchoReplyPacket::new(buffer).ok_or(Error::InvalidPacket)?;
        if buffer[ICMP_HEADER_SIZE..].len() != DEFAULT_TOKEN_SIZE {
            return Err(Error::InvalidSize);
        }
        let mut payload = [0; DEFAULT_TOKEN_SIZE];
        payload.copy_from_slice(&buffer[ICMP_HEADER_SIZE..]);

        Ok(Self {
            ident: packet.get_identifier(),
            seq_cnt: packet.get_sequence_number(),
            payload,
        })
    }
}
impl Decode<Icmpv6> for EchoReply {
    fn decode(buffer: &[u8], _decode_header: bool) -> Result<Self, Error> {
        let packet = icmpv6::Icmpv6Packet::new(buffer).ok_or(Error::InvalidPacket)?;
        if !matches!(packet.get_icmpv6_type(), icmpv6::Icmpv6Types::EchoReply) {
            return Err(Error::InvalidPacket);
        }
        let icmp_payload = packet.payload();
        let ident = u16::from_be_bytes([icmp_payload[0], icmp_payload[1]]);
        let seq_cnt = u16::from_be_bytes([icmp_payload[2], icmp_payload[3]]);
        let mut payload = [0; DEFAULT_TOKEN_SIZE];
        payload.copy_from_slice(&buffer[ICMP_HEADER_SIZE..][..DEFAULT_TOKEN_SIZE]);

        Ok(Self {
            ident,
            seq_cnt,
            payload,
        })
    }
}

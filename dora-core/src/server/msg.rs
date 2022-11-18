//! SerialMsg defines raw bytes and an addr
use bytes::Bytes;
use dhcproto::{Decodable, Encodable};

use std::{io, net::SocketAddr};

// use crate::udp::UdpRecv;

/// A message pulled from TCP or UDP and serialized to bytes, stored with a
/// [`SocketAddr`]
///
/// [`SocketAddr`]: std::net::SocketAddr
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialMsg {
    message: Bytes,
    addr: SocketAddr,
}

impl SerialMsg {
    /// Construct a new `SerialMsg` and the source or destination address
    pub fn new(message: Bytes, addr: SocketAddr) -> Self {
        SerialMsg { message, addr }
    }

    /// Constructs a new `SerialMsg` from another `SerialMsg` and a `SocketAddr`
    pub fn from_msg<T: Encodable>(msg: &T, addr: SocketAddr) -> io::Result<Self> {
        Ok(SerialMsg {
            message: msg
                .to_vec()
                .map_err(|op| io::Error::new(io::ErrorKind::InvalidData, op))?
                .into(),
            addr,
        })
    }
    /// Get a reference to the bytes
    pub fn bytes(&self) -> &[u8] {
        &self.message
    }

    /// Clone underlying `Bytes` pointer
    pub fn msg(&self) -> Bytes {
        self.message.clone()
    }

    /// Get the source or destination address (context dependent)
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Set the source or destination address
    pub fn set_addr(&mut self, addr: SocketAddr) {
        self.addr = addr;
    }

    /// Gets the bytes and address as a tuple
    pub fn contents(self) -> (Bytes, SocketAddr) {
        (self.message, self.addr)
    }

    /// Deserializes the inner data into a Message
    pub fn to_msg<T: Decodable>(&self) -> io::Result<T> {
        T::from_bytes(&self.message).map_err(|op| io::Error::new(io::ErrorKind::InvalidData, op))
    }
}

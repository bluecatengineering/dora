use std::time::Duration;

use thiserror::Error;

pub mod udp_v4;
pub mod udp_v6;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode error: {0}")]
    Encode(String),
    #[error("timed out waiting for response after {0:?}")]
    Timeout(Duration),
    #[error("response channel closed")]
    ChannelClosed,
    #[error("transaction id collision for xid {0}")]
    XidCollision(String),
    #[error("address family mismatch for this transport")]
    AddressFamilyMismatch,
}

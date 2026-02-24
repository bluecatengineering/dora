//! Error types for NATS coordination operations.
//!
//! Provides typed error variants so that consumers (plugins) can distinguish
//! between transport failures, timeouts, protocol conflicts, and codec issues
//! without leaking NATS internals.

use thiserror::Error;

/// Top-level error type for the nats-coordination crate.
#[derive(Debug, Error)]
pub enum CoordinationError {
    /// NATS connection or transport-level failure.
    #[error("transport error: {0}")]
    Transport(String),

    /// Operation timed out waiting for a response.
    #[error("timeout: {0}")]
    Timeout(String),

    /// Revision conflict detected during an optimistic update.
    /// Contains the expected revision that was stale.
    #[error("revision conflict: expected revision {expected}, found {actual}")]
    RevisionConflict { expected: u64, actual: u64 },

    /// Maximum retry attempts exhausted for a conflicting operation.
    #[error("max retries exhausted after {attempts} attempts")]
    MaxRetriesExhausted { attempts: u32 },

    /// Codec error during serialization or deserialization.
    #[error("codec error: {0}")]
    Codec(String),

    /// Configuration error (e.g. missing required fields).
    #[error("configuration error: {0}")]
    Config(String),

    /// The client is not connected or connection was lost.
    #[error("not connected: {0}")]
    NotConnected(String),

    /// A protocol-level error from the coordination peer.
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl CoordinationError {
    /// Returns true if this error indicates a transient failure that may
    /// succeed on retry (transport, timeout, or revision conflict).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            CoordinationError::Transport(_)
                | CoordinationError::Timeout(_)
                | CoordinationError::RevisionConflict { .. }
        )
    }

    /// Returns true if this error is a revision conflict.
    pub fn is_conflict(&self) -> bool {
        matches!(self, CoordinationError::RevisionConflict { .. })
    }

    /// Returns true if this error is a timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(self, CoordinationError::Timeout(_))
    }
}

/// Shorthand result alias for coordination operations.
pub type CoordinationResult<T> = Result<T, CoordinationError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_classification() {
        let transport = CoordinationError::Transport("conn reset".into());
        assert!(transport.is_retryable());
        assert!(!transport.is_conflict());
        assert!(!transport.is_timeout());

        let timeout = CoordinationError::Timeout("deadline exceeded".into());
        assert!(timeout.is_retryable());
        assert!(!timeout.is_conflict());
        assert!(timeout.is_timeout());

        let conflict = CoordinationError::RevisionConflict {
            expected: 3,
            actual: 5,
        };
        assert!(conflict.is_retryable());
        assert!(conflict.is_conflict());
        assert!(!conflict.is_timeout());

        let retries = CoordinationError::MaxRetriesExhausted { attempts: 5 };
        assert!(!retries.is_retryable());

        let codec = CoordinationError::Codec("bad json".into());
        assert!(!codec.is_retryable());

        let config = CoordinationError::Config("missing server".into());
        assert!(!config.is_retryable());

        let not_conn = CoordinationError::NotConnected("no conn".into());
        assert!(!not_conn.is_retryable());

        let proto = CoordinationError::Protocol("unknown version".into());
        assert!(!proto.is_retryable());
    }

    #[test]
    fn test_error_display() {
        let err = CoordinationError::RevisionConflict {
            expected: 1,
            actual: 2,
        };
        let msg = format!("{err}");
        assert!(msg.contains("expected revision 1"));
        assert!(msg.contains("found 2"));
    }
}

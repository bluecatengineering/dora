//! NATS connection manager with reconnect/backoff and optional auth/encryption.
//!
//! Wraps `async-nats` to provide a resilient connection layer. Security mode
//! support is flexible: none, user/password, token, nkey, tls, and creds-file
//! modes are all optional runtime choices.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_nats::ConnectOptions;
use async_nats::jetstream;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use config::NatsConfig;
use config::wire::NatsSecurityMode;

use crate::error::{CoordinationError, CoordinationResult};
use crate::subjects::SubjectResolver;

/// Default connection timeout if not configured.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default request timeout if not configured.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_millis(2000);

/// Base delay for retrying initial NATS connections.
const CONNECT_RETRY_BASE_DELAY: Duration = Duration::from_secs(1);

/// Upper bound for retry backoff during initial NATS connect.
const MAX_CONNECT_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Connection state observable by consumers for degraded-mode checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Connected and operating normally.
    Connected,
    /// Attempting to reconnect after a failure.
    Reconnecting,
    /// Not connected; connection was never established or has been shut down.
    Disconnected,
}

/// Inner state shared behind Arc<RwLock<…>>.
struct ClientInner {
    nats_client: Option<async_nats::Client>,
    state: ConnectionState,
    config: NatsConfig,
}

/// NATS connection manager for lease coordination and host-option lookups.
///
/// Provides:
/// - Connection bootstrap from `NatsConfig`
/// - Automatic reconnection (handled by async-nats internally)
/// - Optional security mode configuration
/// - Current connection state for degraded-mode checks
/// - Publish/request helpers that map errors to typed `CoordinationError`
#[derive(Clone)]
pub struct NatsClient {
    inner: Arc<RwLock<ClientInner>>,
    resolver: SubjectResolver,
    request_timeout: Duration,
}

impl NatsClient {
    /// Create a new client from nats configuration, without connecting yet.
    ///
    /// Call [`connect`] to establish the NATS connection.
    pub fn new(config: NatsConfig, resolver: SubjectResolver) -> Self {
        let request_timeout = config.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT);
        Self {
            inner: Arc::new(RwLock::new(ClientInner {
                nats_client: None,
                state: ConnectionState::Disconnected,
                config,
            })),
            resolver,
            request_timeout,
        }
    }

    /// Build connect options from the nats config, applying the selected security mode.
    async fn build_connect_options(config: &NatsConfig) -> CoordinationResult<ConnectOptions> {
        let mut opts = ConnectOptions::new();

        // Apply security mode
        match &config.security_mode {
            NatsSecurityMode::None => {
                // No auth configuration needed
            }
            NatsSecurityMode::UserPassword => {
                let user = config.username.as_deref().ok_or_else(|| {
                    CoordinationError::Config(
                        "user_password security mode requires 'username'".into(),
                    )
                })?;
                let pass = config.password.as_deref().ok_or_else(|| {
                    CoordinationError::Config(
                        "user_password security mode requires 'password'".into(),
                    )
                })?;
                opts = opts.user_and_password(user.into(), pass.into());
            }
            NatsSecurityMode::Token => {
                let token = config.token.as_deref().ok_or_else(|| {
                    CoordinationError::Config("token security mode requires 'token'".into())
                })?;
                opts = opts.token(token.into());
            }
            NatsSecurityMode::Nkey => {
                let seed_path = config.nkey_seed_path.as_ref().ok_or_else(|| {
                    CoordinationError::Config("nkey security mode requires 'nkey_seed_path'".into())
                })?;
                let seed = std::fs::read_to_string(seed_path).map_err(|e| {
                    CoordinationError::Config(format!(
                        "failed to read nkey seed file '{}': {e}",
                        seed_path.display()
                    ))
                })?;
                let seed = seed.trim().to_string();
                opts = opts.nkey(seed);
            }
            NatsSecurityMode::Tls => {
                // TLS client cert auth
                let cert_path = config.tls_cert_path.as_ref().ok_or_else(|| {
                    CoordinationError::Config("tls security mode requires 'tls_cert_path'".into())
                })?;
                let key_path = config.tls_key_path.as_ref().ok_or_else(|| {
                    CoordinationError::Config("tls security mode requires 'tls_key_path'".into())
                })?;
                opts = opts.add_client_certificate(cert_path.clone(), key_path.clone());
                if let Some(ca_path) = &config.tls_ca_path {
                    opts = opts.add_root_certificates(ca_path.clone());
                }
                opts = opts.require_tls(true);
            }
            NatsSecurityMode::CredsFile => {
                let creds_path = config.creds_file_path.as_ref().ok_or_else(|| {
                    CoordinationError::Config(
                        "creds_file security mode requires 'creds_file_path'".into(),
                    )
                })?;
                opts = opts.credentials_file(creds_path).await.map_err(|e| {
                    CoordinationError::Config(format!(
                        "failed to load credentials file '{}': {e}",
                        creds_path.display()
                    ))
                })?;
            }
        }

        // Apply TLS CA even in non-TLS auth modes (server-side TLS verification)
        if config.security_mode != NatsSecurityMode::Tls {
            if let Some(ca_path) = &config.tls_ca_path {
                opts = opts.add_root_certificates(ca_path.clone());
                opts = opts.require_tls(true);
            }
        }

        // Connection timeout
        let connect_timeout = config.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT);
        opts = opts.connection_timeout(connect_timeout);
        opts = opts.retry_on_initial_connect();

        Ok(opts)
    }

    /// Establish the NATS connection.
    ///
    /// Uses the configured server URLs and security mode. On success, the client
    /// transitions to `Connected` state. async-nats handles automatic reconnection
    /// internally.
    pub async fn connect(&self) -> CoordinationResult<()> {
        let (config, current_state) = {
            let inner = self.inner.read().await;
            (inner.config.clone(), inner.state)
        };

        if current_state == ConnectionState::Connected {
            debug!("NATS client already connected, skipping connect");
            return Ok(());
        }

        info!(
            servers = ?config.servers,
            security_mode = ?config.security_mode,
            connect_retry_max = config.connect_retry_max,
            "connecting to NATS"
        );

        {
            let mut inner = self.inner.write().await;
            inner.nats_client = None;
            inner.state = ConnectionState::Reconnecting;
        }

        let total_attempts = config.connect_retry_max.saturating_add(1);
        for attempt in 0..total_attempts {
            let opts = match Self::build_connect_options(&config).await {
                Ok(opts) => opts,
                Err(err) => {
                    let mut inner = self.inner.write().await;
                    inner.state = ConnectionState::Disconnected;
                    return Err(err);
                }
            };

            match opts.connect(config.servers.clone()).await {
                Ok(client) => {
                    let mut inner = self.inner.write().await;
                    inner.nats_client = Some(client);
                    inner.state = ConnectionState::Connected;

                    info!(
                        attempt = attempt + 1,
                        total_attempts, "NATS connection established"
                    );
                    return Ok(());
                }
                Err(err) => {
                    let attempt_num = attempt + 1;
                    if attempt_num >= total_attempts {
                        error!(
                            attempts = total_attempts,
                            error = %err,
                            "NATS connection failed after all retry attempts"
                        );

                        let mut inner = self.inner.write().await;
                        inner.state = ConnectionState::Disconnected;
                        return Err(CoordinationError::Transport(format!(
                            "NATS connection failed after {total_attempts} attempt(s): {err}"
                        )));
                    }

                    let delay = CONNECT_RETRY_BASE_DELAY
                        .saturating_mul(2u32.saturating_pow(attempt))
                        .min(MAX_CONNECT_RETRY_DELAY);
                    warn!(
                        attempt = attempt_num,
                        total_attempts,
                        retry_in_ms = delay.as_millis(),
                        error = %err,
                        "NATS connection attempt failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }

        unreachable!("initial NATS connect loop should return on success or terminal failure")
    }

    /// Returns the current connection state.
    pub async fn connection_state(&self) -> ConnectionState {
        let inner = self.inner.read().await;
        // If we have a client, check its actual state
        if let Some(ref client) = inner.nats_client {
            match client.connection_state() {
                async_nats::connection::State::Connected => ConnectionState::Connected,
                async_nats::connection::State::Disconnected => ConnectionState::Reconnecting,
                async_nats::connection::State::Pending => ConnectionState::Reconnecting,
            }
        } else {
            inner.state
        }
    }

    /// Returns true if the client is currently connected.
    pub async fn is_connected(&self) -> bool {
        self.connection_state().await == ConnectionState::Connected
    }

    /// Returns the subject resolver.
    pub fn resolver(&self) -> &SubjectResolver {
        &self.resolver
    }

    /// Returns the configured request timeout.
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Return configured leases KV bucket name.
    pub async fn leases_bucket(&self) -> String {
        let inner = self.inner.read().await;
        inner.config.leases_bucket.clone()
    }

    /// Return configured host-options KV bucket name.
    pub async fn host_options_bucket(&self) -> String {
        let inner = self.inner.read().await;
        inner.config.host_options_bucket.clone()
    }

    /// Return configured lease GC interval.
    pub async fn lease_gc_interval(&self) -> Duration {
        let inner = self.inner.read().await;
        inner.config.lease_gc_interval
    }

    /// Run a startup write-path selftest against the lease KV bucket.
    ///
    /// This verifies that JetStream KV is reachable for write/read/delete
    /// operations before the process reports healthy.
    pub async fn startup_write_selftest(&self) -> CoordinationResult<()> {
        let bucket = self.leases_bucket().await;
        let store = self.get_or_create_kv_bucket(&bucket, 16).await?;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let probe_key = format!("startup/selftest/{nonce}");
        let probe_value = format!("dora-startup-selftest-{nonce}");

        store
            .put(&probe_key, probe_value.clone().into_bytes().into())
            .await
            .map_err(|e| {
                CoordinationError::Transport(format!(
                    "nats write selftest put failed for key '{probe_key}': {e}"
                ))
            })?;

        let stored = store.get(probe_key.clone()).await.map_err(|e| {
            CoordinationError::Transport(format!(
                "nats write selftest get failed for key '{probe_key}': {e}"
            ))
        })?;

        let Some(stored) = stored else {
            return Err(CoordinationError::Transport(format!(
                "nats write selftest get returned no value for key '{probe_key}'"
            )));
        };

        if stored.as_ref() != probe_value.as_bytes() {
            return Err(CoordinationError::Transport(format!(
                "nats write selftest value mismatch for key '{probe_key}'"
            )));
        }

        store.delete(&probe_key).await.map_err(|e| {
            CoordinationError::Transport(format!(
                "nats write selftest delete failed for key '{probe_key}': {e}"
            ))
        })?;

        info!(bucket, key = %probe_key, "nats startup write selftest passed");
        Ok(())
    }

    /// Build a JetStream context for the active NATS connection.
    pub async fn jetstream_context(&self) -> CoordinationResult<jetstream::Context> {
        let client = self.nats_client().await?;
        Ok(jetstream::new(client))
    }

    /// Get an existing KV bucket or create it if missing.
    pub async fn get_or_create_kv_bucket(
        &self,
        bucket: &str,
        history: i64,
    ) -> CoordinationResult<jetstream::kv::Store> {
        let js = self.jetstream_context().await?;
        match js.get_key_value(bucket.to_string()).await {
            Ok(store) => Ok(store),
            Err(get_err) => {
                debug!(bucket, error = %get_err, "creating missing JetStream KV bucket");
                js.create_key_value(jetstream::kv::Config {
                    bucket: bucket.to_string(),
                    history,
                    ..Default::default()
                })
                .await
                .map_err(|create_err| {
                    CoordinationError::Transport(format!(
                        "failed to create JetStream KV bucket '{bucket}': {create_err} (get error: {get_err})"
                    ))
                })
            }
        }
    }

    /// Get a reference to the underlying async-nats client.
    /// Returns an error if not connected.
    async fn nats_client(&self) -> CoordinationResult<async_nats::Client> {
        let inner = self.inner.read().await;
        inner
            .nats_client
            .clone()
            .ok_or_else(|| CoordinationError::NotConnected("NATS client not connected".into()))
    }

    /// Publish a message to a subject.
    pub async fn publish(&self, subject: &str, payload: Vec<u8>) -> CoordinationResult<()> {
        let client = self.nats_client().await?;
        client
            .publish(subject.to_string(), payload.into())
            .await
            .map_err(|e| CoordinationError::Transport(format!("publish failed: {e}")))?;
        Ok(())
    }

    /// Send a request and wait for a reply with the configured timeout.
    pub async fn request(&self, subject: &str, payload: Vec<u8>) -> CoordinationResult<Vec<u8>> {
        let client = self.nats_client().await?;

        let response = tokio::time::timeout(
            self.request_timeout,
            client.request(subject.to_string(), payload.into()),
        )
        .await
        .map_err(|_| {
            CoordinationError::Timeout(format!(
                "request to '{subject}' timed out after {:?}",
                self.request_timeout
            ))
        })?
        .map_err(|e| CoordinationError::Transport(format!("request to '{subject}' failed: {e}")))?;

        Ok(response.payload.to_vec())
    }

    /// Shut down the client, transitioning to Disconnected state.
    pub async fn disconnect(&self) {
        let mut inner = self.inner.write().await;
        inner.nats_client = None;
        inner.state = ConnectionState::Disconnected;
        info!("NATS client disconnected");
    }
}

impl std::fmt::Debug for NatsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsClient")
            .field("resolver", &self.resolver)
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::wire::{NatsSecurityMode, NatsSubjects};

    fn test_config() -> NatsConfig {
        NatsConfig {
            servers: vec!["nats://127.0.0.1:4222".into()],
            subject_prefix: "test.cluster".into(),
            subjects: NatsSubjects::default(),
            leases_bucket: "test_leases".into(),
            host_options_bucket: "test_host_options".into(),
            lease_gc_interval: Duration::from_secs(30),
            coordination_state_poll_interval: Duration::from_millis(500),
            contract_version: "1.0.0".into(),
            security_mode: NatsSecurityMode::None,
            username: None,
            password: None,
            token: None,
            nkey_seed_path: None,
            tls_cert_path: None,
            tls_key_path: None,
            tls_ca_path: None,
            creds_file_path: None,
            connect_timeout: Some(Duration::from_secs(2)),
            connect_retry_max: 2,
            request_timeout: Some(Duration::from_millis(500)),
        }
    }

    #[tokio::test]
    async fn test_build_connect_options_none() {
        let config = test_config();
        let opts = NatsClient::build_connect_options(&config).await;
        assert!(opts.is_ok());
    }

    #[tokio::test]
    async fn test_build_connect_options_user_password() {
        let mut config = test_config();
        config.security_mode = NatsSecurityMode::UserPassword;
        config.username = Some("user".into());
        config.password = Some("pass".into());
        let opts = NatsClient::build_connect_options(&config).await;
        assert!(opts.is_ok());
    }

    #[tokio::test]
    async fn test_build_connect_options_user_password_missing_username() {
        let mut config = test_config();
        config.security_mode = NatsSecurityMode::UserPassword;
        config.password = Some("pass".into());
        let result = NatsClient::build_connect_options(&config).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CoordinationError::Config(_)));
    }

    #[tokio::test]
    async fn test_build_connect_options_user_password_missing_password() {
        let mut config = test_config();
        config.security_mode = NatsSecurityMode::UserPassword;
        config.username = Some("user".into());
        let result = NatsClient::build_connect_options(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_connect_options_token() {
        let mut config = test_config();
        config.security_mode = NatsSecurityMode::Token;
        config.token = Some("my-token".into());
        let opts = NatsClient::build_connect_options(&config).await;
        assert!(opts.is_ok());
    }

    #[tokio::test]
    async fn test_build_connect_options_token_missing() {
        let mut config = test_config();
        config.security_mode = NatsSecurityMode::Token;
        let result = NatsClient::build_connect_options(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_connect_options_nkey_missing_path() {
        let mut config = test_config();
        config.security_mode = NatsSecurityMode::Nkey;
        let result = NatsClient::build_connect_options(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_connect_options_tls_missing_cert() {
        let mut config = test_config();
        config.security_mode = NatsSecurityMode::Tls;
        config.tls_key_path = Some("/tmp/key.pem".into());
        let result = NatsClient::build_connect_options(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_connect_options_tls_missing_key() {
        let mut config = test_config();
        config.security_mode = NatsSecurityMode::Tls;
        config.tls_cert_path = Some("/tmp/cert.pem".into());
        let result = NatsClient::build_connect_options(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_connect_options_creds_missing_path() {
        let mut config = test_config();
        config.security_mode = NatsSecurityMode::CredsFile;
        let result = NatsClient::build_connect_options(&config).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_client_initial_state() {
        let config = test_config();
        let resolver = SubjectResolver::with_defaults();
        let client = NatsClient::new(config, resolver);
        assert_eq!(
            client.connection_state().await,
            ConnectionState::Disconnected
        );
        assert!(!client.is_connected().await);
    }

    #[tokio::test]
    async fn test_publish_without_connection_fails() {
        let config = test_config();
        let resolver = SubjectResolver::with_defaults();
        let client = NatsClient::new(config, resolver);
        let result = client.publish("test.subject", b"hello".to_vec()).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            CoordinationError::NotConnected(_)
        ));
    }

    #[tokio::test]
    async fn test_request_without_connection_fails() {
        let config = test_config();
        let resolver = SubjectResolver::with_defaults();
        let client = NatsClient::new(config, resolver);
        let result = client.request("test.subject", b"hello".to_vec()).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            CoordinationError::NotConnected(_)
        ));
    }

    #[tokio::test]
    async fn test_disconnect() {
        let config = test_config();
        let resolver = SubjectResolver::with_defaults();
        let client = NatsClient::new(config, resolver);
        client.disconnect().await;
        assert_eq!(
            client.connection_state().await,
            ConnectionState::Disconnected
        );
    }

    #[test]
    fn test_request_timeout_from_config() {
        let config = test_config();
        let resolver = SubjectResolver::with_defaults();
        let client = NatsClient::new(config, resolver);
        assert_eq!(client.request_timeout(), Duration::from_millis(500));
    }

    #[test]
    fn test_request_timeout_default() {
        let mut config = test_config();
        config.request_timeout = None;
        let resolver = SubjectResolver::with_defaults();
        let client = NatsClient::new(config, resolver);
        assert_eq!(client.request_timeout(), DEFAULT_REQUEST_TIMEOUT);
    }
}

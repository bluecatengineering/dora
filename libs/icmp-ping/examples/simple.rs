use icmp_ping::{Icmpv4, Listener};
use tracing::{error, info};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let host = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let ip = tokio::net::lookup_host(format!("{}:0", host))
        .await
        .expect("host lookup error")
        .next()
        .map(|val| val.ip())
        .unwrap();

    let listener = Listener::<Icmpv4>::new().unwrap();
    let pinger = listener.pinger(ip);
    match pinger.ping(0).await {
        Ok(reply) => {
            info!(reply = ?reply.reply, time = ?reply.time);
        }
        Err(err) => error!(?err),
    };
}

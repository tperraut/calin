//! Runs the tests' OpenID Connect provider on a loopback port, for trying
//! `calin serve` in public mode by hand. Never for real use: it signs in
//! whoever asks, as the user named on the command line.
//!
//! `cargo run -p calin-auth --features test-provider --example test-provider -- \
//!     [port] [client-id] [email]`
//!
//! Prints the issuer URL, then serves until killed.

use std::sync::Arc;

use calin_auth::SystemClock;
use calin_auth::test_provider::TestProvider;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|port| port.parse().ok()).unwrap_or(0);
    let client_id = args.next().unwrap_or_else(|| "calin-dev".to_owned());
    let email = args
        .next()
        .unwrap_or_else(|| "alice@example.com".to_owned());

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let provider = TestProvider::start_on(listener, Arc::new(SystemClock), &client_id, None);
    provider.set_user("user-1", Some(&email), true);
    println!("{}", provider.issuer());
    std::future::pending::<()>().await;
    Ok(())
}

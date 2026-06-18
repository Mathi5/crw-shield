use std::net::SocketAddr;

use crw_core::Config;
use crw_server::{build_router, AppState};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let config = Config::from_env()?;
    let bind_addr = config.bind_addr();
    let state = AppState::from_config(config);

    tracing::info!(addr = %bind_addr, "crw-shield starting");

    let app = build_router(state);
    let addr: SocketAddr = bind_addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

use std::net::SocketAddr;

use crw_core::Config;
use crw_server::{build_router, AppState};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The chromiumoxide driver emits a cosmetic CDP warning on every fetch:
    //   "WS Invalid message: data did not match any variant of untagged enum
    //    Message"
    // (Chrome 120+ sends CDP events that the chromiumoxide_cdp parser
    // doesn't recognise. Harmless but floods the logs.) We silence both
    // crate targets unconditionally — this filter must NOT be overridable
    // via RUST_LOG, otherwise users who set `RUST_LOG=info` (e.g. our
    // docker-compose) will re-enable the noise. We do honour user RUST_LOG
    // for every OTHER target.
    let user_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let combined = user_filter
        .add_directive("chromiumoxide=off".parse().expect("valid directive"))
        .add_directive("chromiumoxide_cdp=off".parse().expect("valid directive"));
    tracing_subscriber::fmt()
        .with_env_filter(combined)
        .with_target(false)
        .init();

    let config = Config::from_env()?;
    let bind_addr = config.bind_addr();
    let state = AppState::from_config_async(config).await;

    tracing::info!(addr = %bind_addr, "crw-shield starting");

    let app = build_router(state.clone());
    let addr: SocketAddr = bind_addr.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    // Graceful shutdown: on SIGTERM/SIGINT, kill the TLS-impersonate-proxy
    // sidecar (if running) before exiting. Without this, the proxy orphan
    // would survive a container restart and port 7890 would stay bound.
    let shutdown_state = state.clone();
    let shutdown = async move {
        let ctrl_c = async {
            tokio::signal::ctrl_c().await.ok();
        };
        #[cfg(unix)]
        let _terminate = async {};
        #[cfg(unix)]
        {
            use tokio::signal::unix::SignalKind;
            let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => tracing::info!("received SIGINT, shutting down"),
                _ = sigterm.recv() => tracing::info!("received SIGTERM, shutting down"),
            }
        }
        #[cfg(not(unix))]
        {
            ctrl_c.await;
            tracing::info!("received Ctrl-C, shutting down");
        }

        if let Some(proxy) = shutdown_state.tls_proxy.as_ref() {
            tracing::info!("killing tls-impersonate-proxy sidecar");
            proxy.kill().await;
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

//! `super-router-server`: the Super Router data plane. See [`super_router_cli`] for the environment variables.

use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();
    let shutdown = CancellationToken::new();
    tokio::spawn(super_router_cli::signal_shutdown(shutdown.clone()));
    if let Err(e) = match super_router_cli::Config::from_env(&|name| std::env::var(name).ok()) {
        Ok(cfg) => super_router_cli::run(cfg, shutdown, None).await,
        Err(e) => Err(e),
    } {
        tracing::error!(error = %e, "server stopped");
        std::process::exit(1);
    }
}

//! # horizon-core
//!
//! Reusable building blocks for [The Graph Horizon](https://thegraph.com) data
//! services. Almost every Horizon data service needs the same payment plumbing:
//!
//! - **TAP v2 (GraphTally)** receipt types, EIP-712 hashing, and validation ([`tap`])
//! - **RAV aggregation** against a TAP aggregator ([`aggregator`])
//! - **On-chain collection** via the shared `collect(address,uint8,bytes)` ABI ([`collector`])
//! - **Persistence** of receipts and RAVs, with bundled migrations ([`db`])
//! - **Config + known addresses** ([`config`], [`addresses`])
//! - A **generic TAP-gated reverse proxy** ([`proxy`])
//!
//! A complete gateway can be as small as:
//!
//! ```no_run
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     horizon_core::run(horizon_core::Config::load()?).await
//! }
//! ```
//!
//! Services that need extra routes can instead use [`build_state`],
//! [`standard_router`], and [`spawn_background`] directly.

use std::sync::Arc;

use alloy_primitives::B256;
use axum::{extract::State, http::StatusCode, routing::{any, get}, Router};
use reqwest::Client;
use std::net::SocketAddr;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};

pub mod addresses;
pub mod aggregator;
pub mod collector;
pub mod config;
pub mod db;
pub mod proxy;
pub mod tap;

pub use config::Config;
pub use db::Pool;

/// Shared state injected into every Axum handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: Pool,
    pub http_client: Client,
    pub domain_sep: B256,
}

/// Build [`AppState`]: connect + migrate the database and pre-compute the
/// EIP-712 domain separator from config.
pub async fn build_state(config: Arc<Config>) -> anyhow::Result<AppState> {
    let pool = db::connect(&config.database.url).await?;
    tracing::info!(url = %config.database.url, "database connected");

    let domain_sep = tap::domain_separator(
        &config.tap.eip712_domain_name,
        config.tap.eip712_chain_id,
        config.tap.eip712_verifying_contract,
    );
    tracing::info!(
        name = %config.tap.eip712_domain_name,
        chain_id = config.tap.eip712_chain_id,
        verifying_contract = %config.tap.eip712_verifying_contract,
        domain_sep = %domain_sep,
        "EIP-712 domain separator computed"
    );

    let http_client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    Ok(AppState { config, pool, http_client, domain_sep })
}

/// Spawn the RAV aggregator and on-chain collector background tasks.
pub fn spawn_background(state: &AppState) {
    aggregator::spawn(Arc::clone(&state.config), state.pool.clone());
    collector::spawn(Arc::clone(&state.config), state.pool.clone());
}

/// Build the standard router: unauthenticated `/health` + `/ready`, and a
/// rate-limited, TAP-gated catch-all proxy to the configured upstream.
pub fn standard_router(state: AppState) -> Router {
    let cfg = Arc::clone(&state.config);

    let period_ms = 1_000u64 / cfg.rate_limit.requests_per_second.max(1) as u64;
    let governor_conf = {
        let mut b = GovernorConfigBuilder::default();
        b.per_millisecond(period_ms).burst_size(cfg.rate_limit.burst_size);
        Arc::new(b.finish().expect("invalid rate limit config"))
    };
    tracing::info!(
        rps = cfg.rate_limit.requests_per_second,
        burst = cfg.rate_limit.burst_size,
        "rate limiter configured"
    );

    let proxy_routes = Router::new()
        .route("/{*path}", any(proxy::handler))
        .route("/", any(proxy::handler))
        .layer(GovernorLayer::new(governor_conf));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .merge(proxy_routes)
        .with_state(state)
}

/// Run a complete standard gateway: build state, spawn background tasks, and
/// serve the standard router. The common one-liner for a new data service.
pub async fn run(config: Config) -> anyhow::Result<()> {
    let config = Arc::new(config);
    let state = build_state(Arc::clone(&config)).await?;
    spawn_background(&state);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let app = standard_router(state);

    tracing::info!(%addr, upstream = %config.backend.upstream_url, "horizon-core gateway listening");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(state): State<AppState>) -> StatusCode {
    match sqlx::query("SELECT 1").execute(&state.pool).await {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

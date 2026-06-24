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
//! [`standard_router`], and [`spawn_background`] directly. For per-endpoint
//! pricing or custom routes (e.g. WebSocket), see [`build_state_with`],
//! [`router_with`], [`run_with`], [`proxy::gate_request`], and the [`pricing`]
//! module.

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
pub mod pricing;
pub mod proxy;
pub mod tap;

pub use config::Config;
pub use db::Pool;
pub use pricing::{PricingPolicy, SharedPricing};

/// Shared state injected into every Axum handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: Pool,
    pub http_client: Client,
    pub domain_sep: B256,
    /// Per-request pricing policy. Defaults to [`pricing::FlatPricing`] (no minimum).
    pub pricing: SharedPricing,
}

/// Build [`AppState`] with the default ([`pricing::FlatPricing`]) policy.
pub async fn build_state(config: Arc<Config>) -> anyhow::Result<AppState> {
    build_state_with(config, pricing::flat()).await
}

/// Build [`AppState`] with a custom [`PricingPolicy`]: connect + migrate the
/// database and pre-compute the EIP-712 domain separator from config.
pub async fn build_state_with(
    config: Arc<Config>,
    pricing: SharedPricing,
) -> anyhow::Result<AppState> {
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

    Ok(AppState { config, pool, http_client, domain_sep, pricing })
}

/// Spawn the RAV aggregator and on-chain collector background tasks.
pub fn spawn_background(state: &AppState) {
    aggregator::spawn(Arc::clone(&state.config), state.pool.clone());
    collector::spawn(Arc::clone(&state.config), state.pool.clone());
}

/// Build the standard router: unauthenticated `/health` + `/ready`, and a
/// rate-limited, TAP-gated catch-all proxy to the configured upstream.
pub fn standard_router(state: AppState) -> Router {
    router_with(state, Router::new())
}

/// Like [`standard_router`], but merges caller-provided `extra` routes (e.g. a
/// WebSocket relay) alongside the proxy. The `extra` routes share [`AppState`]
/// and the same per-IP rate limiter, and — being more specific — take precedence
/// over the catch-all proxy. `/health` and `/ready` remain unauthenticated and
/// unthrottled.
///
/// Custom routes can gate themselves with [`proxy::gate_request`] to reuse the
/// receipt validation + pricing + persistence pipeline.
pub fn router_with(state: AppState, extra: Router<AppState>) -> Router {
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

    // Custom routes first (more specific), then the catch-all proxy; all rate-limited.
    let gated = extra
        .route("/{*path}", any(proxy::handler))
        .route("/", any(proxy::handler))
        .layer(GovernorLayer::new(governor_conf));

    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .merge(gated)
        .with_state(state)
}

/// Run a complete standard gateway: build state, spawn background tasks, and
/// serve the standard router. The common one-liner for a new data service.
pub async fn run(config: Config) -> anyhow::Result<()> {
    run_inner(config, pricing::flat(), Router::new()).await
}

/// Like [`run`], but with a custom [`PricingPolicy`] and `extra` routes — the
/// entry point for services that need per-endpoint pricing and/or custom routes
/// (e.g. a WebSocket data service) while reusing all the payment plumbing.
pub async fn run_with(
    config: Config,
    pricing: SharedPricing,
    extra: Router<AppState>,
) -> anyhow::Result<()> {
    run_inner(config, pricing, extra).await
}

async fn run_inner(
    config: Config,
    pricing: SharedPricing,
    extra: Router<AppState>,
) -> anyhow::Result<()> {
    let config = Arc::new(config);
    let state = build_state_with(Arc::clone(&config), pricing).await?;
    spawn_background(&state);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let app = router_with(state, extra);

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

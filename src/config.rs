//! Configuration shared by all horizon-core data-service gateways.
//!
//! Service-specific gateways may embed [`Config`] directly (the common case) or
//! compose its sub-structs into a larger config. The only service-specific field
//! is [`BackendConfig::upstream_url`] — the data-plane endpoint the gateway
//! TAP-gates and proxies to.

use alloy_primitives::Address;
use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer};

fn de_u128<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Int(i64),
        Str(String),
    }
    match Raw::deserialize(d)? {
        Raw::Int(n) => u128::try_from(n).map_err(|_| D::Error::custom("negative u128")),
        Raw::Str(s) => s.trim().replace('_', "").parse::<u128>().map_err(D::Error::custom),
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub indexer: IndexerConfig,
    pub tap: TapConfig,
    pub backend: BackendConfig,
    pub database: DatabaseConfig,
    pub collector: Option<CollectorConfig>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

#[derive(Debug, Deserialize, Clone)]
pub struct IndexerConfig {
    /// This provider's on-chain address.
    pub service_provider_address: Address,
    /// Hex-encoded 32-byte operator private key used for signing collect() transactions.
    pub operator_private_key: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TapConfig {
    /// The data-service contract address (set after deployment).
    pub data_service_address: Address,
    /// Gateway (consumer) addresses authorised to issue TAP receipts.
    pub authorized_senders: Vec<Address>,
    /// EIP-712 domain name for GraphTallyCollector.
    #[serde(default = "default_tap_domain_name")]
    pub eip712_domain_name: String,
    /// Chain ID where GraphTallyCollector is deployed (42161 = Arbitrum One).
    #[serde(default = "default_tap_chain_id")]
    pub eip712_chain_id: u64,
    /// GraphTallyCollector contract address.
    #[serde(default = "default_tap_verifying_contract")]
    pub eip712_verifying_contract: Address,
    /// Maximum age of a TAP receipt before rejection (nanoseconds).
    #[serde(default = "default_max_receipt_age_ns")]
    pub max_receipt_age_ns: u64,
    /// Base URL of the TAP aggregator's /rav/aggregate endpoint.
    pub aggregator_url: Option<String>,
    /// How often to run RAV aggregation (seconds).
    #[serde(default = "default_aggregation_interval_secs")]
    pub aggregation_interval_secs: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BackendConfig {
    /// The data-plane URL the gateway TAP-gates and proxies to.
    pub upstream_url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CollectorConfig {
    /// Arbitrum One RPC URL for submitting collect() transactions.
    pub arbitrum_rpc_url: String,
    /// How often to check for unredeemed RAVs (seconds).
    #[serde(default = "default_collect_interval_secs")]
    pub collect_interval_secs: u64,
    /// Skip RAVs below this GRT wei threshold (avoids dust gas spend).
    #[serde(default, deserialize_with = "de_u128")]
    pub min_collect_value: u128,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RateLimitConfig {
    /// Number of requests per second allowed per IP address.
    #[serde(default = "default_rps")]
    pub requests_per_second: u32,
    /// Additional burst capacity above the steady-state rate.
    #[serde(default = "default_burst")]
    pub burst_size: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self { requests_per_second: default_rps(), burst_size: default_burst() }
    }
}

impl Config {
    /// Load from the path in `$GATEWAY_CONFIG`, defaulting to `gateway.toml`.
    pub fn load() -> Result<Self> {
        let path = std::env::var("GATEWAY_CONFIG").unwrap_or_else(|_| "gateway.toml".to_string());
        Self::load_from(&path)
    }

    pub fn load_from(path: &str) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config from {path}"))?;
        toml::from_str(&contents).context("failed to parse config")
    }
}

fn default_rps() -> u32 { 20 }
fn default_burst() -> u32 { 40 }

fn default_host() -> String { "0.0.0.0".to_string() }
fn default_port() -> u16 { 8080 }
fn default_tap_domain_name() -> String { "GraphTallyCollector".to_string() }
fn default_tap_chain_id() -> u64 { 42161 } // Arbitrum One
fn default_tap_verifying_contract() -> Address {
    crate::addresses::GRAPH_TALLY_COLLECTOR.parse().unwrap()
}
fn default_max_receipt_age_ns() -> u64 { 30_000_000_000 } // 30 seconds
fn default_aggregation_interval_secs() -> u64 { 60 }
fn default_collect_interval_secs() -> u64 { 3600 }

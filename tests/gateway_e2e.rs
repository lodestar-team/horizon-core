//! End-to-end gateway test: the real TAP-gated HTTP path against a live Postgres.
//!
//! Proves the off-chain half of the paid loop:
//!   missing receipt → 402, valid signed receipt → 200 + proxied body + persisted row,
//!   replayed nonce → 402, expired receipt → 402.
//!
//! Requires a Postgres reachable at $TEST_DATABASE_URL; skipped if unset.
//! Run: TEST_DATABASE_URL=postgres://test:test@localhost:55432/test cargo test --test gateway_e2e -- --nocapture

use std::sync::Arc;

use alloy_primitives::{keccak256, Address, B256, Bytes};
use axum::{routing::any, Router};
use horizon_core::{
    config::{
        BackendConfig, Config, DatabaseConfig, IndexerConfig, RateLimitConfig, ServerConfig,
        TapConfig,
    },
    tap,
};
use k256::ecdsa::SigningKey;

const DOMAIN_NAME: &str = "GraphTallyCollector";
const CHAIN_ID: u64 = 42161;

fn verifying_contract() -> Address {
    Address::from_slice(&[0xAB; 20])
}

fn data_service() -> Address {
    Address::from_slice(&[0x0C; 20])
}

fn provider() -> Address {
    Address::from_slice(&[0x0B; 20])
}

fn eth_address(sk: &SigningKey) -> Address {
    let vk = sk.verifying_key();
    let encoded = vk.to_encoded_point(false);
    let hash = keccak256(&encoded.as_bytes()[1..]);
    Address::from_slice(&hash[12..])
}

fn sign_hex(sk: &SigningKey, hash: B256) -> String {
    let (sig, rec_id) = sk.sign_prehash_recoverable(hash.as_slice()).unwrap();
    let mut bytes = [0u8; 65];
    bytes[..64].copy_from_slice(&sig.to_bytes());
    bytes[64] = rec_id.to_byte();
    format!("0x{}", hex::encode(bytes))
}

/// Build a JSON SignedReceipt header value for the given nonce / timestamp.
fn signed_receipt_header(sk: &SigningKey, nonce: u64, timestamp_ns: u64, value: u128) -> String {
    let domain = tap::domain_separator(DOMAIN_NAME, CHAIN_ID, verifying_contract());
    let receipt = tap::Receipt {
        data_service: data_service(),
        service_provider: provider(),
        timestamp_ns,
        nonce,
        value,
        metadata: Bytes::default(),
    };
    let hash = tap::eip712_hash(domain, &receipt);
    let sig = sign_hex(sk, hash);
    serde_json::to_string(&tap::SignedReceipt { receipt, signature: sig }).unwrap()
}

async fn spawn_stub_upstream() -> String {
    // Echoes a fixed body so the gateway proxy can be verified end-to-end.
    let app = Router::new().route(
        "/{*path}",
        any(|| async { "FILE_CHUNK_OK" }),
    ).route("/", any(|| async { "FILE_CHUNK_OK" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

#[tokio::test]
async fn gateway_tap_path_end_to_end() {
    let Ok(db_url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL not set — skipping gateway e2e test");
        return;
    };

    // Fresh signer; its address is the authorised sender.
    let sk = SigningKey::from_slice(&[7u8; 32]).unwrap();
    let signer_addr = eth_address(&sk);

    let upstream = spawn_stub_upstream().await;

    let config = Config {
        server: ServerConfig { host: "127.0.0.1".into(), port: 0 },
        indexer: IndexerConfig {
            service_provider_address: provider(),
            operator_private_key:
                "0x0000000000000000000000000000000000000000000000000000000000000001".into(),
        },
        tap: TapConfig {
            data_service_address: data_service(),
            authorized_senders: vec![signer_addr],
            eip712_domain_name: DOMAIN_NAME.into(),
            eip712_chain_id: CHAIN_ID,
            eip712_verifying_contract: verifying_contract(),
            max_receipt_age_ns: 30_000_000_000,
            aggregator_url: None,
            aggregation_interval_secs: 60,
        },
        backend: BackendConfig { upstream_url: upstream.clone() },
        database: DatabaseConfig { url: db_url },
        collector: None,
        rate_limit: RateLimitConfig { requests_per_second: 1000, burst_size: 1000 },
    };

    let state = horizon_core::build_state(Arc::new(config)).await.expect("build_state");

    // Clean slate so replay nonces are deterministic across runs.
    sqlx::query("TRUNCATE tap_receipts, tap_ravs")
        .execute(&state.pool)
        .await
        .unwrap();

    let pool = state.pool.clone();
    let app = horizon_core::standard_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gw_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .unwrap();
    });

    let base = format!("http://{gw_addr}");
    let client = reqwest::Client::new();

    // 1. No receipt → 402 Payment Required.
    let r = client.get(format!("{base}/files/abc")).send().await.unwrap();
    assert_eq!(r.status(), 402, "missing receipt must be 402");

    // 2. Valid receipt → 200 + proxied body + persisted row.
    let header = signed_receipt_header(&sk, 1, now_ns(), 1_000);
    let r = client
        .get(format!("{base}/files/abc"))
        .header("tap-receipt", &header)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "valid receipt must be 200");
    assert_eq!(r.text().await.unwrap(), "FILE_CHUNK_OK", "body must be proxied from upstream");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tap_receipts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "receipt must be persisted");

    // 3. Replayed nonce → 402.
    let r = client
        .get(format!("{base}/files/abc"))
        .header("tap-receipt", &header)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 402, "replayed nonce must be rejected");

    // 4. Expired receipt → 402.
    let old = signed_receipt_header(&sk, 2, now_ns() - 60_000_000_000, 1_000);
    let r = client
        .get(format!("{base}/files/abc"))
        .header("tap-receipt", &old)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 402, "expired receipt must be rejected");

    // Still only the one valid receipt persisted.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tap_receipts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "no extra receipts should be stored");
}

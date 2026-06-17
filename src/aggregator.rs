//! Background RAV aggregation task.
//!
//! Runs on a configurable interval. For each distinct payer present in
//! `tap_receipts` it:
//!   1. Fetches all stored receipts for that payer.
//!   2. POSTs them to the TAP aggregator's /rav/aggregate endpoint.
//!   3. Upserts the returned SignedRav into `tap_ravs`.
//!   4. Prunes receipts covered by the new RAV (timestamp_ns <= rav.timestamp_ns).

use std::{sync::Arc, time::Duration};

use alloy_primitives::Bytes;

use crate::{config::Config, db, tap};

pub fn spawn(config: Arc<Config>, pool: db::Pool) {
    let Some(url) = config.tap.aggregator_url.clone() else {
        tracing::info!("tap.aggregator_url not set — RAV aggregation disabled");
        return;
    };

    let interval = Duration::from_secs(config.tap.aggregation_interval_secs);
    tracing::info!(%url, interval_secs = interval.as_secs(), "RAV aggregator started");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to build HTTP client for aggregator");

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = run_once(&url, &config, &pool, &client).await {
                tracing::warn!("RAV aggregation cycle failed: {e:#}");
            }
        }
    });
}

async fn run_once(
    aggregator_url: &str,
    config: &Config,
    pool: &db::Pool,
    client: &reqwest::Client,
) -> anyhow::Result<()> {
    let payers = db::distinct_payers(pool).await?;

    if payers.is_empty() {
        tracing::debug!("no receipts in db, skipping aggregation");
        return Ok(());
    }

    let service_provider = config.indexer.service_provider_address;
    let data_service = config.tap.data_service_address;
    let endpoint = format!("{aggregator_url}/rav/aggregate");

    for payer_hex in payers {
        if let Err(e) =
            aggregate_payer(pool, client, &endpoint, service_provider, data_service, &payer_hex)
                .await
        {
            tracing::warn!(payer = %payer_hex, "RAV aggregation failed for payer: {e:#}");
        }
    }

    Ok(())
}

async fn aggregate_payer(
    pool: &db::Pool,
    client: &reqwest::Client,
    endpoint: &str,
    service_provider: alloy_primitives::Address,
    data_service: alloy_primitives::Address,
    payer_hex: &str,
) -> anyhow::Result<()> {
    let rows = db::fetch_by_payer(pool, payer_hex).await?;
    if rows.is_empty() {
        return Ok(());
    }

    let receipts: Vec<tap::SignedReceipt> = rows
        .iter()
        .map(|row| tap::SignedReceipt {
            receipt: tap::Receipt {
                data_service,
                service_provider,
                timestamp_ns: row.timestamp_ns as u64,
                nonce: row.nonce as u64,
                value: row.value.parse::<u128>().unwrap_or(0),
                metadata: Bytes::from(row.metadata.clone()),
            },
            signature: row.signature.clone(),
        })
        .collect();

    let payer: alloy_primitives::Address = payer_hex
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid payer address in db: {payer_hex}"))?;

    let body = serde_json::json!({
        "service_provider": service_provider,
        "payer": payer,
        "receipts": receipts,
    });

    let resp = client
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("POST {endpoint} failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("aggregator returned {status}: {text}");
    }

    let resp_json: serde_json::Value = resp.json().await?;
    let signed_rav: tap::SignedRav = serde_json::from_value(resp_json["signed_rav"].clone())?;

    let rav = &signed_rav.rav;
    db::upsert_rav(pool, rav, &signed_rav.signature).await?;

    let pruned = db::delete_covered(pool, payer_hex, rav.timestamp_ns as i64).await?;

    tracing::info!(
        payer = %payer_hex,
        receipts = rows.len(),
        pruned,
        value_aggregate = %rav.value_aggregate,
        "RAV updated"
    );

    Ok(())
}

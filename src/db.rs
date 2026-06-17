//! TAP receipt and RAV persistence.

use sqlx::{postgres::PgPoolOptions, PgPool, Row};

use crate::tap::{Rav, ValidatedReceipt};

pub type Pool = PgPool;

pub async fn connect(url: &str) -> anyhow::Result<Pool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(url)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

// ── Receipt helpers ────────────────────────────────────────────────────────────

pub async fn insert_receipt(pool: &Pool, v: &ValidatedReceipt) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO tap_receipts
            (signer_address, payer_address, timestamp_ns, nonce, value, signature, metadata)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(format!("{:?}", v.signer))
    .bind(format!("{:?}", v.payer))
    .bind(v.receipt.timestamp_ns as i64)
    .bind(v.receipt.nonce as i64)
    .bind(v.receipt.value.to_string())
    .bind(&v.signature)
    .bind(v.receipt.metadata.as_ref())
    .execute(pool)
    .await?;
    Ok(())
}

pub struct RawReceipt {
    pub payer_address: String,
    pub timestamp_ns: i64,
    pub nonce: i64,
    pub value: String,
    pub signature: String,
    pub metadata: Vec<u8>,
}

pub async fn fetch_by_payer(pool: &Pool, payer_hex: &str) -> anyhow::Result<Vec<RawReceipt>> {
    let rows = sqlx::query(
        r#"
        SELECT payer_address, timestamp_ns, nonce, value, signature, metadata
        FROM   tap_receipts
        WHERE  payer_address = $1
        ORDER  BY timestamp_ns ASC
        "#,
    )
    .bind(payer_hex)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| RawReceipt {
            payer_address: r.get("payer_address"),
            timestamp_ns: r.get("timestamp_ns"),
            nonce: r.get("nonce"),
            value: r.get("value"),
            signature: r.get("signature"),
            metadata: r.get("metadata"),
        })
        .collect())
}

pub async fn distinct_payers(pool: &Pool) -> anyhow::Result<Vec<String>> {
    let rows = sqlx::query("SELECT DISTINCT payer_address FROM tap_receipts")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|r| r.get("payer_address")).collect())
}

pub async fn delete_covered(pool: &Pool, payer_hex: &str, up_to_ns: i64) -> anyhow::Result<u64> {
    let result =
        sqlx::query("DELETE FROM tap_receipts WHERE payer_address = $1 AND timestamp_ns <= $2")
            .bind(payer_hex)
            .bind(up_to_ns)
            .execute(pool)
            .await?;
    Ok(result.rows_affected())
}

// ── RAV helpers ───────────────────────────────────────────────────────────────

pub async fn upsert_rav(pool: &Pool, rav: &Rav, signature: &str) -> anyhow::Result<()> {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    sqlx::query(
        r#"
        INSERT INTO tap_ravs
            (collection_id, payer_address, service_provider, data_service,
             timestamp_ns, value_aggregate, signature, last_updated)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (collection_id) DO UPDATE SET
            timestamp_ns    = EXCLUDED.timestamp_ns,
            value_aggregate = EXCLUDED.value_aggregate,
            signature       = EXCLUDED.signature,
            last_updated    = EXCLUDED.last_updated,
            redeemed        = false
        "#,
    )
    .bind(format!("{:?}", rav.collection_id))
    .bind(format!("{:?}", rav.payer))
    .bind(format!("{:?}", rav.service_provider))
    .bind(format!("{:?}", rav.data_service))
    .bind(rav.timestamp_ns as i64)
    .bind(rav.value_aggregate.to_string())
    .bind(signature)
    .bind(now_secs)
    .execute(pool)
    .await?;
    Ok(())
}

pub struct RedeemableRav {
    pub collection_id: String,
    pub payer_address: String,
    pub service_provider: String,
    pub data_service: String,
    pub timestamp_ns: i64,
    pub value_aggregate: String,
    pub signature: String,
}

pub async fn fetch_unredeemed_ravs(pool: &Pool) -> anyhow::Result<Vec<RedeemableRav>> {
    let rows = sqlx::query(
        r#"
        SELECT collection_id, payer_address, service_provider, data_service,
               timestamp_ns, value_aggregate, signature
        FROM   tap_ravs
        WHERE  redeemed = false
        ORDER  BY last_updated ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| RedeemableRav {
            collection_id: r.get("collection_id"),
            payer_address: r.get("payer_address"),
            service_provider: r.get("service_provider"),
            data_service: r.get("data_service"),
            timestamp_ns: r.get("timestamp_ns"),
            value_aggregate: r.get("value_aggregate"),
            signature: r.get("signature"),
        })
        .collect())
}

pub async fn mark_rav_redeemed(pool: &Pool, collection_id: &str) -> anyhow::Result<()> {
    sqlx::query("UPDATE tap_ravs SET redeemed = true WHERE collection_id = $1")
        .bind(collection_id)
        .execute(pool)
        .await?;
    Ok(())
}

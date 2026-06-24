//! Generic TAP-gated reverse proxy.
//!
//! Every request must carry a valid `TAP-Receipt` header. The receipt is
//! validated (EIP-712 sig + staleness + authorised sender), persisted to
//! `tap_receipts` (replayed nonces rejected), and then the request — including
//! its `Range` headers, so chunked/byte-range downloads work — is forwarded to
//! the configured upstream data plane.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::Response,
};

use crate::{db, tap, AppState};

/// Maximum request body the gateway will buffer before forwarding (4 MiB).
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Validate a `TAP-Receipt` value for `path`, enforce the gateway's
/// [`PricingPolicy`](crate::pricing::PricingPolicy), and persist the receipt
/// (rejecting replayed nonces). Returns the validated receipt, or an HTTP
/// `(status, message)` to return to the client.
///
/// This is the receipt-gating pipeline the proxy uses, exposed for custom routes
/// (e.g. a WebSocket handler) that need the same gating. `path` is used only for
/// the pricing lookup — pass the request path without its query string.
pub async fn gate_request(
    state: &AppState,
    tap_header: &str,
    path: &str,
) -> Result<tap::ValidatedReceipt, (StatusCode, String)> {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    let validated = tap::validate_receipt(
        tap_header,
        state.domain_sep,
        &state.config.tap.authorized_senders,
        state.config.tap.data_service_address,
        state.config.indexer.service_provider_address,
        state.config.tap.max_receipt_age_ns,
        now_ns,
    )
    .map_err(|e| (StatusCode::PAYMENT_REQUIRED, e.to_string()))?;

    // Enforce per-endpoint pricing: the receipt value must cover this path's price.
    let min_value = state.pricing.min_value(path);
    if validated.receipt.value < min_value {
        return Err((
            StatusCode::PAYMENT_REQUIRED,
            format!(
                "receipt value {} below required {min_value} for {path}",
                validated.receipt.value
            ),
        ));
    }

    match db::insert_receipt(&state.pool, &validated).await {
        Ok(()) => Ok(validated),
        Err(e) if is_duplicate_nonce(&e) => {
            Err((StatusCode::PAYMENT_REQUIRED, "receipt nonce already used".into()))
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

pub async fn handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Result<Response<Body>, (StatusCode, String)> {
    // ── 1. Extract TAP-Receipt header ─────────────────────────────────────────
    let header_str = req
        .headers()
        .get("tap-receipt")
        .ok_or_else(|| (StatusCode::PAYMENT_REQUIRED, "TAP-Receipt header required".to_string()))?
        .to_str()
        .map_err(|_| (StatusCode::BAD_REQUEST, "TAP-Receipt is not valid UTF-8".to_string()))?
        .to_owned();

    // ── 2-3. Validate + price + persist (reject replays) ──────────────────────
    let _validated = gate_request(&state, &header_str, req.uri().path()).await?;

    // ── 4. Proxy to the upstream data plane ───────────────────────────────────
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let backend_url = format!(
        "{}{}",
        state.config.backend.upstream_url.trim_end_matches('/'),
        path_and_query
    );

    let method = reqwest::Method::from_bytes(req.method().as_str().as_bytes())
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Forward safe request headers — including Range, so byte-range/chunked
    // downloads (file hosting, firehose flatfiles) pass through untouched.
    let mut builder = state.http_client.request(method, &backend_url);
    for name in ["content-type", "prefer", "range", "range-unit", "accept", "accept-encoding"] {
        if let Some(v) = req.headers().get(name) {
            if let Ok(s) = v.to_str() {
                builder = builder.header(name, s);
            }
        }
    }

    let body_bytes = axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    if !body_bytes.is_empty() {
        builder = builder.body(body_bytes);
    }

    let resp = builder
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;

    let status = StatusCode::from_u16(resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();

    // Preserve range/length metadata so clients can verify chunk boundaries.
    let content_range = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let accept_ranges = resp
        .headers()
        .get("accept-ranges")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let resp_bytes = resp
        .bytes()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;

    let mut response = Response::builder()
        .status(status)
        .header("content-type", content_type);

    if let Some(cr) = content_range {
        response = response.header("content-range", cr);
    }
    if let Some(ar) = accept_ranges {
        response = response.header("accept-ranges", ar);
    }

    Ok(response.body(Body::from(resp_bytes)).unwrap())
}

/// Returns true if the error is a Postgres unique-constraint violation (SQLSTATE 23505).
fn is_duplicate_nonce(e: &anyhow::Error) -> bool {
    if let Some(sqlx::Error::Database(db_err)) = e.downcast_ref::<sqlx::Error>() {
        return db_err.code().as_deref() == Some("23505");
    }
    false
}

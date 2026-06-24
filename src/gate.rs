//! Pre-forward request gating.
//!
//! A [`RequestGate`] runs after receipt validation + pricing but before the
//! request is proxied. Use it for checks that aren't expressible as a flat price:
//! consumer credit limits, on-chain escrow balance pre-checks, per-payer
//! allowlists, etc. The default ([`AllowAll`]) admits every validated request, so
//! gateways that don't opt in behave exactly as before.
//!
//! Gates are async (an escrow check is typically an `eth_call`) and object-safe
//! via `async_trait`, so they can be stored as `Arc<dyn RequestGate>`.

use std::sync::Arc;

use axum::http::StatusCode;

use crate::tap::ValidatedReceipt;

/// Why a gate rejected a request — surfaced to the client as the HTTP response.
#[derive(Debug, Clone)]
pub struct GateRejection {
    pub status: StatusCode,
    pub message: String,
}

impl GateRejection {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    /// `402 Payment Required` — the common case (out of credit / empty escrow).
    pub fn payment_required(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYMENT_REQUIRED, message)
    }
}

/// Result of a gate check: `Ok(())` to admit, `Err(rejection)` to deny.
pub type GateResult = Result<(), GateRejection>;

/// A pre-forward check on a validated request. See the module docs.
#[async_trait::async_trait]
pub trait RequestGate: Send + Sync {
    /// Decide whether to admit a request. `path` is the request path (no query).
    async fn check(&self, validated: &ValidatedReceipt, path: &str) -> GateResult;
}

/// A shared, type-erased gate as stored on [`crate::AppState`].
pub type SharedGate = Arc<dyn RequestGate>;

/// Admits every validated request — the default gate.
pub struct AllowAll;

#[async_trait::async_trait]
impl RequestGate for AllowAll {
    async fn check(&self, _validated: &ValidatedReceipt, _path: &str) -> GateResult {
        Ok(())
    }
}

/// The default shared gate ([`AllowAll`]).
pub fn allow_all() -> SharedGate {
    Arc::new(AllowAll)
}

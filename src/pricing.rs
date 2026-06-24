//! Per-request pricing policy.
//!
//! Many data services charge different amounts for different endpoints — a cheap
//! status lookup vs. an expensive SQL query (compute-unit / tiered pricing). A
//! [`PricingPolicy`] declares the minimum acceptable TAP receipt `value` (GRT wei)
//! for a given request path; the proxy (and any custom route via
//! [`crate::proxy::gate_request`]) rejects underpaid receipts with `402`.
//!
//! The default policy is [`FlatPricing`] — no per-path minimum — so gateways that
//! don't opt in behave exactly as before.
//!
//! ```
//! use horizon_core::pricing::{FnPricing, PricingPolicy};
//! // 1 GRT wei per compute unit; /sql costs 20 CU, everything else 1 CU.
//! let policy = FnPricing(|path: &str| if path.starts_with("/v1/sql") { 20 } else { 1 });
//! assert_eq!(policy.min_value("/v1/sql"), 20);
//! assert_eq!(policy.min_value("/v1/status"), 1);
//! ```

use std::sync::Arc;

/// Decides the minimum acceptable TAP receipt `value` (GRT wei) for a request path.
///
/// Implementations must be cheap and side-effect free — `min_value` is called on
/// every gated request.
pub trait PricingPolicy: Send + Sync {
    /// Minimum receipt value (GRT wei) required to serve `path`. Return `0` to
    /// accept any value.
    fn min_value(&self, path: &str) -> u128;
}

/// A shared, type-erased pricing policy as stored on [`crate::AppState`].
pub type SharedPricing = Arc<dyn PricingPolicy>;

/// Flat pricing — no per-path minimum. The historical horizon-core behaviour.
#[derive(Debug, Default, Clone, Copy)]
pub struct FlatPricing;

impl PricingPolicy for FlatPricing {
    fn min_value(&self, _path: &str) -> u128 {
        0
    }
}

/// Wrap a closure `Fn(&str) -> u128` as a [`PricingPolicy`].
///
/// The closure returns the minimum receipt value for the given path — typically
/// `compute_units(path) * price_per_cu`.
pub struct FnPricing<F>(pub F);

impl<F> PricingPolicy for FnPricing<F>
where
    F: Fn(&str) -> u128 + Send + Sync,
{
    fn min_value(&self, path: &str) -> u128 {
        (self.0)(path)
    }
}

/// The default shared policy ([`FlatPricing`]).
pub fn flat() -> SharedPricing {
    Arc::new(FlatPricing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_accepts_anything() {
        assert_eq!(FlatPricing.min_value("/v1/sql"), 0);
    }

    #[test]
    fn fn_pricing_tiers() {
        let p = FnPricing(|path: &str| if path.starts_with("/v1/sql") { 20 } else { 1 });
        assert_eq!(p.min_value("/v1/sql"), 20);
        assert_eq!(p.min_value("/v1/status"), 1);
    }
}

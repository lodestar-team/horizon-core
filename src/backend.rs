//! Upstream backend resolution.
//!
//! A [`BackendResolver`] picks the upstream base URL for a request path, so one
//! gateway can front several backends — e.g. a multi-chain RPC proxy routing
//! `/rpc/1` and `/rpc/42161` to different nodes. The default ([`SingleBackend`])
//! resolves every path to the configured `backend.upstream_url`.
//!
//! ```
//! use horizon_core::backend::{BackendResolver, FnBackend};
//! let r = FnBackend(|path: &str| match path.strip_prefix("/rpc/") {
//!     Some("1") => Some("http://eth:8545".into()),
//!     Some("42161") => Some("http://arb:8545".into()),
//!     _ => None,
//! });
//! assert_eq!(r.resolve("/rpc/1").as_deref(), Some("http://eth:8545"));
//! assert_eq!(r.resolve("/rpc/999"), None);
//! ```

use std::sync::Arc;

/// Resolves the upstream base URL for a request path.
pub trait BackendResolver: Send + Sync {
    /// Return the upstream base URL for `path`, or `None` if the path is
    /// unroutable (the proxy then responds `404`).
    fn resolve(&self, path: &str) -> Option<String>;
}

/// A shared, type-erased backend resolver as stored on [`crate::AppState`].
pub type SharedBackend = Arc<dyn BackendResolver>;

/// Resolves every path to a single fixed upstream — the historical behaviour.
#[derive(Debug, Clone)]
pub struct SingleBackend(pub String);

impl BackendResolver for SingleBackend {
    fn resolve(&self, _path: &str) -> Option<String> {
        Some(self.0.clone())
    }
}

/// Wrap a closure `Fn(&str) -> Option<String>` as a [`BackendResolver`].
pub struct FnBackend<F>(pub F);

impl<F> BackendResolver for FnBackend<F>
where
    F: Fn(&str) -> Option<String> + Send + Sync,
{
    fn resolve(&self, path: &str) -> Option<String> {
        (self.0)(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_backend_resolves_all_paths() {
        let b = SingleBackend("http://up:3000".to_string());
        assert_eq!(b.resolve("/anything").as_deref(), Some("http://up:3000"));
    }

    #[test]
    fn fn_backend_routes_by_path() {
        let b = FnBackend(|p: &str| (p == "/ok").then(|| "http://ok".to_string()));
        assert_eq!(b.resolve("/ok").as_deref(), Some("http://ok"));
        assert_eq!(b.resolve("/no"), None);
    }
}

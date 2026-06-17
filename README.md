# horizon-core

Reusable building blocks for [The Graph Horizon](https://thegraph.com) data services.

Almost every Horizon data service — JSON-RPC, Substreams, Solana, WebSocket, file hosting — needs the
same payment plumbing. `horizon-core` is that plumbing, extracted once and shared, so a new data service's
gateway is a thin binary instead of a copy-paste of the last one.

## What's in the box

| Module | Responsibility |
|---|---|
| `tap` | TAP v2 (GraphTally) receipt types, EIP-712 domain separator + hashing, signature recovery, `validate_receipt` |
| `aggregator` | Background task: POST stored receipts to a TAP aggregator, upsert the returned signed RAV, prune covered receipts |
| `collector` | Background task: ABI-encode unredeemed RAVs and submit them to `<DataService>.collect()` on Arbitrum One. The `collect(address,uint8,bytes)` ABI is identical for every Horizon data service, so this is fully generic |
| `db` | `tap_receipts` / `tap_ravs` persistence + CRUD, with bundled migrations |
| `config` | Config structs (`server`, `indexer`, `tap`, `backend`, `database`, `collector`, `rate_limit`) |
| `addresses` | Known Horizon contract addresses (e.g. `GraphTallyCollector` on Arbitrum One) |
| `proxy` | A generic TAP-gated reverse proxy: receipt-in → verify → persist (reject replays) → forward (preserving `Range` headers for chunked/byte-range downloads) |

## A complete gateway

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    horizon_core::run(horizon_core::Config::load()?).await
}
```

`run` builds state (connect + migrate the DB, compute the EIP-712 domain separator), spawns the aggregator
and collector, and serves `/health`, `/ready`, and a rate-limited, TAP-gated catch-all proxy to
`backend.upstream_url`.

Services that need extra routes can compose the pieces directly:

```rust
let state = horizon_core::build_state(config).await?;
horizon_core::spawn_background(&state);
let app = horizon_core::standard_router(state.clone()).route("/custom", get(my_handler));
```

## The TAP v2 flow

1. A consumer signs an EIP-712 `Receipt` under the `GraphTallyCollector` domain (chainId 42161) and sends it
   in the `TAP-Receipt` header on every request.
2. The gateway validates the receipt (data service + provider match, not expired, authorised signer),
   rejects replayed `(signer, nonce)` pairs, persists it, and forwards the request to the upstream data plane.
3. The aggregator periodically rolls stored receipts into a signed RAV via a TAP aggregator.
4. The collector submits unredeemed RAVs to the data-service contract's `collect()`, which redeems them through
   `GraphTallyCollector → PaymentsEscrow → GraphPayments`.

## First consumer

[FHSCE](https://github.com/lodestar-team/FHSCE) — the File Hosting Service, Community Edition — is the first
data service built on `horizon-core`.

## License

Apache-2.0

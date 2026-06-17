//! Known Horizon contract addresses on Arbitrum One (chainId 42161).
//!
//! These are the shared protocol contracts every data service settles against.
//! Service-specific data-service contract addresses are configured per gateway.

/// GraphTallyCollector — redeems TAP v2 receipts/RAVs on-chain. Arbitrum One.
pub const GRAPH_TALLY_COLLECTOR: &str = "0x8f69F5C07477Ac46FBc491B1E6D91E2bb0111A9e";

/// Arbitrum One chain ID — the TAP v2 EIP-712 domain chain.
pub const ARBITRUM_ONE_CHAIN_ID: u64 = 42161;

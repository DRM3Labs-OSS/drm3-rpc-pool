//! Ready-made endpoint lists for common EVM chains.
//!
//! Each preset returns a list of public, no-key RPC endpoints in a sensible
//! priority order. They are a starting point: pair them with your own keyed
//! endpoints (Alchemy/Infura/etc.) at a lower `priority` so paid capacity is
//! preferred and the public URLs act as failover. Nothing here is
//! chain-specific beyond the URL list - `base()` is included but the crate is
//! not Base-specific.
//!
//! Public endpoints come and go; treat these as defaults to be overridden by a
//! config file, not as a guarantee.

use crate::config::{RpcEndpoint, RpcPoolConfig};

/// Build an `RpcPoolConfig` from a preset name, or `None` if unknown.
///
/// Recognized names (case-insensitive): `base`, `ethereum`/`eth`/`mainnet`,
/// `arbitrum`/`arb`, `optimism`/`op`, `polygon`/`matic`, `bnb`/`bsc`.
pub fn config_for(name: &str) -> Option<RpcPoolConfig> {
    let endpoints = endpoints_for(name)?;
    Some(RpcPoolConfig {
        endpoints,
        ..RpcPoolConfig::default()
    })
}

/// Endpoint list for a preset name (see [`config_for`] for accepted aliases).
pub fn endpoints_for(name: &str) -> Option<Vec<RpcEndpoint>> {
    match name.trim().to_ascii_lowercase().as_str() {
        "base" => Some(base()),
        "ethereum" | "eth" | "mainnet" => Some(ethereum()),
        "arbitrum" | "arb" => Some(arbitrum()),
        "optimism" | "op" => Some(optimism()),
        "polygon" | "matic" => Some(polygon()),
        "bnb" | "bsc" => Some(bnb()),
        _ => None,
    }
}

/// All preset names (canonical form) for help text and `init` listings.
pub fn names() -> &'static [&'static str] {
    &["base", "ethereum", "arbitrum", "optimism", "polygon", "bnb"]
}

/// Build a preset endpoint list in ranked failover order (`priority = index`).
/// Public endpoints are heterogeneous - some are reliably faster than others -
/// and live benchmarking shows that riding the best one as a primary and
/// failing over beats blindly spreading load across all of them. Set two
/// endpoints to the same `priority` if you want them treated as peers (the
/// pool then spreads load across that tier by least in-flight), and add your
/// own keyed provider at a lower `priority` to prefer it.
fn list(labeled: &[(&str, &str)]) -> Vec<RpcEndpoint> {
    labeled
        .iter()
        .enumerate()
        .map(|(i, (label, url))| RpcEndpoint {
            label: Some((*label).to_string()),
            priority: i as u32,
            ..RpcEndpoint::new(*url)
        })
        .collect()
}

/// Base mainnet (chain id 8453).
pub fn base() -> Vec<RpcEndpoint> {
    list(&[
        ("base-official", "https://mainnet.base.org"),
        ("base-publicnode", "https://base-rpc.publicnode.com"),
        ("base-1rpc", "https://1rpc.io/base"),
        ("base-llamarpc", "https://base.llamarpc.com"),
        ("base-blockpi", "https://base.blockpi.network/v1/rpc/public"),
    ])
}

/// Ethereum mainnet (chain id 1).
pub fn ethereum() -> Vec<RpcEndpoint> {
    list(&[
        ("eth-llamarpc", "https://eth.llamarpc.com"),
        ("eth-publicnode", "https://ethereum-rpc.publicnode.com"),
        ("eth-cloudflare", "https://cloudflare-eth.com"),
        (
            "eth-blockpi",
            "https://ethereum.blockpi.network/v1/rpc/public",
        ),
        ("eth-1rpc", "https://1rpc.io/eth"),
    ])
}

/// Arbitrum One (chain id 42161).
pub fn arbitrum() -> Vec<RpcEndpoint> {
    list(&[
        ("arb-official", "https://arb1.arbitrum.io/rpc"),
        ("arb-llamarpc", "https://arbitrum.llamarpc.com"),
        ("arb-publicnode", "https://arbitrum-one-rpc.publicnode.com"),
        ("arb-1rpc", "https://1rpc.io/arb"),
    ])
}

/// OP Mainnet (chain id 10).
pub fn optimism() -> Vec<RpcEndpoint> {
    list(&[
        ("op-official", "https://mainnet.optimism.io"),
        ("op-llamarpc", "https://optimism.llamarpc.com"),
        ("op-publicnode", "https://optimism-rpc.publicnode.com"),
        ("op-1rpc", "https://1rpc.io/op"),
    ])
}

/// Polygon PoS (chain id 137).
pub fn polygon() -> Vec<RpcEndpoint> {
    list(&[
        ("polygon-official", "https://polygon-rpc.com"),
        ("polygon-llamarpc", "https://polygon.llamarpc.com"),
        (
            "polygon-publicnode",
            "https://polygon-bor-rpc.publicnode.com",
        ),
        ("polygon-1rpc", "https://1rpc.io/matic"),
    ])
}

/// BNB Smart Chain (chain id 56).
pub fn bnb() -> Vec<RpcEndpoint> {
    list(&[
        ("bnb-official", "https://bsc-dataseed.bnbchain.org"),
        ("bnb-publicnode", "https://bsc-rpc.publicnode.com"),
        ("bnb-llamarpc", "https://binance.llamarpc.com"),
        ("bnb-1rpc", "https://1rpc.io/bnb"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_nonempty_and_valid() {
        for name in names() {
            let cfg = config_for(name).unwrap_or_else(|| panic!("preset {name} missing"));
            assert!(!cfg.endpoints.is_empty(), "{name} has no endpoints");
            cfg.validate()
                .unwrap_or_else(|e| panic!("preset {name} invalid: {e}"));
        }
    }

    #[test]
    fn aliases_resolve() {
        assert!(config_for("eth").is_some());
        assert!(config_for("ARB").is_some());
        assert!(config_for("matic").is_some());
        assert!(config_for("bsc").is_some());
        assert!(config_for("nope").is_none());
    }

    #[test]
    fn priorities_are_ordered_and_labeled() {
        let eps = base();
        for (i, ep) in eps.iter().enumerate() {
            assert_eq!(ep.priority, i as u32);
            assert!(ep.label.is_some());
        }
    }
}

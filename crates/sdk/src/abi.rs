pub const DEX_REVISION: &str = env!("DEX_REVISION");

#[allow(clippy::too_many_arguments)]
pub mod dex {
    alloy::sol!(
        #[derive(Debug)]
        #[sol(rpc)]
        Exchange,
        "../../abi/dex/Exchange.json"
    );
}

#[allow(clippy::too_many_arguments)]
pub mod erc1967_proxy {
    alloy::sol!(
        #[derive(Debug)]
        #[sol(rpc)]
        ERC1967Proxy,
        "../../abi/dex/ERC1967Proxy.json"
    );
}

#[allow(clippy::too_many_arguments)]
pub mod errors {
    alloy::sol!(
        #[derive(Debug)]
        #[sol(rpc)]
        Exchange,
        "../../abi/dex/Errors.abi.json"
    );
}

/// Owner setters that existed before v1.1.7.4, when fees were a per-contract
/// pair rather than a keyed schedule.
///
/// Removed from the exchange ABI by that release, so they are declared here to
/// drive [`crate::testing::TestExchange::new_at_previous_version`]. Calling
/// them against a current deployment reverts on the unknown selector.
#[cfg(feature = "testing")]
pub mod dex_legacy {
    alloy::sol! {
        #[sol(rpc)]
        interface LegacyExchange {
            function setTakerFee(uint256 perpId, uint256 takerFeePer100K) external;
            function setMakerFee(uint256 perpId, uint256 makerFeePer100K) external;
            // Pre-v1.1.7.4 `addContract`: carries two genesis-fee args the
            // v1.1.7.4 signature dropped, so it is a distinct selector reachable
            // only against the previous generation.
            function addContract(
                string name,
                string symbol,
                uint256 perpId,
                uint256 basePricePNS,
                uint256 priceDecimals,
                uint256 lotDecimals,
                uint256 takerFeePer100K,
                uint256 makerFeePer100K,
                uint256 initMarginFracHdths,
                uint256 maintMarginFracHdths
            ) external;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub mod testing {
    alloy::sol!(
        /// Test ERC-20 token to use as an exchange collateral token.
        #[derive(Debug)]
        #[sol(rpc)]
        TestToken,
        "../../abi/testing/TestToken.json"
    );
}

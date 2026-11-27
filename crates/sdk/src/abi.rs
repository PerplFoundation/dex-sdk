pub const DEX_REVISION: &str = env!("DEX_REVISION");

#[allow(clippy::too_many_arguments)]
pub mod dex {
    alloy::sol!(
        #[derive(Debug)]
        #[sol(rpc)]
        Exchange,
        "abi/dex/Exchange.json"
    );
}

#[allow(clippy::too_many_arguments)]
pub mod erc1967_proxy {
    alloy::sol!(
        #[derive(Debug)]
        #[sol(rpc)]
        ERC1967Proxy,
        "abi/dex/ERC1967Proxy.json"
    );
}

#[allow(clippy::too_many_arguments)]
pub mod errors {
    alloy::sol!(
        #[derive(Debug)]
        #[sol(rpc)]
        Exchange,
        "abi/dex/Errors.abi.json"
    );
}

/// Selectors of earlier contract generations that the current ABI no longer
/// carries, declared here to drive the generations
/// [`crate::testing::TestExchange::new_at`] deploys and the upgrade hops
/// between them. Calling any of them against a current deployment reverts on
/// the unknown selector.
///
/// * The owner fee setters existed before v1.1.7.4, when fees were a
///   per-contract pair rather than a keyed schedule.
/// * `initializeV3` / `initializeV4` are the v1.1.7.4 and v1.1.7.5
///   reinitializers. Spent on every deployment, they were removed from the
///   build by v1.1.7.6 to recover bytecode, so the v1.1.7.5 implementation is
///   the last one that can run them.
#[cfg(feature = "testing")]
#[allow(clippy::too_many_arguments)]
pub mod dex_legacy {
    alloy::sol! {
        #[sol(rpc)]
        interface LegacyExchange {
            function setTakerFee(uint256 perpId, uint256 takerFeePer100K) external;
            function setMakerFee(uint256 perpId, uint256 makerFeePer100K) external;
            // v1.1.7.4: seeds the keyed fee schedules and repoints every live
            // perpetual at the default one.
            function initializeV3(
                uint256[8] defaultTakerFeesPer100K,
                uint256[8] defaultMakerFeesPer100K,
                uint256[8] rwaTakerFeesPer100K,
                uint256[8] rwaMakerFeesPer100K,
                uint256[] existingPerpIds
            ) external;
            // v1.1.7.5: redenominates every stored schedule rate from
            // hundred-thousandths to millionths, attesting its pre-image.
            function initializeV4(
                uint256[8] expectedOldTakerFees,
                uint256[8] expectedOldMakerFees,
                uint256 expectedNonZeroSchedCount
            ) external;
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
        "abi/testing/TestToken.json"
    );
}

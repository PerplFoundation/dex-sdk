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

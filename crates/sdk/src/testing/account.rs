use alloy::primitives::{Address, U256};
use fastnum::UD128;

use super::TestExchange;
use crate::types;

#[derive(Debug)]
pub struct TestAccount<'e> {
    pub id: types::AccountId,
    pub address: Address,
    /// Hex-encoded key the account's address is derived from, so a test can
    /// drive a client that signs for itself rather than one that submits
    /// through the harness's own wallet.
    pub pk: String,
    pub exchange: &'e TestExchange,
}

impl<'e> TestAccount<'e> {
    pub async fn balance(&self) -> UD128 {
        let acc = self
            .exchange
            .exchange
            .getAccountById(U256::from(self.id))
            .call()
            .await
            .unwrap();
        self.exchange
            .collateral_converter
            .from_unsigned(acc.balanceCNS)
    }

    pub async fn locked_balance(&self) -> UD128 {
        let acc = self
            .exchange
            .exchange
            .getAccountById(U256::from(self.id))
            .call()
            .await
            .unwrap();
        self.exchange
            .collateral_converter
            .from_unsigned(acc.lockedBalanceCNS)
    }
}

use alloy::{
    network::Ethereum,
    primitives::{I256, U256},
    providers::PendingTransactionBuilder,
};
use fastnum::UD64;

use super::TestExchange;
use crate::{error::DexError, num, types};

#[derive(Debug)]
pub struct TestPerp<'e> {
    pub id: types::PerpetualId,
    pub name: String,
    pub price_converter: num::Converter,
    pub size_converter: num::Converter,
    pub leverage_converter: num::Converter,
    pub exchange: &'e TestExchange,
}

impl<'e> TestPerp<'e> {
    pub async fn with_mark_price(self, price: UD64) -> Self {
        self.exchange
            .exchange
            .updateMarkPricePNS(U256::from(self.id), self.price_converter.to_unsigned(price))
            .from(self.exchange.price_admin)
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    pub async fn with_min_post(self, min: U256) -> Self {
        self.exchange
            .exchange
            .setMinPost(min)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    pub async fn with_min_settle(self, min: U256) -> Self {
        self.exchange
            .exchange
            .setMinSettle(min)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    pub async fn unpause(self) -> Self {
        self.exchange
            .exchange
            .setContractPaused(U256::from(self.id), false)
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    pub async fn set_mark_price(&self, price: UD64) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .updateMarkPricePNS(U256::from(self.id), self.price_converter.to_unsigned(price))
            .from(self.exchange.price_admin)
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
    }

    pub async fn set_funding_rate(
        &self,
        price: u32,
        rate: i32,
    ) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .setFundingSum(U256::from(self.id), I256::try_from(rate).unwrap(), price, true, true)
            .from(self.exchange.anvil.addresses()[2]) // From Price Admin
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
    }

    pub async fn order(
        &self,
        account_id: types::AccountId,
        request: types::OrderRequest,
    ) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .execOrder(request.to_order_desc(
                self.price_converter,
                self.size_converter,
                self.leverage_converter,
                Some(self.exchange.collateral_converter),
            ))
            .from(
                *self
                    .exchange
                    .account_address
                    .get(&account_id)
                    .unwrap()
                    .value(),
            )
            .gas(1000000)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
    }

    pub async fn orders(
        &self,
        account_id: types::AccountId,
        requests: Vec<types::OrderRequest>,
    ) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .execOpsAndOrders(
                vec![],
                requests
                    .iter()
                    .map(|req| {
                        req.to_order_desc(
                            self.price_converter,
                            self.size_converter,
                            self.leverage_converter,
                            Some(self.exchange.collateral_converter),
                        )
                    })
                    .collect(),
                true,
            )
            .from(
                *self
                    .exchange
                    .account_address
                    .get(&account_id)
                    .unwrap()
                    .value(),
            )
            .gas(1000000 * requests.len() as u64)
            .send()
            .await
            .map_err::<DexError, _>(DexError::from)
            .unwrap()
    }
}

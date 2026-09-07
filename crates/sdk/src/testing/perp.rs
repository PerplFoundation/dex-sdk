use alloy::{
    network::Ethereum,
    primitives::{Address, I256, U256},
    providers::PendingTransactionBuilder,
};
use fastnum::UD64;

use super::TestExchange;
use crate::{abi::dex_legacy::LegacyExchange, error::DexError, num, state, types};

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
            .updateMarkPricePNS(U256::from(self.id), self.price_converter.to_unsigned(price).to())
            .from(self.exchange.price_admin)
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
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
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
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
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
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
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    pub async fn set_maintenance_margin(
        &self,
        maintenance_margin: UD64,
    ) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .setMaintenanceMarginFraction(
                U256::from(self.id),
                self.leverage_converter.to_unsigned(maintenance_margin),
            )
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
    }

    pub async fn set_mark_price(&self, price: UD64) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .updateMarkPricePNS(U256::from(self.id), self.price_converter.to_unsigned(price).to())
            .from(self.exchange.price_admin)
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
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
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
    }

    /// Sets this contract's fees through the pre-v1.1.7.4 per-contract setters,
    /// for use with [`TestExchange::new_at_previous_version`].
    ///
    /// The current contract has no equivalent - fees live in the keyed
    /// schedules and these setters were removed - so this reverts against
    /// it. Emits the deprecated `TakerFeeUpdated`/`MakerFeeUpdated` events.
    pub async fn with_legacy_fees(self, taker_fee: UD64, maker_fee: UD64) -> Self {
        let legacy =
            LegacyExchange::new(*self.exchange.exchange.address(), self.exchange.provider.clone());
        // Per100K: this reaches the PREVIOUS generation, which reads fee rates
        // against 100,000. The current setters use ppm.
        let fee_converter = num::fee_converter();
        legacy
            .setTakerFee(U256::from(self.id), fee_converter.to_unsigned(taker_fee))
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        legacy
            .setMakerFee(U256::from(self.id), fee_converter.to_unsigned(maker_fee))
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        self
    }

    /// Sets a custom fee schedule for this perpetual contract, eight
    /// `(taker, maker)` rates indexed by an account's fee tier.
    ///
    /// v1.1.7.4 split the one-shot custom-schedule setter into value-set +
    /// repoint, done here back to back. Use
    /// [`Self::set_own_fee_schedule_values`] and [`Self::use_own_fee_schedule`]
    /// to exercise the two halves apart.
    pub async fn set_fee_schedule(
        &self,
        taker_fees: [UD64; state::FEE_TIERS],
        maker_fees: [UD64; state::FEE_TIERS],
    ) -> PendingTransactionBuilder<Ethereum> {
        self.set_own_fee_schedule_values(taker_fees, maker_fees)
            .await;
        self.use_own_fee_schedule().await
    }

    /// Writes the rates of the schedule keyed by this contract's id, without
    /// pointing the contract at it - schedule values and the pointer into them
    /// are independent.
    pub async fn set_own_fee_schedule_values(
        &self,
        taker_fees: [UD64; state::FEE_TIERS],
        maker_fees: [UD64; state::FEE_TIERS],
    ) {
        // ppm, like every other schedule setter here -- see
        // `TestExchange::set_fee_schedule`.
        let fee_converter = num::ppm_fee_converter();
        self.exchange
            .exchange
            .setFeeSchedValues(
                U256::from(self.id),
                taker_fees.map(|fee| fee_converter.to_unsigned(fee)),
                maker_fees.map(|fee| fee_converter.to_unsigned(fee)),
            )
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
    }

    /// Points this perpetual contract at the schedule keyed by its own id, see
    /// [`Self::set_own_fee_schedule_values`].
    pub async fn use_own_fee_schedule(&self) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .setPerpToFeeSchedule(U256::from(self.id), U256::from(self.id))
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
    }

    /// Repoints this perpetual contract at the exchange-wide RWA default fee
    /// schedule.
    pub async fn set_rwa_default_fee(&self) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .setPerpToDefaultRwaFeeSched(U256::from(self.id))
            .gas(500000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
    }

    /// Posts an order via the V1 entrypoint, which carries no builder
    /// attribution.
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
            .from(self.account_address(account_id))
            .gas(5000000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
    }

    /// Posts an order via the V2 entrypoint, carrying the request's builder
    /// attribution if any.
    pub async fn order_v2(
        &self,
        account_id: types::AccountId,
        request: types::OrderRequest,
    ) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .execOrderV2(
                request.to_order_desc(
                    self.price_converter,
                    self.size_converter,
                    self.leverage_converter,
                    Some(self.exchange.collateral_converter),
                ),
                request.to_order_extension().unwrap(),
            )
            .from(self.account_address(account_id))
            .gas(5000000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
    }

    /// Posts a batch of orders via the V2 entrypoint, carrying each request's
    /// builder attribution if any.
    pub async fn orders_v2(
        &self,
        account_id: types::AccountId,
        requests: Vec<types::OrderRequest>,
        revert_on_fail: bool,
    ) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .execOrdersV2(
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
                revert_on_fail,
                requests
                    .iter()
                    .map(|req| req.to_order_extension().unwrap())
                    .collect(),
            )
            .from(self.account_address(account_id))
            .gas(150000000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
    }

    fn account_address(&self, account_id: types::AccountId) -> Address {
        *self
            .exchange
            .account_address
            .get(&account_id)
            .unwrap()
            .value()
    }

    pub async fn orders(
        &self,
        account_id: types::AccountId,
        requests: Vec<types::OrderRequest>,
    ) -> PendingTransactionBuilder<Ethereum> {
        self.exchange
            .exchange
            .execOrders(
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
            .from(self.account_address(account_id))
            .gas(150000000)
            .send()
            .await
            .map_err::<DexError, _>(|err| DexError::Provider(err.into()))
            .unwrap()
    }
}

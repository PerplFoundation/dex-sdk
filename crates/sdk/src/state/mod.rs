//! Exchange state tracking.
//!
//! Initial state snapshot has to be taken from the recent on-chain state by the
//! [`SnapshotBuilder`], then the snapshot can be kept up to date by the event
//! data from [`crate::stream::raw`] in a consistent manner.
//!
//! [`Exchange`] is at the root of indexed state and provides access to all
//! nested state entities, as well as basic market data derived from observed
//! trading activity.
//!
//! Some of the state and market data can be retrieved/computed only from the
//! event stream and is not available from the plain snapshot, the documentation
//! for corresponding access methods explicitly covers such cases.

mod account;
mod event;
mod exchange;
mod l3_book;
mod order;
mod perpetual;
mod position;

use std::collections::{HashMap, hash_map};

pub use account::*;
use alloy::{
    eips::BlockId,
    primitives::{Address, U256},
    providers::Provider,
};
pub use event::*;
pub use exchange::*;
use itertools::Itertools;
pub use l3_book::*;
pub use order::*;
pub use perpetual::*;
pub use position::*;

use crate::{
    Chain,
    abi::dex::{
        self,
        Exchange::{
            PerpetualInfo, PerpetualInfoV2, PositionInfo, PositionInfoV2, getExchangeInfoReturn,
        },
    },
    error::{DexError, ProviderError},
    num, types,
};

/// Default number of orders to fetch via single call.
/// Assuming Monad's 8100 gas per storage slot access and 30M gas limit of
/// `eth_call`, plus some buffer.
const DEFAULT_ORDERS_PER_BATCH: usize = 3000;

/// Default number of positions to fetch via single call.
/// Assuming Monad's 8100 gas per storage slot access and 30M gas limit of
/// `eth_call`, plus some buffer.
const DEFAULT_POSITIONS_PER_BATCH: usize = 3000;

/// Builds a consistent snapshot of the exchange state
/// that can be then kept up-to-date by the data from [`crate::stream::raw`].
pub struct SnapshotBuilder<P> {
    chain: Chain,
    instance: dex::Exchange::ExchangeInstance<P>,
    provider: P,
    block_id: BlockId,
    perpetuals: Vec<types::PerpetualId>,
    accounts: Vec<types::AccountAddressOrID>,
    all_positions: bool,
    orders_per_batch: usize,
    positions_per_batch: usize,
}

impl<P: Provider + Clone> SnapshotBuilder<P> {
    /// Creates a new [`SnapshotBuilder`] which fetches the full exchange state
    /// at the latest block.
    pub fn new(chain: &Chain, provider: P) -> Self {
        Self {
            chain: chain.clone(),
            instance: dex::Exchange::new(chain.exchange(), provider.clone()),
            provider,
            block_id: BlockId::Number(alloy::eips::BlockNumberOrTag::Latest),
            perpetuals: chain.perpetuals.clone(),
            accounts: vec![],
            all_positions: false,
            orders_per_batch: DEFAULT_ORDERS_PER_BATCH,
            positions_per_batch: DEFAULT_POSITIONS_PER_BATCH,
        }
    }

    /// Sets the block number or tag to fetch the state at (default: latest).
    /// If tag is provided, it gets converted to a specific block number first
    /// to ensure state consistency.
    pub fn at_block(mut self, block: BlockId) -> Self {
        self.block_id = block;
        self
    }

    /// Sets the list of perpetual contract IDs to fetch the state for.
    pub fn with_perpetuals(mut self, perpetuals: Vec<types::PerpetualId>) -> Self {
        self.perpetuals = perpetuals;
        self
    }

    /// Sets the list of addresses to fetch the state of exchange accounts for.
    /// Assumes accounts already exist, snapshot creation will fail otherwise.
    pub fn with_accounts(mut self, accounts: Vec<types::AccountAddressOrID>) -> Self {
        self.accounts = accounts;
        self.all_positions = false;
        self
    }

    /// Forces to fetch all available positions, along with corresponding
    /// accounts, but without account state snapshot.
    /// Mutually exclusive with [`Self::with_accounts`].
    pub fn with_all_positions(mut self) -> Self {
        self.accounts = vec![];
        self.all_positions = true;
        self
    }

    /// Sets the number of orders to fetch in a single batch via multicall
    /// (default: 3000). Use if default does not fit node/provider gas and
    /// response size limits.
    pub fn with_orders_per_batch(mut self, orders_per_batch: usize) -> Self {
        self.orders_per_batch = orders_per_batch;
        self
    }

    /// Sets the number of positions to fetch in a single batch (default: 3000).
    /// Use if default does not fit node/provider gas and response size limits.
    pub fn with_positions_per_batch(mut self, positions_per_batch: usize) -> Self {
        self.positions_per_batch = positions_per_batch;
        self
    }

    /// Build the snapshot
    pub async fn build(mut self) -> Result<Exchange, DexError> {
        // Normalize block ID to fetch consistent state
        let instant = self.normalize_block().await?;

        // Probe once to learn whether the deployed contract exposes the V2
        // getters added in v1.1.7.3b. Older deployments revert on the selector.
        let supports_v2 = self.supports_v2().await;

        // Global exchange parameters and state
        let (
            exchange_info,
            funding_interval,
            min_post,
            min_settle,
            recycle_fee,
            is_halted,
            num_of_accounts,
        ) = self.exchange_info().await?;
        let collateral_converter = num::Converter::new(exchange_info.collateralDecimals.to());

        // Perpetual contracts parameters, state and active orders
        let perpetuals = self.perpetuals(instant, supports_v2).await?;

        let accounts = if !self.accounts.is_empty() {
            // Accounts parameters, state and open positions if specific accounts requested
            self.accounts(instant, &perpetuals, collateral_converter, supports_v2)
                .await?
        } else if self.all_positions {
            // All positions with corresponding accounts without parameters and balance
            // snapshot
            self.position_accounts(
                instant,
                &perpetuals,
                num_of_accounts.to(),
                collateral_converter,
                supports_v2,
            )
            .await?
        } else {
            HashMap::new()
        };

        Ok(Exchange::new(
            self.chain.clone(),
            instant,
            collateral_converter,
            funding_interval.to(),
            collateral_converter.from_unsigned(min_post),
            collateral_converter.from_unsigned(min_settle),
            collateral_converter.from_unsigned(recycle_fee),
            perpetuals,
            accounts,
            is_halted,
            self.all_positions,
        ))
    }

    /// Returns true if the deployed exchange exposes the V2 getter functions
    /// (added in v1.1.7.3b). Pre-V2 contracts revert on the unknown selector.
    ///
    /// Probes via `getPerpetualInfoV2` against a configured perpetual id -
    /// unlike `getPositionV2`, the perpetual getter does not validate account
    /// existence, so the probe distinguishes selector presence from state.
    ///
    /// TODO: generalize versioning logic once smart contract supports EIP-165
    async fn supports_v2(&self) -> bool {
        let Some(perp_id) = self.perpetuals.first() else {
            // No configured perpetuals means no V2 getters will be called -
            // detection result is irrelevant. Default to V2 (current SDK).
            return true;
        };
        self.instance
            .getPerpetualInfoV2(U256::from(*perp_id))
            .block(self.block_id)
            .call()
            .await
            .is_ok()
    }

    /// Fetches `PerpetualInfoV2`, falling back to the V0 ABI when the contract
    /// has not been upgraded yet (the V0 layout omits `fundingSumScalingExp`,
    /// which is defaulted to zero on the V0 path).
    async fn fetch_perpetual_info(
        &self,
        perp_id: U256,
        supports_v2: bool,
    ) -> Result<PerpetualInfoV2, alloy::contract::Error> {
        if supports_v2 {
            self.instance
                .getPerpetualInfoV2(perp_id)
                .block(self.block_id)
                .call()
                .await
        } else {
            self.instance
                .getPerpetualInfo(perp_id)
                .block(self.block_id)
                .call()
                .await
                .map(perpetual_info_v0_to_v2)
        }
    }

    /// Fetches `PositionInfoV2`, falling back to the V0 ABI when the contract
    /// has not been upgraded yet (the V0 layout omits `priceResiduePNSQ16`,
    /// which is defaulted to zero on the V0 path).
    async fn fetch_position_info(
        &self,
        perp_id: U256,
        account_id: U256,
        supports_v2: bool,
    ) -> Result<PositionInfoV2, alloy::contract::Error> {
        if supports_v2 {
            self.instance
                .getPositionV2(perp_id, account_id)
                .block(self.block_id)
                .call()
                .await
                .map(|r| r.positionInfo)
        } else {
            self.instance
                .getPosition(perp_id, account_id)
                .block(self.block_id)
                .call()
                .await
                .map(|r| position_info_v0_to_v2(r.positionInfo))
        }
    }

    async fn normalize_block(&mut self) -> Result<types::StateInstant, DexError> {
        // Transform provided block ID to fixed number block ID and use if for all calls
        // to retrieve consistent state
        let block_header = self
            .provider
            .get_block(self.block_id)
            .await
            .map_err(|err| DexError::Provider(err.into()))?
            .map(|b| b.into_header())
            .ok_or(DexError::Provider(ProviderError::InvalidRequest(
                "block not found".to_string(),
            )))?;
        self.block_id = BlockId::number(block_header.number);
        Ok(types::StateInstant::new(block_header.number, block_header.timestamp))
    }

    async fn exchange_info(
        &self,
    ) -> Result<(getExchangeInfoReturn, U256, U256, U256, U256, bool, U256), DexError> {
        let (
            exchange_info_call,
            funding_interval_call,
            min_post_call,
            min_settle_call,
            recycle_fee_call,
            is_halted_call,
            num_of_accounts_call,
        ) = (
            self.instance.getExchangeInfo().block(self.block_id),
            self.instance.getFundingInterval().block(self.block_id),
            self.instance.getMinimumPostCNS().block(self.block_id),
            self.instance.getMinimumSettleCNS().block(self.block_id),
            self.instance.getRecycleFeeCNS().block(self.block_id),
            self.instance.isHalted().block(self.block_id),
            self.instance.numberOfAccounts(),
        );
        futures::try_join!(
            exchange_info_call.call().into_future(),
            funding_interval_call.call().into_future(),
            min_post_call.call().into_future(),
            min_settle_call.call().into_future(),
            recycle_fee_call.call().into_future(),
            is_halted_call.call().into_future(),
            num_of_accounts_call.call().into_future(),
        )
        .map_err(|err| DexError::Provider(err.into()))
    }

    async fn perpetuals(
        &self,
        instant: types::StateInstant,
        supports_v2: bool,
    ) -> Result<HashMap<types::PerpetualId, perpetual::Perpetual>, DexError> {
        let perpetual_futs = self.perpetuals.iter().map(|perp_id| async move {
            let pid = U256::from(*perp_id);
            let (maker_fee_call, taker_fee_call, margins_call) = (
                self.instance.getMakerFee(pid).block(self.block_id),
                self.instance.getTakerFee(pid).block(self.block_id),
                self.instance
                    .getMarginFractions(pid, U256::ZERO)
                    .block(self.block_id),
            );

            futures::try_join!(
                self.fetch_perpetual_info(pid, supports_v2),
                maker_fee_call.call().into_future(),
                taker_fee_call.call().into_future(),
                margins_call.call().into_future(),
            )
            .map(|(perp_info, maker_fee, taker_fee, margins)| {
                (*perp_id, perp_info, maker_fee, taker_fee, margins)
            })
        });

        let mut perpetuals = futures::future::try_join_all(perpetual_futs)
            .await
            .map_err(|err| DexError::Provider(err.into()))?
            .into_iter()
            .map(|(perp_id, perp_info, maker_fee, taker_fee, margins)| {
                let perp = Perpetual::new(
                    instant,
                    perp_id,
                    &perp_info,
                    maker_fee,
                    taker_fee,
                    margins.perpInitMarginFracHdths,
                    margins.perpMaintMarginFracHdths,
                );
                (perp_id, perp)
            })
            .collect::<HashMap<_, _>>();

        // Fetching orders one perp at a time to bound parallel requests
        for perp in perpetuals.values_mut() {
            self.perpetual_orders(perp).await?;
        }

        Ok(perpetuals)
    }

    async fn perpetual_orders(&self, perp: &mut perpetual::Perpetual) -> Result<(), DexError> {
        let pid = U256::from(perp.id());
        let order_id_index = self
            .instance
            .getOrderIdIndex(pid)
            .block(self.block_id)
            .call()
            .await
            .map_err(|err| DexError::Provider(err.into()))?;

        let order_ids = order_id_index
            .leaves
            .into_iter()
            .enumerate()
            .flat_map(|(leaf, bitmap)| {
                // Skip the first bit of the first leaf slot (_NULL_ORDER_ID)
                // All remaining IDs are guaranteed non-zero since we start at bit 1
                ((if leaf == 0 { 1 } else { 0 })..U256::BITS)
                    .filter(move |bit| bitmap.bit(*bit))
                    .map(move |bit| {
                        let id = (leaf * U256::BITS + bit) as u16;
                        // Safety: we skip bit 0 of leaf 0, so id is always >= 1
                        std::num::NonZeroU16::new(id).expect("order id from bitmap cannot be 0")
                    })
            })
            .collect::<Vec<_>>();

        let order_batch_futs = order_ids.chunks(self.orders_per_batch).map(|chunk| {
            let multicall = self
                .provider
                .multicall()
                .block(self.block_id)
                .dynamic()
                .extend(
                    chunk
                        .iter()
                        .map(|oid| self.instance.getOrder(pid, U256::from(oid.get()))),
                );
            async move { multicall.aggregate().await }
        });

        let (instant, base_price, price_converter, size_converter, leverage_converter) = (
            perp.instant(),
            perp.base_price(),
            perp.price_converter(),
            perp.size_converter(),
            perp.leverage_converter(),
        );

        // Collect all orders first, then add via snapshot method to preserve FIFO
        // ordering
        let orders: Vec<Order> = futures::future::try_join_all(order_batch_futs)
            .await
            .map_err(|err| DexError::Provider(err.into()))?
            .into_iter()
            .flatten()
            .map(|ord| {
                Order::from_snapshot(
                    instant,
                    ord,
                    base_price,
                    price_converter,
                    size_converter,
                    leverage_converter,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| DexError::OrderParse(perp.id(), err))?;

        perp.add_orders_from_snapshot(orders)
    }

    async fn accounts(
        &self,
        instant: types::StateInstant,
        perpetuals: &HashMap<types::PerpetualId, perpetual::Perpetual>,
        collateral_converter: num::Converter,
        supports_v2: bool,
    ) -> Result<HashMap<types::AccountId, Account>, DexError> {
        let account_futs = self.accounts.iter().map(|acc| async move {
            let acc_info = match acc {
                types::AccountAddressOrID::Address(addr) => self
                    .instance
                    .getAccountByAddr(*addr)
                    .block(self.block_id)
                    .call()
                    .await
                    .map_err(|err| DexError::Provider(err.into()))?,
                types::AccountAddressOrID::ID(id) => self
                    .instance
                    .getAccountById(U256::from(*id))
                    .block(self.block_id)
                    .call()
                    .await
                    .map_err(|err| DexError::Provider(err.into()))?,
            };
            let perps_with_positions = perpetuals_with_position(&acc_info.positions);
            let position_futs = perps_with_positions.iter().map(|perp_id| async {
                self.fetch_position_info(U256::from(*perp_id), acc_info.accountId, supports_v2)
                    .await
                    .map(|pos_info| (*perp_id, pos_info))
                    .map_err(|err| DexError::Provider(err.into()))
            });
            let positions = futures::future::try_join_all(position_futs).await?;
            Ok::<_, DexError>((acc_info.accountId, acc_info, positions))
        });

        Ok(futures::future::try_join_all(account_futs)
            .await?
            .into_iter()
            .map(|(acc_id, acc_info, positions)| {
                (
                    acc_id.to(),
                    Account::new(
                        instant,
                        acc_id.to(),
                        &acc_info,
                        positions
                            .into_iter()
                            .filter_map(|(perp_id, pos_info)| {
                                perpetuals.get(&perp_id).map(|perp| {
                                    (
                                        perp_id,
                                        Position::new(
                                            instant,
                                            perp_id,
                                            &pos_info,
                                            collateral_converter,
                                            perp.price_converter(),
                                            perp.size_converter(),
                                            perp.maintenance_margin(),
                                        ),
                                    )
                                })
                            })
                            .collect(),
                        collateral_converter,
                    ),
                )
            })
            .collect())
    }

    async fn position_accounts(
        &self,
        instant: types::StateInstant,
        perpetuals: &HashMap<types::PerpetualId, perpetual::Perpetual>,
        num_accounts: usize,
        collateral_converter: num::Converter,
        supports_v2: bool,
    ) -> Result<HashMap<types::AccountId, Account>, DexError> {
        let mut accounts: HashMap<types::AccountId, Account> = HashMap::new();
        for (perp_id, perp) in perpetuals {
            let pid = U256::from(*perp_id);
            let infos = self
                .fetch_position_infos_for_perp(pid, num_accounts, supports_v2)
                .await?;
            for info in infos {
                if info.lotLNS.is_zero() {
                    continue;
                }
                let position = Position::new(
                    instant,
                    *perp_id,
                    &info,
                    collateral_converter,
                    perp.price_converter(),
                    perp.size_converter(),
                    perp.maintenance_margin(),
                );
                match accounts.entry(info.accountId.to()) {
                    hash_map::Entry::Occupied(mut e) => {
                        e.get_mut().positions_mut().insert(*perp_id, position);
                    },
                    hash_map::Entry::Vacant(e) => {
                        e.insert(Account::from_position(instant, position));
                    },
                }
            }
        }

        Ok(accounts)
    }

    /// Batches `getPosition`/`getPositionV2` calls for every account id of a
    /// single perpetual. Normalizes both ABI versions to `PositionInfoV2`.
    async fn fetch_position_infos_for_perp(
        &self,
        perp_id: U256,
        num_accounts: usize,
        supports_v2: bool,
    ) -> Result<Vec<PositionInfoV2>, DexError> {
        let account_id_chunks = (1..num_accounts + 1).chunks(self.positions_per_batch);
        if supports_v2 {
            let batch_futs = account_id_chunks.into_iter().map(|chunk| {
                let multicall = self
                    .provider
                    .multicall()
                    .block(self.block_id)
                    .dynamic()
                    .extend(chunk.map(|aid| self.instance.getPositionV2(perp_id, U256::from(aid))));
                async move { multicall.aggregate().await }
            });
            Ok(futures::future::try_join_all(batch_futs)
                .await
                .map_err(|err| DexError::Provider(err.into()))?
                .into_iter()
                .flatten()
                .map(|r| r.positionInfo)
                .collect())
        } else {
            let batch_futs = account_id_chunks.into_iter().map(|chunk| {
                let multicall = self
                    .provider
                    .multicall()
                    .block(self.block_id)
                    .dynamic()
                    .extend(chunk.map(|aid| self.instance.getPosition(perp_id, U256::from(aid))));
                async move { multicall.aggregate().await }
            });
            Ok(futures::future::try_join_all(batch_futs)
                .await
                .map_err(|err| DexError::Provider(err.into()))?
                .into_iter()
                .flatten()
                .map(|r| position_info_v0_to_v2(r.positionInfo))
                .collect())
        }
    }
}

fn position_info_v0_to_v2(v0: PositionInfo) -> PositionInfoV2 {
    PositionInfoV2 {
        accountId: v0.accountId,
        nextNodeId: v0.nextNodeId,
        prevNodeId: v0.prevNodeId,
        positionType: v0.positionType,
        depositCNS: v0.depositCNS,
        pricePNS: v0.pricePNS,
        lotLNS: v0.lotLNS,
        entryBlock: v0.entryBlock,
        pnlCNS: v0.pnlCNS,
        deltaPnlCNS: v0.deltaPnlCNS,
        premiumPnlCNS: v0.premiumPnlCNS,
        priceResiduePNSQ16: U256::ZERO,
    }
}

fn perpetual_info_v0_to_v2(v0: PerpetualInfo) -> PerpetualInfoV2 {
    PerpetualInfoV2 {
        name: v0.name,
        symbol: v0.symbol,
        priceDecimals: v0.priceDecimals,
        lotDecimals: v0.lotDecimals,
        linkFeedId: v0.linkFeedId,
        priceTolPer100K: v0.priceTolPer100K,
        marginTol: v0.marginTol,
        marginTolDecimals: v0.marginTolDecimals,
        refPriceMaxAgeSec: v0.refPriceMaxAgeSec,
        positionBalanceCNS: v0.positionBalanceCNS,
        insuranceBalanceCNS: v0.insuranceBalanceCNS,
        markPNS: v0.markPNS,
        markTimestamp: v0.markTimestamp,
        lastPNS: v0.lastPNS,
        lastTimestamp: v0.lastTimestamp,
        oraclePNS: v0.oraclePNS,
        oracleTimestampSec: v0.oracleTimestampSec,
        longOpenInterestLNS: v0.longOpenInterestLNS,
        shortOpenInterestLNS: v0.shortOpenInterestLNS,
        fundingStartBlock: v0.fundingStartBlock,
        fundingRatePct100k: v0.fundingRatePct100k,
        absFundingClampPctPer100K: v0.absFundingClampPctPer100K,
        status: v0.status,
        basePricePNS: v0.basePricePNS,
        maxBidPriceONS: v0.maxBidPriceONS,
        minBidPriceONS: v0.minBidPriceONS,
        maxAskPriceONS: v0.maxAskPriceONS,
        minAskPriceONS: v0.minAskPriceONS,
        numOrders: v0.numOrders,
        ignOracle: v0.ignOracle,
        fundingSumScalingExp: U256::ZERO,
    }
}

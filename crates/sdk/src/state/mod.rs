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
//!
//! The deployed contract can lag behind the revision the SDK targets, so the
//! snapshot detects its [`ContractFeatures`] first and degrades gracefully.

mod account;
mod event;
mod exchange;
mod fee;
mod l3_book;
mod order;
mod perpetual;
mod position;
mod version;

use std::collections::{HashMap, hash_map};

pub use account::*;
use alloy::{
    eips::BlockId,
    primitives::U256,
    providers::{CallItem, Provider},
};
pub use event::*;
pub use exchange::*;
use fastnum::UD64;
pub use fee::*;
use itertools::Itertools;
pub use l3_book::*;
pub use order::*;
pub use perpetual::*;
pub use position::*;
pub use version::*;

use crate::{
    Chain,
    abi::dex::{
        self,
        Exchange::{
            Order as OrderV0, OrderV2, PerpetualInfo, PerpetualInfoV2, PositionInfo,
            PositionInfoV2, getExchangeInfoReturn,
        },
    },
    error::{DexError, ProviderError},
    num, types,
};

/// Default number of orders to fetch via single call.
/// Assuming Monad's 8100 gas per storage slot access and 30M gas limit of
/// `eth_call`, plus some buffer.
const DEFAULT_ORDERS_PER_BATCH: usize = 1000;

/// Default number of positions to fetch via single call.
/// Assuming Monad's 8100 gas per storage slot access and 30M gas limit of
/// `eth_call`, plus some buffer.
const DEFAULT_POSITIONS_PER_BATCH: usize = 1000;

/// Number of perpetual IDs to probe for existence via single call on contracts
/// without the existence bitmap. Bounded by the same gas budget as the batches
/// above, with `getMarginFractions` being a couple of slots per ID.
const PERPETUAL_PROBES_PER_BATCH: usize = 256;

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
    /// at the latest safe/voted block.
    pub fn new(chain: &Chain, provider: P) -> Self {
        Self {
            chain: chain.clone(),
            instance: dex::Exchange::new(chain.exchange(), provider.clone()),
            provider,
            block_id: BlockId::Number(alloy::eips::BlockNumberOrTag::Safe),
            perpetuals: chain.perpetuals.clone(),
            accounts: vec![],
            all_positions: false,
            orders_per_batch: DEFAULT_ORDERS_PER_BATCH,
            positions_per_batch: DEFAULT_POSITIONS_PER_BATCH,
        }
    }

    /// Sets the block number or tag to fetch the state at (default:
    /// [`alloy::eips::BlockNumberOrTag::Safe`]). If tag is provided, it gets
    /// converted to a specific block number first to ensure state
    /// consistency.
    pub fn at_block(mut self, block: BlockId) -> Self {
        self.block_id = block;
        self
    }

    /// Sets the list of perpetual contract IDs to fetch the state for.
    ///
    /// An empty list (the default, see [`Chain::perpetuals`]) means *every*
    /// perpetual listed on the exchange, discovered on-chain.
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

        // Probe once to learn what the deployed contract exposes - it can lag
        // behind the revision the SDK is compiled against.
        let mut features = ContractFeatures::probe(
            &self.instance,
            self.block_id,
            self.perpetuals.first().copied(),
        )
        .await;

        // Resolve the set of perpetuals to track, discovering it on-chain when
        // it was not configured explicitly
        if self.perpetuals.is_empty() {
            self.perpetuals = discover_perpetuals(
                &self.instance,
                &self.provider,
                self.block_id,
                features,
                self.chain.excluded_perpetuals(),
            )
            .await?;
            // An unversioned contract could not be probed for the V2 getters
            // without a perpetual to probe against; now there is one
            if let Some(perp_id) = self.perpetuals.first().copied() {
                features
                    .probe_v2_state_getters(&self.instance, self.block_id, perp_id)
                    .await;
            }
        }

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

        // Every fee schedule perpetuals resolve their fees from, alongside the
        // perpetual contracts' own parameters, state and active orders. Both
        // are keyed off the perpetual ids resolved above and pinned to the same
        // block, so they are independent of each other.
        let (fee_schedules, perpetuals) =
            futures::try_join!(self.fee_schedules(features), self.perpetuals(instant, features))?;

        let accounts = if !self.accounts.is_empty() {
            // Accounts parameters, state and open positions if specific accounts requested
            self.accounts(instant, &perpetuals, collateral_converter, features)
                .await?
        } else if self.all_positions {
            // All positions with corresponding accounts without parameters and balance
            // snapshot
            self.position_accounts(
                instant,
                &perpetuals,
                num_of_accounts.to(),
                collateral_converter,
                features,
            )
            .await?
        } else {
            HashMap::new()
        };

        Ok(Exchange::new(
            self.chain.clone(),
            instant,
            features,
            collateral_converter,
            funding_interval.to(),
            collateral_converter.from_unsigned(min_post),
            collateral_converter.from_unsigned(min_settle),
            collateral_converter.from_unsigned(recycle_fee),
            fee_schedules,
            perpetuals,
            accounts,
            is_halted,
            self.all_positions,
        ))
    }

    /// Fetches every fee schedule perpetuals resolve their fees from: the two
    /// exchange-wide ones plus the custom schedule keyed by each perpetual
    /// being tracked.
    ///
    /// A custom schedule is fetched whether or not the perpetual it is keyed by
    /// currently points at it - the two are independent, and the registry has
    /// to be able to resolve the rates of a `PerpFeeSchedIdSet` repoint that
    /// arrives without a `FeeScheduleSet` of its own.
    ///
    /// Pre-v1.1.7.4 contracts have no schedule registry - fees live on the
    /// perpetual itself and no event ever repoints one at a shared schedule, so
    /// empty schedules are returned and never consulted.
    async fn fee_schedules(
        &self,
        features: ContractFeatures,
    ) -> Result<FeeScheduleRegistry, DexError> {
        if !features.keyed_fee_schedules() {
            return Ok(FeeScheduleRegistry::new(
                FeeSchedule::flat(FeeScheduleKey::Default, UD64::ZERO, UD64::ZERO),
                FeeSchedule::flat(FeeScheduleKey::RwaDefault, UD64::ZERO, UD64::ZERO),
                HashMap::new(),
            ));
        }
        // Resolved from the deployed version: v1.1.7.5 redenominated the stored
        // rates from hundred-thousandths to millionths, so the same integer means
        // a tenth of what it used to.
        let fee_converter = features.fee_rate_converter();
        let (default_call, rwa_call) = (
            self.instance
                .getDefaultPerpFeeSchedule()
                .block(self.block_id),
            self.instance
                .getFeeScheduleById(FeeScheduleKey::RwaDefault.to_raw())
                .block(self.block_id),
        );
        let custom_calls = self.perpetuals.iter().map(|perp_id| {
            let key = FeeScheduleKey::Custom(*perp_id);
            let call = self
                .instance
                .getFeeScheduleById(key.to_raw())
                .block(self.block_id);
            async move {
                call.call().await.map(|schedule| {
                    (
                        *perp_id,
                        FeeSchedule::new(
                            key,
                            schedule.takerFeesPer100K,
                            schedule.makerFeesPer100K,
                            fee_converter,
                        ),
                    )
                })
            }
        });
        let (default, rwa, custom) = futures::try_join!(
            default_call.call().into_future(),
            rwa_call.call().into_future(),
            futures::future::try_join_all(custom_calls),
        )
        .map_err(|err| DexError::Provider(err.into()))?;
        Ok(FeeScheduleRegistry::new(
            FeeSchedule::new(
                FeeScheduleKey::Default,
                default.takerFeesPer100K,
                default.makerFeesPer100K,
                fee_converter,
            ),
            FeeSchedule::new(
                FeeScheduleKey::RwaDefault,
                rwa.takerFeesPer100K,
                rwa.makerFeesPer100K,
                fee_converter,
            ),
            custom.into_iter().collect(),
        ))
    }

    /// Fetches the fee schedule a perpetual resolves its fees from.
    ///
    /// Pre-v1.1.7.4 contracts have a single fee pair per perpetual, which is
    /// normalized to a flat schedule under the default key - the same rate in
    /// every tier, as no tiers exist there.
    async fn fetch_fee_schedule(
        &self,
        perp_id: U256,
        features: ContractFeatures,
    ) -> Result<FeeSchedule, alloy::contract::Error> {
        let fee_converter = features.fee_rate_converter();
        if features.keyed_fee_schedules() {
            self.instance
                .getPerpFeeSchedule(perp_id)
                .block(self.block_id)
                .call()
                .await
                .map(|schedule| {
                    FeeSchedule::new(
                        FeeScheduleKey::from_raw(schedule.feeSchedId),
                        schedule.takerFeesPer100K,
                        schedule.makerFeesPer100K,
                        fee_converter,
                    )
                })
        } else {
            let (maker_fee_call, taker_fee_call) = (
                self.instance.getMakerFee(perp_id).block(self.block_id),
                self.instance.getTakerFee(perp_id).block(self.block_id),
            );
            let (maker_fee, taker_fee) = futures::try_join!(
                maker_fee_call.call().into_future(),
                taker_fee_call.call().into_future(),
            )?;
            Ok(FeeSchedule::flat(
                FeeScheduleKey::Default,
                fee_converter.from_unsigned(taker_fee),
                fee_converter.from_unsigned(maker_fee),
            ))
        }
    }

    /// Fetches `PerpetualInfoV2`, falling back to the V0 ABI when the contract
    /// has not been upgraded yet (the V0 layout omits `fundingSumScalingExp`,
    /// which is defaulted to zero on the V0 path).
    async fn fetch_perpetual_info(
        &self,
        perp_id: U256,
        features: ContractFeatures,
    ) -> Result<PerpetualInfoV2, alloy::contract::Error> {
        if features.v2_state_getters() {
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
        features: ContractFeatures,
    ) -> Result<PositionInfoV2, alloy::contract::Error> {
        if features.v2_state_getters() {
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
            // Must be pinned like every other call here: the count bounds the
            // account IDs `position_accounts` reads, and `getPosition*` reverts
            // for an account that does not exist at the snapshot block.
            self.instance.numberOfAccounts().block(self.block_id),
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
        features: ContractFeatures,
    ) -> Result<HashMap<types::PerpetualId, perpetual::Perpetual>, DexError> {
        let perpetual_futs = self.perpetuals.iter().map(|perp_id| async move {
            let pid = U256::from(*perp_id);
            let margins_call = self
                .instance
                .getMarginFractions(pid, U256::ZERO)
                .block(self.block_id);

            futures::try_join!(
                self.fetch_perpetual_info(pid, features),
                self.fetch_fee_schedule(pid, features),
                margins_call.call().into_future(),
            )
            .map(|(perp_info, fee_schedule, margins)| (*perp_id, perp_info, fee_schedule, margins))
        });

        let mut perpetuals = futures::future::try_join_all(perpetual_futs)
            .await
            .map_err(|err| DexError::Provider(err.into()))?
            .into_iter()
            .map(|(perp_id, perp_info, fee_schedule, margins)| {
                let perp = Perpetual::new(
                    instant,
                    perp_id,
                    &perp_info,
                    fee_schedule,
                    margins.perpInitMarginFracHdths,
                    margins.perpMaintMarginFracHdths,
                );
                (perp_id, perp)
            })
            .collect::<HashMap<_, _>>();

        // Fetching orders one perp at a time to bound parallel requests
        for perp in perpetuals.values_mut() {
            self.perpetual_orders(perp, features).await?;
        }

        Ok(perpetuals)
    }

    async fn perpetual_orders(
        &self,
        perp: &mut perpetual::Perpetual,
        features: ContractFeatures,
    ) -> Result<(), DexError> {
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

        let orders = self.fetch_orders(pid, &order_ids, features).await?;

        let (instant, base_price, price_converter, size_converter, leverage_converter) = (
            perp.instant(),
            perp.base_price(),
            perp.price_converter(),
            perp.size_converter(),
            perp.leverage_converter(),
        );

        // Collect all orders first, then add via snapshot method to preserve FIFO
        // ordering
        let orders: Vec<Order> = orders
            .into_iter()
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

    /// Batches `getOrder`/`getOrderV2` calls for the given order IDs of a
    /// single perpetual. Normalizes both ABI versions to `OrderV2`; the V0
    /// layout omits the builder attribution, which is defaulted to none on
    /// the V0 path.
    async fn fetch_orders(
        &self,
        perp_id: U256,
        order_ids: &[types::OrderId],
        features: ContractFeatures,
    ) -> Result<Vec<OrderV2>, DexError> {
        let order_ids = order_ids.to_vec();
        if features.builder_attribution() {
            aggregate_batched(order_ids, self.orders_per_batch, |chunk| {
                let multicall = self
                    .provider
                    .multicall()
                    .block(self.block_id)
                    .dynamic()
                    .extend(
                        chunk
                            .iter()
                            .map(|oid| self.instance.getOrderV2(perp_id, U256::from(oid.get()))),
                    );
                async move { multicall.aggregate().await }
            })
            .await
        } else {
            Ok(aggregate_batched(order_ids, self.orders_per_batch, |chunk| {
                let multicall = self
                    .provider
                    .multicall()
                    .block(self.block_id)
                    .dynamic()
                    .extend(
                        chunk
                            .iter()
                            .map(|oid| self.instance.getOrder(perp_id, U256::from(oid.get()))),
                    );
                async move { multicall.aggregate().await }
            })
            .await?
            .into_iter()
            .map(order_v0_to_v2)
            .collect())
        }
    }

    async fn accounts(
        &self,
        instant: types::StateInstant,
        perpetuals: &HashMap<types::PerpetualId, perpetual::Perpetual>,
        collateral_converter: num::Converter,
        features: ContractFeatures,
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
            let fee_tier = self
                .fetch_account_fee_tier(acc_info.accountId, features)
                .await?;
            let perps_with_positions = perpetuals_with_position(&acc_info.positions);
            let position_futs = perps_with_positions.iter().map(|perp_id| async {
                self.fetch_position_info(U256::from(*perp_id), acc_info.accountId, features)
                    .await
                    .map(|pos_info| (*perp_id, pos_info))
                    .map_err(|err| DexError::Provider(err.into()))
            });
            let positions = futures::future::try_join_all(position_futs).await?;
            Ok::<_, DexError>((acc_info.accountId, acc_info, fee_tier, positions))
        });

        Ok(futures::future::try_join_all(account_futs)
            .await?
            .into_iter()
            .map(|(acc_id, acc_info, fee_tier, positions)| {
                (
                    acc_id.to(),
                    Account::new(
                        instant,
                        acc_id.to(),
                        &acc_info,
                        fee_tier,
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

    /// Fetches the fee tier of an account, `None` on contracts that have no
    /// per-account tiers.
    async fn fetch_account_fee_tier(
        &self,
        account_id: U256,
        features: ContractFeatures,
    ) -> Result<Option<types::FeeTier>, DexError> {
        if !features.keyed_fee_schedules() {
            return Ok(None);
        }
        self.instance
            .getAccountFeeTier(account_id)
            .block(self.block_id)
            .call()
            .await
            .map(|tier| Some(tier.to()))
            .map_err(|err| DexError::Provider(err.into()))
    }

    async fn position_accounts(
        &self,
        instant: types::StateInstant,
        perpetuals: &HashMap<types::PerpetualId, perpetual::Perpetual>,
        num_accounts: usize,
        collateral_converter: num::Converter,
        features: ContractFeatures,
    ) -> Result<HashMap<types::AccountId, Account>, DexError> {
        let mut accounts: HashMap<types::AccountId, Account> = HashMap::new();
        for (perp_id, perp) in perpetuals {
            let pid = U256::from(*perp_id);
            let infos = self
                .fetch_position_infos_for_perp(pid, num_accounts, features)
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
        features: ContractFeatures,
    ) -> Result<Vec<PositionInfoV2>, DexError> {
        let account_ids = (1..num_accounts + 1).collect::<Vec<_>>();
        if features.v2_state_getters() {
            Ok(aggregate_batched(account_ids, self.positions_per_batch, |chunk| {
                let multicall = self
                    .provider
                    .multicall()
                    .block(self.block_id)
                    .dynamic()
                    .extend(
                        chunk
                            .iter()
                            .map(|aid| self.instance.getPositionV2(perp_id, U256::from(*aid))),
                    );
                async move { multicall.aggregate().await }
            })
            .await?
            .into_iter()
            .map(|r| r.positionInfo)
            .collect())
        } else {
            Ok(aggregate_batched(account_ids, self.positions_per_batch, |chunk| {
                let multicall = self
                    .provider
                    .multicall()
                    .block(self.block_id)
                    .dynamic()
                    .extend(
                        chunk
                            .iter()
                            .map(|aid| self.instance.getPosition(perp_id, U256::from(*aid))),
                    );
                async move { multicall.aggregate().await }
            })
            .await?
            .into_iter()
            .map(|r| position_info_v0_to_v2(r.positionInfo))
            .collect())
        }
    }
}

/// Runs `call` over `items` in concurrent batches of `batch_size`, halving any
/// batch that fails and retrying it.
///
/// A multicall can fail for reasons that belong to the batch rather than to any
/// single call in it - overwhelmingly, exhausting the node's `eth_call` gas
/// budget. Per-call cost is not uniform across perpetual contracts: reading a
/// position from a paused contract with no funding history has been measured at
/// ~30x the cost of reading one from an active contract, so no single batch
/// size is both efficient and safe. Since the perpetual set is discovered
/// rather than configured, such a contract is found rather than chosen, and a
/// fixed batch size would fail the whole snapshot on it.
///
/// Splitting converges on a size the node will serve, keeping the batch large
/// (and the snapshot fast) for the common case. A batch of one that still fails
/// is a genuine error and propagates - the alternative, dropping it, would
/// silently omit state from a snapshot that presents itself as complete.
async fn aggregate_batched<T, R, F, Fut>(
    items: Vec<T>,
    batch_size: usize,
    call: F,
) -> Result<Vec<R>, DexError>
where
    T: Clone,
    F: Fn(Vec<T>) -> Fut,
    Fut: Future<Output = Result<Vec<R>, alloy::providers::MulticallError>>,
{
    // Batches still to fetch, each with its offset in `items` so the results can
    // be restored to the original order after any amount of splitting
    let mut pending = items
        .chunks(batch_size.max(1))
        .enumerate()
        .map(|(i, chunk)| (i * batch_size, chunk.to_vec()))
        .collect::<Vec<_>>();
    let mut fetched: Vec<(usize, Vec<R>)> = Vec::with_capacity(pending.len());

    while !pending.is_empty() {
        let results =
            futures::future::join_all(pending.iter().map(|(_, chunk)| call(chunk.clone()))).await;
        let mut retry = Vec::new();
        for ((offset, chunk), result) in pending.into_iter().zip(results) {
            match result {
                Ok(values) => fetched.push((offset, values)),
                Err(_) if chunk.len() > 1 => {
                    let mid = chunk.len() / 2;
                    retry.push((offset + mid, chunk[mid..].to_vec()));
                    retry.push((offset, chunk[..mid].to_vec()));
                },
                Err(err) => return Err(DexError::Provider(err.into())),
            }
        }
        pending = retry;
    }

    fetched.sort_by_key(|(offset, _)| *offset);
    Ok(fetched.into_iter().flat_map(|(_, values)| values).collect())
}

/// Returns the IDs of every perpetual contract listed on the exchange at
/// `block_id`.
///
/// The exchange reports its own listings, so a client does not need to be
/// configured with them - see [`Chain::perpetuals`].
pub async fn listed_perpetuals<P: Provider + Clone>(
    chain: &Chain,
    provider: P,
    block_id: BlockId,
) -> Result<Vec<types::PerpetualId>, DexError> {
    let instance = dex::Exchange::new(chain.exchange(), provider.clone());
    let features = ContractFeatures::probe(&instance, block_id, None).await;
    discover_perpetuals(&instance, &provider, block_id, features, chain.excluded_perpetuals()).await
}

/// Returns the IDs of every perpetual listed on the exchange, less the ones
/// [`Chain::excluded_perpetuals`] leaves out.
///
/// Reads the existence bitmap on v1.1.7.4+, a single call covering the whole
/// `0..=`[`types::MAX_PERPETUAL_ID`] ID space. Older deployments have no
/// bitmap, so existence is probed by batching `getMarginFractions` over that ID
/// space - it reverts `ContractDoesNotExist` for unlisted IDs and reads only a
/// couple of slots for listed ones.
async fn discover_perpetuals<P: Provider + Clone>(
    instance: &dex::Exchange::ExchangeInstance<P>,
    provider: &P,
    block_id: BlockId,
    features: ContractFeatures,
    excluded: &[types::PerpetualId],
) -> Result<Vec<types::PerpetualId>, DexError> {
    if features.perpetual_discovery() {
        let bitmap = instance
            .getPerpetualExistsBitmap()
            .block(block_id)
            .call()
            .await
            .map_err(|err| DexError::Provider(err.into()))?;
        return Ok(bitmap
            .into_iter()
            .enumerate()
            .flat_map(|(word, bits)| {
                (0..U256::BITS).filter_map(move |bit| {
                    let perp_id = (word * U256::BITS + bit) as types::PerpetualId;
                    (bits.bit(bit) && perp_id <= types::MAX_PERPETUAL_ID).then_some(perp_id)
                })
            })
            .filter(|perp_id| !excluded.contains(perp_id))
            .collect());
    }

    let probe_batch_futs = (0..=types::MAX_PERPETUAL_ID)
        .filter(|perp_id| !excluded.contains(perp_id))
        .chunks(PERPETUAL_PROBES_PER_BATCH)
        .into_iter()
        .map(|chunk| {
            let perp_ids = chunk.collect::<Vec<_>>();
            let multicall = provider
                .multicall()
                .block(block_id)
                .dynamic::<dex::Exchange::getMarginFractionsCall>()
                // Probing IS the point here: an unlisted ID reverts, and the
                // batch must survive that
                .extend_calls(perp_ids.iter().map(|perp_id| {
                    CallItem::from(instance.getMarginFractions(U256::from(*perp_id), U256::ZERO))
                        .with_failure_allowed()
                }));
            async move { multicall.aggregate3().await.map(|res| (perp_ids, res)) }
        })
        .collect::<Vec<_>>();

    Ok(futures::future::try_join_all(probe_batch_futs)
        .await
        .map_err(|err| DexError::Provider(err.into()))?
        .into_iter()
        .flat_map(|(perp_ids, results)| {
            perp_ids
                .into_iter()
                .zip(results)
                .filter_map(|(perp_id, result)| result.is_ok().then_some(perp_id))
        })
        .collect())
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

fn order_v0_to_v2(v0: OrderV0) -> OrderV2 {
    OrderV2 {
        accountId: v0.accountId,
        orderType: v0.orderType,
        priceONS: v0.priceONS,
        lotLNS: v0.lotLNS,
        recycleFeeRaw: v0.recycleFeeRaw,
        expiryBlock: v0.expiryBlock,
        leverageHdths: v0.leverageHdths,
        orderId: v0.orderId,
        prevOrderId: v0.prevOrderId,
        nextOrderId: v0.nextOrderId,
        maxNegPnlCollatBPS: v0.maxNegPnlCollatBPS,
        builderId: 0,
        builderFeePer100K: 0,
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

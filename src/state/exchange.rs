use super::*;
use crate::{Chain, abi::dex::Exchange::ExchangeEvents, stream, types::EventContext};
use fastnum::{D256, UD64, UD128};
use itertools::chain;

pub type StateBlockEvents = types::BlockEvents<types::EventContext<Vec<StateEvents>>>;

/// Exchange state snapshot.
///
/// [`super::SnapshotBuilder`] can be used to create the snapshot at
/// specified/latest block, which can then be kept up to date by
/// calling [`Self::apply_events`] with events from [`crate::stream::raw`].
#[derive(Clone, Debug)]
pub struct Exchange {
    chain: Chain,
    instant: types::StateInstant,
    collateral_converter: num::Converter,
    funding_interval_blocks: u32,
    min_post: UD128,
    min_settle: UD128,
    recycle_fee: UD128,
    perpetuals: HashMap<types::PerpetualId, Perpetual>,
    accounts: HashMap<types::AccountId, Account>,
    is_halted: bool,
    track_all_accounts: bool,
}

impl Exchange {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        chain: Chain,
        instant: types::StateInstant,
        collateral_converter: num::Converter,
        funding_interval_blocks: u32,
        min_post: UD128,
        min_settle: UD128,
        recycle_fee: UD128,
        perpetuals: HashMap<types::PerpetualId, Perpetual>,
        accounts: HashMap<types::AccountId, Account>,
        is_halted: bool,
        track_all_accounts: bool,
    ) -> Self {
        Self {
            chain,
            instant,
            collateral_converter,
            funding_interval_blocks,
            min_post,
            min_settle,
            recycle_fee,
            perpetuals,
            accounts,
            is_halted,
            track_all_accounts,
        }
    }

    /// Revision of the exchange smart contract the SDK targeted at.
    pub const fn revision() -> &'static str {
        crate::abi::DEX_REVISION
    }

    /// Chain the snapshot collected from.
    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// Instant the snapshot is consistent with or was last updated at.
    pub fn instant(&self) -> types::StateInstant {
        self.instant
    }

    /// Converter of fixed-point <-> decimal numbers for collateral token
    /// amounts.
    pub fn collateral_converter(&self) -> num::Converter {
        self.collateral_converter
    }

    /// Funding interval in blocks.
    ///
    /// Each perpetual contract has own [Perpetual::funding_start_block]  this interval
    /// applied to.
    pub fn funding_interval_blocks(&self) -> u32 {
        self.funding_interval_blocks
    }

    /// Minimal amount in collateral token that can be posted to the book.
    pub fn min_post(&self) -> UD128 {
        self.min_post
    }

    /// Minimal amount in collateral token that can be settled.
    pub fn min_settle(&self) -> UD128 {
        self.min_settle
    }

    /// Amount in collateral token locked with each posted order to
    /// pay the account that cleans it up:
    /// * When cancelled/changed by the original poster -> the original poster
    /// * When filled -> the original poster
    /// * In all other cases -> the one that performed the recycling
    pub fn recycle_fee(&self) -> UD128 {
        self.recycle_fee
    }

    /// Perpetual contracts state tracked within the exchange, according to initial
    /// snapshot building configuration.
    pub fn perpetuals(&self) -> &HashMap<types::PerpetualId, Perpetual> {
        &self.perpetuals
    }

    /// Accounts state tracked within the exchange, according to initial
    /// snapshot building configuration.
    pub fn accounts(&self) -> &HashMap<types::AccountId, Account> {
        &self.accounts
    }

    /// Indicates if exchange is being halted.
    pub fn is_halted(&self) -> bool {
        self.is_halted
    }

    /// Updates state snapshot by applying raw exchange events from the
    /// specific block.
    ///
    /// Blocks expected to arrive strictly in-order, with already applied blocks being ignored,
    /// to enforce state consistency as most raw events provide only incremental state update
    /// information rather than full piece of state snapshot.
    ///
    /// Exchange emits two categories of events:
    /// * State mutation events
    /// * Order request error responses, for requests issued in batches via
    ///   [`crate::abi::dex::Exchange::ExchangeInstance::execOpsAndOrders`]
    ///   with `revertOnFail` = false.
    ///
    /// This method applies state mutation events only to tracked perpetual contracts and accounts
    /// provided to [`SnapshotBuilder`] during the initial snapshot creation, and returns order request
    /// failure events only for requests issues by tracked accounts.
    /// Successfull order book mutations are applied to all orders of tracked perpetual contracts,
    /// so client code can keep up to date order book representation externally if needed.
    ///
    /// # Returns
    ///
    /// On success, list of state mutation and failure [`StateEvents`] produced from the original raw events,
    /// filtered as described above and with numeric systems conversion applied.
    ///
    /// [`StateEvents`] are roughly resemble [`crate::abi::dex::Exchange::ExchangeEvents`] so corresponding
    /// smart contract documentation and raw event data for error responses could be helpful with debugging,
    /// but there is no exact match and more than one state event can be emitted in response to a single raw event, eg.
    /// processing of single order event produces up to two account events on top of order events within the
    /// same event context.
    ///
    /// On failure, the corresponding [`DexError`], any of which indicates some inconsistency in event sequence or
    /// event handling logic and should not be ignored as it may lead to state inconsistency.
    ///
    pub fn apply_events(
        &mut self,
        events: &stream::RawBlockEvents,
    ) -> Result<Option<StateBlockEvents>, DexError> {
        let next_instant = events.instant();
        if self.instant >= next_instant {
            // Block already applied
            return Ok(None);
        }
        if self.instant.block_number() + 1 < next_instant.block_number() {
            // Block arrived out of order
            return Err(DexError::BlockOutOfOrder(
                self.instant.block_number() + 1,
                next_instant.block_number(),
            ));
        }

        // Apply events sequentially and accumulate produced state events,
        // keeping intermediate context as many order events are incremental
        let mut order_context: Option<OrderContext> = None;
        let mut prev_tx_index: Option<u64> = None;
        let mut state_events = vec![];
        for event in events.events() {
            if prev_tx_index.is_some_and(|idx| idx < event.tx_index()) {
                // Reset order context at the transaction boundary
                order_context.take();
            }
            let result = self.apply_raw_event(next_instant, event, &mut order_context)?;
            if !result.is_empty() {
                state_events.push(event.pass(result));
            }
            prev_tx_index = Some(event.tx_index());
        }

        // Commit instant, can produce its own set of events
        self.instant = events.instant();
        let mut perp_events = vec![];
        for perp in self.perpetuals.values_mut() {
            let result = perp.update_state_instant(self.instant);
            if !result.is_empty() {
                perp_events.push(result.clone());
                state_events.push(EventContext::empty(result));
            }
        }

        // Applying produced state events as a second pass
        for event in perp_events.iter().flatten() {
            let result = self.apply_state_event(self.instant, event)?;
            if !result.is_empty() {
                state_events.push(EventContext::empty(result));
            }
        }

        Ok(Some(StateBlockEvents::new(self.instant, state_events)))
    }

    fn apply_raw_event(
        &mut self,
        instant: types::StateInstant,
        event: &stream::RawEvent,
        ctx: &mut Option<OrderContext>,
    ) -> Result<Vec<StateEvents>, DexError> {
        let cc = self.collateral_converter;

        let must_ctx = || {
            ctx.as_ref().ok_or(DexError::OrderContextExpected(
                event.tx_index(),
                event.log_index(),
            ))
        };

        Ok(match event.event() {
            ExchangeEvents::AccountCreated(e) => {
                if self.track_all_accounts {
                    self.accounts.insert(
                        e.id.to(),
                        Account::from_event(instant, e.id.to(), e.account),
                    );
                    vec![StateEvents::Account(AccountEvent {
                        account_id: e.id.to(),
                        request_id: None,
                        r#type: AccountEventType::Created(e.id.to()),
                    })]
                } else {
                    vec![]
                }
            }
            ExchangeEvents::AccountFreeze(e) => self
                .account(e.accountId)
                .map(|acc| {
                    acc.update_frozen(instant, e.status > 0);
                    StateEvents::account(acc, ctx, AccountEventType::Frozen(acc.frozen()))
                })
                .into_iter()
                .collect(),
            ExchangeEvents::AccountFrozen(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::AccountFrozen))
                .into_iter()
                .collect(),
            ExchangeEvents::AccountLiquidationCredit(e) => self
                .account(e.accountId)
                .map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.endBalanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                })
                .into_iter()
                .collect(),
            ExchangeEvents::AdministratorUpdated(_) => vec![], // Ignored
            ExchangeEvents::AmountExceedsAvailableBalance(e) => self
                .err_ctx(ctx, event)?
                .map(|ctx| {
                    StateEvents::order_error(
                        ctx,
                        OrderErrorType::AmountExceedsAvailableBalance(
                            cc.from_unsigned(e.amountCNS),
                            cc.from_unsigned(e.availableBalanceCNS),
                        ),
                    )
                })
                .into_iter()
                .collect(),
            ExchangeEvents::BankruptcyPriceExceedsReferencePrice(_) => vec![], // Ignored
            ExchangeEvents::BuyToLiquidateBuyerRestricted(_) => vec![],        // Ignored
            ExchangeEvents::BuyToLiquidateParamsUpdated(_) => vec![],          // Ignored
            ExchangeEvents::BuyToLiquidateThresholdUpdated(_) => vec![],       // Ignored
            ExchangeEvents::BuyToLiquidateRestrictionUpdated(_) => vec![],     // Ignored
            ExchangeEvents::CancelExistingInvalidCloseOrders(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| {
                    StateEvents::order_error(ctx, OrderErrorType::CancelExistingInvalidCloseOrders)
                })
                .into_iter()
                .collect(),
            ExchangeEvents::CantBuyToLiquidate(_) => vec![], // Ignored
            ExchangeEvents::CantChangeCloseOrder(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::CantChangeCloseOrder))
                .into_iter()
                .collect(),
            ExchangeEvents::CantDeleverageAgainstOpposingPositions(_) => vec![], // Ignored
            ExchangeEvents::CantLiquidatePosAboveMMR(_) => vec![],               // Ignored
            ExchangeEvents::ChangeExpiredOrderNeedsNewExpiry(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| {
                    StateEvents::order_error(ctx, OrderErrorType::ChangeExpiredOrderNeedsNewExpiry)
                })
                .into_iter()
                .collect(),
            ExchangeEvents::ClearingExpiredOrder(e) => chain!(
                if let Some(perp) = self.perpetual(e.perpId) {
                    let order = perp.remove_order(e.orderId.to())?;
                    Some(StateEvents::order(
                        perp,
                        &order,
                        ctx,
                        OrderEventType::Removed,
                    ))
                } else {
                    None
                },
                self.account(e.accountId).map(|acc| {
                    acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                    StateEvents::account(
                        acc,
                        ctx,
                        AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                    )
                }),
                self.account(e.recyclerAccountId).map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.recyclerBalanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                }),
            )
            .collect(),
            ExchangeEvents::ClearingFrozenAccountOrder(e) => chain!(
                if let Some(perp) = self.perpetual(e.perpId) {
                    let order = perp.remove_order(e.orderId.to())?;
                    Some(StateEvents::order(
                        perp,
                        &order,
                        ctx,
                        OrderEventType::Removed,
                    ))
                } else {
                    None
                },
                self.account(e.accountId).map(|acc| {
                    acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                    StateEvents::account(
                        acc,
                        ctx,
                        AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                    )
                }),
                self.account(e.recyclerAccountId).map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.recyclerBalanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                }),
            )
            .collect(),
            ExchangeEvents::ClearingInvalidCloseOrder(e) => chain!(
                if let Some(perp) = self.perpetual(e.perpId) {
                    let order = perp.remove_order(e.orderId.to())?;
                    Some(StateEvents::order(
                        perp,
                        &order,
                        ctx,
                        OrderEventType::Removed,
                    ))
                } else {
                    None
                },
                self.account(e.accountId).map(|acc| {
                    acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                    StateEvents::account(
                        acc,
                        ctx,
                        AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                    )
                }),
                self.account(e.recyclerAccountId).map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.recyclerBalanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                }),
            )
            .collect(),
            ExchangeEvents::ClearingSelfMatchingOrder(e) => chain!(
                if let Some(perp) = self.perpetual(e.perpId) {
                    let order = perp.remove_order(e.orderId.to())?;
                    Some(StateEvents::order(
                        perp,
                        &order,
                        ctx,
                        OrderEventType::Removed,
                    ))
                } else {
                    None
                },
                self.account(e.accountId).map(|acc| {
                    acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                    StateEvents::account(
                        acc,
                        ctx,
                        AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                    )
                }),
                self.account(e.recyclerAccountId).map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.recyclerBalanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                }),
            )
            .collect(),
            ExchangeEvents::CloseOrderExceedsPosition(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::CloseOrderExceedsPosition))
                .into_iter()
                .collect(),
            ExchangeEvents::CloseOrderPositionMismatch(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| {
                    StateEvents::order_error(ctx, OrderErrorType::CloseOrderPositionMismatch)
                })
                .into_iter()
                .collect(),
            ExchangeEvents::CollateralDeposit(e) => self
                .account(e.accountId)
                .map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                })
                .into_iter()
                .collect(),
            ExchangeEvents::CollateralWithdrawal(e) => self
                .account(e.accountId)
                .map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                })
                .into_iter()
                .collect(),
            ExchangeEvents::ContractAdded(_) => vec![], // TODO: support tracking of newly created contracts
            ExchangeEvents::ContractIsPaused(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::ContractIsPaused))
                .into_iter()
                .collect(),
            ExchangeEvents::ContractLinkFeedUpdated(e) => self
                .perpetual(e.perpId)
                .map(|perp| {
                    perp.update_oracle_feed_id(instant, e.feedId);
                    StateEvents::perpetual(
                        perp,
                        PerpetualEventType::OracleConfigurationUpdated {
                            is_used: perp.is_oracle_used(),
                            feed_id: perp.oracle_feed_id(),
                        },
                    )
                })
                .into_iter()
                .collect(),
            ExchangeEvents::ContractPaused(e) => self
                .perpetual(e.perpId)
                .map(|perp| {
                    perp.update_paused(instant, e.paused);
                    StateEvents::perpetual(perp, PerpetualEventType::Paused(perp.is_paused()))
                })
                .into_iter()
                .collect(),
            ExchangeEvents::ContractRemoved(e) => self
                .perpetual(e.perpId)
                .map(|perp| {
                    perp.update_paused(instant, true);
                    StateEvents::perpetual(perp, PerpetualEventType::Paused(perp.is_paused()))
                })
                .into_iter()
                .collect(),
            ExchangeEvents::CrossesBook(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::CrossesBook))
                .into_iter()
                .collect(),
            ExchangeEvents::DeleveragePositionListEmpty(_) => vec![], // Ignored
            ExchangeEvents::ExceedsLastExecutionBlock(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::ExceedsLastExecutionBlock))
                .into_iter()
                .collect(),
            ExchangeEvents::ExchangeHalted(e) => {
                self.is_halted = e.halted;
                vec![StateEvents::Exchange(ExchangeEvent::Halted(self.is_halted))]
            }
            ExchangeEvents::FeeParamsUpdated(_) => vec![], // Ignored
            ExchangeEvents::FundingClampPctUpdated(_) => vec![], // Ignored
            ExchangeEvents::FundingEventCompleted(e) => {
                if let Some(perp) = self.perpetual(e.perpId) {
                    perp.update_funding(
                        instant,
                        perp.funding_rate_converter()
                            .from_signed(e.actualRatePct100k),
                        perp.price_converter()
                            .from_i64(e.fundingPaymentPNS.as_i64()),
                        e.fundingEventBlock.to(),
                    );
                }
                vec![]
            }
            ExchangeEvents::FundingEventSetTooEarly(_) => vec![], // Ignored
            ExchangeEvents::FundingPriceExceedsTol(_) => vec![],  // Ignored
            ExchangeEvents::FundingSumAlreadySet(_) => vec![],    // Ignored
            ExchangeEvents::IgnoreOracleUpdated(e) => self
                .perpetual(e.perpId)
                .map(|perp| {
                    perp.update_is_oracle_used(instant, !e.ignOracle);
                    StateEvents::perpetual(
                        perp,
                        PerpetualEventType::OracleConfigurationUpdated {
                            is_used: perp.is_oracle_used(),
                            feed_id: perp.oracle_feed_id(),
                        },
                    )
                })
                .into_iter()
                .collect(),
            ExchangeEvents::ImmediateOrCancelExecuted(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::ImmediateOrCancelExecuted))
                .into_iter()
                .collect(),
            ExchangeEvents::IncreasePositionCollateral(e) => chain!(
                self.position(e.accountId, e.perpId)?.map(|(pos, _)| {
                    pos.update_deposit(instant, cc.from_unsigned(e.positionDepositCNS));
                    StateEvents::position(
                        pos,
                        ctx,
                        PositionEventType::DepositUpdated(pos.deposit()),
                    )
                }),
                self.account(e.accountId).map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                }),
            )
            .collect(),
            ExchangeEvents::InitialMarginFractionUpdated(e) => self
                .perpetual(e.perpId)
                .map(|perp| {
                    perp.update_initial_margin(
                        instant,
                        perp.leverage_converter()
                            .from_unsigned(e.initMarginFracHdths),
                    );
                    StateEvents::perpetual(
                        perp,
                        PerpetualEventType::InitialMarginFractionUpdated(perp.initial_margin()),
                    )
                })
                .into_iter()
                .collect(),
            ExchangeEvents::InsolventPositionCannotBeForcedClose(_) => vec![], // Ignored
            ExchangeEvents::InsuficientFundsForRecycleFee(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| {
                    StateEvents::order_error(ctx, OrderErrorType::InsuficientFundsForRecycleFee)
                })
                .into_iter()
                .collect(),
            ExchangeEvents::InsurancePaymentForSettlement(_) => vec![], // Ignored
            ExchangeEvents::InvalidAccountFrozenOrder(_) => vec![],     // Ignored
            ExchangeEvents::InvalidBankruptcyPrice(_) => vec![],        // Ignored
            ExchangeEvents::InvalidExpiryBlock(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::InvalidExpiryBlock))
                .into_iter()
                .collect(),
            ExchangeEvents::InvalidLinkReportForContract(_) => vec![], // Ignored
            ExchangeEvents::InvalidLinkReportVersion(_) => vec![],     // Ignored
            ExchangeEvents::InvalidLiquidationPrice(_) => vec![],      // Ignored
            ExchangeEvents::InvalidOrderId(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::InvalidOrderId))
                .into_iter()
                .collect(),
            ExchangeEvents::InvalidSynthPerpPrice(_) => vec![], // Ignored
            ExchangeEvents::LinkDatastreamConfigured(_) => vec![], // Ignored
            ExchangeEvents::LinkDsError_0(_) => vec![],         // Ignored
            ExchangeEvents::LinkDsError_1(_) => vec![],         // Ignored
            ExchangeEvents::LinkDsPanic(_) => vec![],           // Ignored
            ExchangeEvents::LinkPriceUpdated(e) => self
                .perpetual(e.perpId)
                .map(|perp| {
                    perp.update_oracle_price(
                        instant,
                        perp.price_converter().from_unsigned(e.oraclePricePNS),
                    );
                    StateEvents::perpetual(
                        perp,
                        PerpetualEventType::OraclePriceUpdated(perp.oracle_price()),
                    )
                })
                .into_iter()
                .collect(),
            ExchangeEvents::LiquidationBuyerUpdated(_) => vec![], // Ignored
            ExchangeEvents::LiquidationParamsUpdated(_) => vec![], // Ignored
            ExchangeEvents::LotOutOfRange(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::SizeOutOfRange))
                .into_iter()
                .collect(),
            ExchangeEvents::MaintenanceMarginFractionUpdated(e) => self
                .perpetual(e.perpId)
                .map(|perp| {
                    perp.update_maintenance_margin(
                        instant,
                        perp.leverage_converter()
                            .from_unsigned(e.maintMarginFracHdths),
                    );
                    StateEvents::perpetual(
                        perp,
                        PerpetualEventType::MaintenanceMarginFractionUpdated(
                            perp.maintenance_margin(),
                        ),
                    )
                })
                .into_iter()
                .collect(),
            ExchangeEvents::MakerFeeUpdated(e) => self
                .perpetual(e.perpId)
                .map(|perp| {
                    perp.update_maker_fee(
                        instant,
                        perp.fee_converter().from_unsigned(e.makerFeePer100K),
                    );
                    StateEvents::perpetual(
                        perp,
                        PerpetualEventType::MakerFeeUpdated(perp.maker_fee()),
                    )
                })
                .into_iter()
                .collect(),
            ExchangeEvents::MakerOrderFilled(e) => chain!(
                if let Some((perp, order)) = self.order(e.perpId, e.orderId)? {
                    let fill_price = perp.price_converter().from_unsigned(e.pricePNS);
                    let fill_size = perp.size_converter().from_unsigned(e.lotLNS);
                    let fee = cc.from_unsigned(e.feeCNS);

                    perp.update_last_price(instant, fill_price);
                    vec![
                        if order.size() > fill_size {
                            let new_size = order.size() - fill_size;
                            perp.update_order(order.updated(
                                instant,
                                ctx,
                                None,
                                Some(new_size),
                                None,
                            ))
                            .expect("order exists");
                            StateEvents::order(
                                perp,
                                &order,
                                ctx,
                                OrderEventType::Updated {
                                    price: None,
                                    size: Some(new_size),
                                    expiry_block: None,
                                },
                            )
                        } else {
                            perp.remove_order(order.order_id()).expect("order exists");
                            StateEvents::order(perp, &order, ctx, OrderEventType::Removed)
                        },
                        StateEvents::order(
                            perp,
                            &order,
                            ctx,
                            OrderEventType::Filled {
                                fill_price,
                                fill_size,
                                fee,
                                is_maker: true,
                            },
                        ),
                        StateEvents::perpetual(
                            perp,
                            PerpetualEventType::LastPriceUpdated(perp.last_price()),
                        ),
                    ]
                } else {
                    vec![]
                },
                self.account(e.accountId).map(|acc| {
                    acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                    StateEvents::account(
                        acc,
                        ctx,
                        AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                    )
                }),
                self.account(e.accountId).map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                }),
            )
            .collect(),
            ExchangeEvents::MakerOrderSettlementFailed(e) => chain!(
                if let Some(perp) = self.perpetual(e.perpId) {
                    let order = perp.remove_order(e.orderId.to())?;
                    chain!(
                        Some(StateEvents::order(
                            perp,
                            &order,
                            ctx,
                            OrderEventType::Removed
                        )),
                        self.err_ctx(ctx, event)?
                            .map(|ctx| StateEvents::affected_order_error(
                                ctx,
                                &order,
                                OrderErrorType::MakerOrderSettlementFailed
                            ))
                    )
                    .collect()
                } else {
                    vec![]
                },
                self.account(e.accountId).map(|acc| {
                    acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                    StateEvents::account(
                        acc,
                        ctx,
                        AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                    )
                }),
                self.account(e.recyclerAccountId).map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.recyclerBalanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                }),
            )
            .collect(),
            ExchangeEvents::MarkExceedsTol(_) => vec![], // Ignored
            ExchangeEvents::MarkUpdated(e) => {
                let perp_mark = self.perpetual(e.perpId).map(|perp| {
                    perp.update_mark_price(
                        instant,
                        perp.price_converter().from_unsigned(e.pricePNS),
                    );
                    (perp.id(), perp.mark_price())
                });
                if let Some((perp_id, mark_price)) = perp_mark {
                    chain!(
                        Some(StateEvents::Perpetual(PerpetualEvent {
                            perpetual_id: perp_id,
                            r#type: PerpetualEventType::MarkPriceUpdated(mark_price),
                        })),
                        // Applying updated mark to all tracked positions
                        self.accounts.values_mut().filter_map(|acc| {
                            acc.positions_mut().get_mut(&perp_id).map(|pos| {
                                pos.apply_mark_price(instant, mark_price);
                                StateEvents::position(
                                    pos,
                                    &None,
                                    PositionEventType::UnrealizedPnLUpdated {
                                        pnl: pos.pnl(),
                                        delta_pnl: pos.delta_pnl(),
                                        premium_pnl: pos.premium_pnl(),
                                    },
                                )
                            })
                        }),
                    )
                    .collect()
                } else {
                    vec![]
                }
            }
            ExchangeEvents::MaxMatchesReached(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::MaxMatchesReached))
                .into_iter()
                .collect(),
            ExchangeEvents::MaxOpenInterestUpdated(_) => vec![], // Ignored
            ExchangeEvents::MaximumAccountOrders(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::MaximumAccountOrders))
                .into_iter()
                .collect(),
            ExchangeEvents::MinAccountOpenAmountUpdated(_) => vec![], // Ignored
            ExchangeEvents::MinPostUpdated(e) => {
                self.min_post = cc.from_unsigned(e.minPostCNS);
                vec![StateEvents::Exchange(ExchangeEvent::MinPostUpdated(
                    self.min_post,
                ))]
            }
            ExchangeEvents::MinSettleUpdated(e) => {
                self.min_settle = cc.from_unsigned(e.minSettleCNS);
                vec![StateEvents::Exchange(ExchangeEvent::MinSettleUpdated(
                    self.min_settle,
                ))]
            }
            ExchangeEvents::OracleAgeExceedsMax(_) => vec![], // Ignored
            ExchangeEvents::OracleDisabled(_) => vec![],      // Ignored
            ExchangeEvents::OrderBatchCompleted(_) => {
                // Reset context
                ctx.take();
                vec![]
            }
            ExchangeEvents::OrderCancelled(e) => {
                let c = must_ctx()?;
                chain!(
                    if let Some(perp) = self.perpetuals.get_mut(&c.perpetual_id) {
                        let order = perp.remove_order(c.order_id.unwrap_or_default())?;
                        Some(StateEvents::order(
                            perp,
                            &order,
                            ctx,
                            OrderEventType::Removed,
                        ))
                    } else {
                        None
                    },
                    if let Some(acc) = self.accounts.get_mut(&c.account_id) {
                        acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                        acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                        vec![
                            StateEvents::account(
                                acc,
                                ctx,
                                AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                            ),
                            StateEvents::account(
                                acc,
                                ctx,
                                AccountEventType::BalanceUpdated(acc.balance()),
                            ),
                        ]
                    } else {
                        vec![]
                    },
                )
                .collect()
            }
            ExchangeEvents::OrderCancelledByAdmin(e) => chain!(
                self.order(e.perpId, e.orderId)?.map(|(perp, order)| {
                    perp.remove_order(order.order_id()).expect("order exists");
                    StateEvents::order(perp, &order, ctx, OrderEventType::Removed)
                }),
                self.account(e.accountId).map(|acc| {
                    acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                    StateEvents::account(
                        acc,
                        ctx,
                        AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                    )
                }),
            )
            .collect(),
            ExchangeEvents::OrderCancelledByLiquidator(e) => chain!(
                self.order(e.perpId, e.orderId)?.map(|(perp, order)| {
                    perp.remove_order(order.order_id()).expect("order exists");
                    StateEvents::order(perp, &order, ctx, OrderEventType::Removed)
                }),
                self.account(e.accountId).map(|acc| {
                    acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                    StateEvents::account(
                        acc,
                        ctx,
                        AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                    )
                }),
            )
            .collect(),
            ExchangeEvents::OrderChanged(e) => {
                let c = must_ctx()?;
                chain!(
                    if let Some(perp) = self.perpetuals.get_mut(&c.perpetual_id) {
                        let order_id = c.order_id.unwrap_or_default();
                        let order = perp
                            .orders()
                            .get(&order_id)
                            .cloned()
                            .ok_or(DexError::OrderNotFound(perp.id(), order_id))?;
                        let new_price = perp.price_converter().from_unsigned(e.pricePNS);
                        let new_size = perp.size_converter().from_unsigned(e.lotLNS);
                        let new_expiry_block = e.expiryBlock.to();
                        let price_update = if order.price() != new_price {
                            Some(new_price)
                        } else {
                            None
                        };
                        let size_update = if order.size() != new_size {
                            Some(new_size)
                        } else {
                            None
                        };
                        let expiry_block_update = if order.expiry_block() != new_expiry_block {
                            Some(new_expiry_block)
                        } else {
                            None
                        };
                        let updated = order.updated(
                            instant,
                            ctx,
                            price_update,
                            size_update,
                            expiry_block_update,
                        );
                        perp.update_order(updated)?;
                        Some(StateEvents::order(
                            perp,
                            &order,
                            ctx,
                            OrderEventType::Updated {
                                price: price_update,
                                size: size_update,
                                expiry_block: expiry_block_update,
                            },
                        ))
                    } else {
                        None
                    },
                    if let Some(acc) = self.accounts.get_mut(&c.account_id) {
                        acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                        acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                        vec![
                            StateEvents::account(
                                acc,
                                ctx,
                                AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                            ),
                            StateEvents::account(
                                acc,
                                ctx,
                                AccountEventType::BalanceUpdated(acc.balance()),
                            ),
                        ]
                    } else {
                        vec![]
                    },
                )
                .collect()
            }
            ExchangeEvents::OrderDescIdTooLow(_) => vec![], // Ignored
            ExchangeEvents::OrderDoesNotExist(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::OrderDoesNotExist))
                .into_iter()
                .collect(),
            ExchangeEvents::OrderForwardingNotAllowed(_) => vec![], // Ignored
            ExchangeEvents::OrderForwardingUpdated(_) => vec![],    // Ignored
            ExchangeEvents::OrderPlaced(e) => {
                let c = must_ctx()?;
                chain!(
                    if let Some(perp) = self.perpetuals.get_mut(&c.perpetual_id) {
                        let order = Order::placed(
                            instant,
                            c,
                            e.orderId.to(),
                            perp.size_converter().from_unsigned(e.lotLNS),
                            perp.price_converter(),
                            perp.leverage_converter(),
                        );
                        let event = OrderEventType::Placed {
                            r#type: order.r#type(),
                            price: order.price(),
                            size: order.size(),
                            expiry_block: order.expiry_block(),
                            leverage: order.leverage(),
                            post_only: order.post_only().unwrap_or_default(),
                            fill_or_kill: order.fill_or_kill().unwrap_or_default(),
                            immediate_or_cancel: order.immediate_or_cancel().unwrap_or_default(),
                        };
                        perp.add_order(order);
                        Some(StateEvents::order(perp, &order, ctx, event))
                    } else {
                        None
                    },
                    if let Some(acc) = self.accounts.get_mut(&c.account_id) {
                        acc.update_locked_balance(instant, cc.from_unsigned(e.lockedBalanceCNS));
                        acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                        vec![
                            StateEvents::account(
                                acc,
                                ctx,
                                AccountEventType::LockedBalanceUpdated(acc.locked_balance()),
                            ),
                            StateEvents::account(
                                acc,
                                ctx,
                                AccountEventType::BalanceUpdated(acc.balance()),
                            ),
                        ]
                    } else {
                        vec![]
                    },
                )
                .collect()
            }
            ExchangeEvents::OrderPostFailed(e) => self
                .err_ctx(ctx, event)?
                .map(|ctx| {
                    StateEvents::order_error(ctx, OrderErrorType::OrderPostFailed(e.reason.to()))
                })
                .into_iter()
                .collect(),
            ExchangeEvents::OrderRequest(e) => {
                // Store order request context as it is required to handle
                // future events
                ctx.replace(OrderContext::from(e));
                vec![]
            }
            ExchangeEvents::OrderSettlementImpliesInsolvent(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| {
                    StateEvents::order_error(ctx, OrderErrorType::OrderSettlementImpliesInsolvent)
                })
                .into_iter()
                .collect(),
            ExchangeEvents::OrderSizeExceedsAvailableSize(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| {
                    StateEvents::order_error(ctx, OrderErrorType::OrderSizeExceedsAvailableSize)
                })
                .into_iter()
                .collect(),
            ExchangeEvents::OverCollatDescentThreshUpdated(_) => vec![], // Ignored
            ExchangeEvents::OwnershipTransferStarted(_) => vec![],       // Ignored
            ExchangeEvents::OwnershipTransferred(_) => vec![],           // Ignored
            ExchangeEvents::PermissonedCancelParamsUpdated(_) => vec![], // Ignored
            ExchangeEvents::PositionAdministratorUpdated(_) => vec![],   // Ignored
            ExchangeEvents::PositionClosed(e) => {
                if let Some((acc, perp)) = self.account_perpetual(e.accountId, e.perpId) {
                    let pos = acc
                        .positions_mut()
                        .remove(&perp.id())
                        .ok_or(DexError::PositionNotFound(acc.id(), perp.id()))?;
                    chain!(
                        Some(StateEvents::position(
                            &pos,
                            ctx,
                            PositionEventType::Closed {
                                r#type: pos.r#type(),
                                entry_price: pos.entry_price(),
                                exit_price: perp.price_converter().from_unsigned(e.pricePNS),
                                size: pos.size(),
                                delta_pnl: cc.from_signed(e.deltaPnlCNS),
                                premium_pnl: cc.from_signed(e.fundingCNS),
                            }
                        )),
                        if PositionType::from(e.positionType) == PositionType::Long {
                            perp.update_open_interest(instant, pos.size(), UD64::ZERO);
                            Some(StateEvents::perpetual(
                                perp,
                                PerpetualEventType::OpenInterestUpdated(perp.open_interest()),
                            ))
                        } else {
                            None
                        },
                    )
                    .collect()
                } else {
                    vec![]
                }
            }
            ExchangeEvents::PositionDecreased(e) => {
                if let Some((pos, perp)) = self.position(e.accountId, e.perpId)? {
                    let prev_size = pos.size();
                    pos.update_size(instant, perp.size_converter().from_unsigned(e.endLotLNS));
                    pos.update_deposit(instant, cc.from_unsigned(e.endDepositCNS));
                    pos.apply_mark_price(instant, perp.mark_price());
                    pos.update_premium_pnl(
                        instant,
                        pos.premium_pnl().sub(cc.from_signed(e.fundingCNS)),
                    );
                    pos.apply_maintenance_margin(instant, perp.maintenance_margin());
                    chain!(
                        Some(StateEvents::position(
                            pos,
                            ctx,
                            PositionEventType::Decreased {
                                prev_size,
                                new_size: pos.size(),
                                deposit: pos.deposit(),
                                delta_pnl: pos.delta_pnl(),
                                premium_pnl: pos.premium_pnl(),
                            }
                        )),
                        if pos.r#type() == PositionType::Long {
                            perp.update_open_interest(instant, prev_size, pos.size());
                            Some(StateEvents::perpetual(
                                perp,
                                PerpetualEventType::OpenInterestUpdated(perp.open_interest()),
                            ))
                        } else {
                            None
                        },
                    )
                    .collect()
                } else {
                    vec![]
                }
            }
            ExchangeEvents::PositionDeleveraged(e) => chain!(
                if let Some((pos, perp)) = self.position(e.accountId, e.perpId)? {
                    let prev_size = pos.size();
                    pos.update_size(instant, perp.size_converter().from_unsigned(e.endLotLNS));
                    pos.update_deposit(instant, cc.from_unsigned(e.endDepositCNS));
                    pos.apply_mark_price(instant, perp.mark_price());
                    pos.update_premium_pnl(
                        instant,
                        pos.premium_pnl().sub(cc.from_signed(e.fundingCNS)),
                    );
                    pos.apply_maintenance_margin(instant, perp.maintenance_margin());
                    chain!(
                        Some(StateEvents::position(
                            pos,
                            ctx,
                            PositionEventType::Deleveraged {
                                force_close: e.forceClose,
                                r#type: pos.r#type(),
                                entry_price: pos.entry_price(),
                                exit_price: perp
                                    .price_converter()
                                    .from_unsigned(e.deleveragePricePNS),
                                prev_size,
                                new_size: pos.size(),
                                deposit: pos.deposit(),
                                delta_pnl: pos.delta_pnl(),
                                premium_pnl: pos.premium_pnl(),
                            }
                        )),
                        if pos.r#type() == PositionType::Long {
                            perp.update_open_interest(instant, prev_size, pos.size());
                            Some(StateEvents::perpetual(
                                perp,
                                PerpetualEventType::OpenInterestUpdated(perp.open_interest()),
                            ))
                        } else {
                            None
                        },
                    )
                    .collect()
                } else {
                    vec![]
                },
                self.account(e.accountId).map(|acc| {
                    if e.endLotLNS == U256::ZERO {
                        acc.positions_mut()
                            .remove(&e.perpId.to::<types::PerpetualId>());
                    }
                    acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                }),
            )
            .collect(),
            ExchangeEvents::PositionDoesNotExist(_) => vec![], // Ignored
            ExchangeEvents::PositionIncreased(e) => {
                if let Some((pos, perp)) = self.position(e.accountId, e.perpId)? {
                    let prev_size = pos.size();
                    pos.update_entry_price(
                        instant,
                        perp.price_converter().from_unsigned(e.pricePNS),
                    );
                    pos.update_size(instant, perp.size_converter().from_unsigned(e.endLotLNS));
                    pos.update_deposit(instant, cc.from_unsigned(e.endDepositCNS));
                    pos.apply_mark_price(instant, perp.mark_price());
                    pos.update_premium_pnl(instant, D256::ZERO);
                    pos.apply_maintenance_margin(instant, perp.maintenance_margin());

                    chain!(
                        Some(StateEvents::position(
                            pos,
                            ctx,
                            PositionEventType::Increased {
                                entry_price: pos.entry_price(),
                                prev_size,
                                new_size: pos.size(),
                                deposit: pos.deposit(),
                            }
                        )),
                        if pos.r#type() == PositionType::Long {
                            perp.update_open_interest(instant, prev_size, pos.size());
                            Some(StateEvents::perpetual(
                                perp,
                                PerpetualEventType::OpenInterestUpdated(perp.open_interest()),
                            ))
                        } else {
                            None
                        },
                    )
                    .collect()
                } else {
                    vec![]
                }
            }
            ExchangeEvents::PositionInverted(e) => {
                if let Some((pos, perp)) = self.position(e.accountId, e.perpId)? {
                    let prev_type = pos.r#type();
                    let prev_entry_price = pos.entry_price();
                    let prev_size = pos.size();
                    pos.update_type(instant, PositionType::from(e.positionType));
                    pos.update_entry_price(
                        instant,
                        perp.price_converter().from_unsigned(e.pricePNS),
                    );
                    pos.update_size(instant, perp.size_converter().from_unsigned(e.endLotLNS));
                    pos.update_deposit(instant, cc.from_unsigned(e.endDepositCNS));
                    pos.apply_mark_price(instant, perp.mark_price());
                    pos.update_premium_pnl(instant, D256::ZERO);
                    pos.apply_maintenance_margin(instant, perp.maintenance_margin());
                    if pos.r#type() == PositionType::Long {
                        perp.update_open_interest(instant, UD64::ZERO, pos.size());
                    } else {
                        perp.update_open_interest(instant, prev_size, UD64::ZERO);
                    }
                    vec![
                        StateEvents::position(
                            pos,
                            ctx,
                            PositionEventType::Closed {
                                r#type: prev_type,
                                entry_price: prev_entry_price,
                                exit_price: pos.entry_price(),
                                size: prev_size,
                                delta_pnl: cc.from_signed(e.deltaPnlCNS),
                                premium_pnl: cc.from_signed(e.fundingCNS),
                            },
                        ),
                        StateEvents::position(
                            pos,
                            ctx,
                            PositionEventType::Inverted {
                                r#type: pos.r#type(),
                                entry_price: pos.entry_price(),
                                prev_size,
                                new_size: pos.size(),
                                deposit: pos.deposit(),
                                delta_pnl: pos.delta_pnl(),
                                premium_pnl: pos.premium_pnl(),
                            },
                        ),
                        StateEvents::perpetual(
                            perp,
                            PerpetualEventType::OpenInterestUpdated(perp.open_interest()),
                        ),
                    ]
                } else {
                    vec![]
                }
            }
            ExchangeEvents::PositionLiquidated(e) => chain!(
                if let Some((pos, perp)) = self.position(e.posAccountId, e.perpId)? {
                    let prev_size = pos.size();
                    pos.update_size(instant, perp.size_converter().from_unsigned(e.posLotLNS));
                    pos.update_deposit(instant, cc.from_unsigned(e.posDepositCNS));
                    pos.apply_mark_price(instant, perp.mark_price());
                    pos.update_premium_pnl(
                        instant,
                        pos.premium_pnl().sub(cc.from_signed(e.fundingCNS)),
                    );
                    pos.apply_maintenance_margin(instant, perp.maintenance_margin());
                    chain!(
                        Some(StateEvents::position(
                            pos,
                            ctx,
                            PositionEventType::Liquidated {
                                r#type: pos.r#type(),
                                entry_price: pos.entry_price(),
                                exit_price: perp.price_converter().from_unsigned(e.liqPricePNS),
                                prev_size,
                                liquidated_size: perp.size_converter().from_unsigned(e.liqLotLNS),
                                new_size: pos.size(),
                                deposit: pos.deposit(),
                                delta_pnl: pos.delta_pnl(),
                                premium_pnl: pos.premium_pnl(),
                            }
                        )),
                        if pos.r#type() == PositionType::Long {
                            perp.update_open_interest(instant, prev_size, pos.size());
                            Some(StateEvents::perpetual(
                                perp,
                                PerpetualEventType::OpenInterestUpdated(perp.open_interest()),
                            ))
                        } else {
                            None
                        },
                    )
                    .collect()
                } else {
                    vec![]
                },
                self.account(e.posAccountId).map(|acc| {
                    if e.posLotLNS == U256::ZERO {
                        acc.positions_mut()
                            .remove(&e.perpId.to::<types::PerpetualId>());
                    }
                    acc.update_balance(instant, cc.from_unsigned(e.accBalanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                }),
            )
            .collect(),
            ExchangeEvents::PositionLiquidationCredit(e) => self
                .position(e.accountId, e.perpId)?
                .map(|(pos, _)| {
                    pos.update_deposit(instant, cc.from_unsigned(e.endDepositCNS));
                    StateEvents::position(
                        pos,
                        ctx,
                        PositionEventType::DepositUpdated(pos.deposit()),
                    )
                })
                .into_iter()
                .collect(),
            ExchangeEvents::PositionOpened(e) => {
                if let Some((acc, perp)) = self.account_perpetual(e.accountId, e.perpId) {
                    let pos = Position::opened(
                        instant,
                        perp.id(),
                        acc.id(),
                        PositionType::from(e.positionType),
                        perp.price_converter().from_unsigned(e.pricePNS),
                        perp.size_converter().from_unsigned(e.lotLNS),
                        cc.from_unsigned(e.depositCNS),
                        perp.maintenance_margin(),
                    );
                    let events = chain!(
                        Some(StateEvents::position(
                            &pos,
                            ctx,
                            PositionEventType::Opened {
                                r#type: pos.r#type(),
                                entry_price: pos.entry_price(),
                                size: pos.size(),
                                deposit: pos.deposit(),
                            }
                        )),
                        if pos.r#type() == PositionType::Long {
                            perp.update_open_interest(instant, UD64::ZERO, pos.size());
                            Some(StateEvents::perpetual(
                                perp,
                                PerpetualEventType::OpenInterestUpdated(perp.open_interest()),
                            ))
                        } else {
                            None
                        },
                    )
                    .collect();
                    acc.positions_mut().insert(perp.id(), pos);
                    events
                } else {
                    vec![]
                }
            }
            ExchangeEvents::PositionUnwound(e) => {
                if let Some((acc, perp)) = self.account_perpetual(e.accountId, e.perpId) {
                    let pos = acc
                        .positions_mut()
                        .remove(&perp.id())
                        .ok_or(DexError::PositionNotFound(acc.id(), perp.id()))?;
                    acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                    chain!(
                        Some(StateEvents::position(
                            &pos,
                            ctx,
                            PositionEventType::Unwound {
                                r#type: pos.r#type(),
                                entry_price: pos.entry_price(),
                                exit_price: perp.price_converter().from_unsigned(e.pricePNS),
                                size: pos.size(),
                                fair_market_value: cc.from_signed(e.positionFmvCNS),
                                payment: cc.from_unsigned(e.paymentCNS),
                            }
                        )),
                        Some(StateEvents::account(
                            acc,
                            ctx,
                            AccountEventType::BalanceUpdated(acc.balance())
                        )),
                        if pos.r#type() == PositionType::Long {
                            perp.update_open_interest(instant, pos.size(), UD64::ZERO);
                            Some(StateEvents::perpetual(
                                perp,
                                PerpetualEventType::OpenInterestUpdated(perp.open_interest()),
                            ))
                        } else {
                            None
                        },
                    )
                    .collect()
                } else {
                    vec![]
                }
            }
            ExchangeEvents::PositionUnwoundWithoutPayment(e) => {
                if let Some((acc, perp)) = self.account_perpetual(e.accountId, e.perpId) {
                    let pos = acc
                        .positions_mut()
                        .remove(&perp.id())
                        .ok_or(DexError::PositionNotFound(acc.id(), perp.id()))?;
                    chain!(
                        Some(StateEvents::position(
                            &pos,
                            ctx,
                            PositionEventType::Unwound {
                                r#type: pos.r#type(),
                                entry_price: pos.entry_price(),
                                exit_price: perp.price_converter().from_unsigned(e.pricePNS),
                                size: pos.size(),
                                fair_market_value: cc.from_signed(e.positionFmvCNS),
                                payment: UD128::ZERO,
                            }
                        )),
                        if pos.r#type() == PositionType::Long {
                            perp.update_open_interest(instant, pos.size(), UD64::ZERO);
                            Some(StateEvents::perpetual(
                                perp,
                                PerpetualEventType::OpenInterestUpdated(perp.open_interest()),
                            ))
                        } else {
                            None
                        },
                    )
                    .collect()
                } else {
                    vec![]
                }
            }
            ExchangeEvents::PostOrderUnderMinimum(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::PostOrderUnderMinimum))
                .into_iter()
                .collect(),
            ExchangeEvents::PriceAdministratorUpdated(_) => vec![], // Ignored
            ExchangeEvents::PriceMaxAgeUpdated(e) => {
                if let Some(perp) = self.perpetual(e.perpId) {
                    perp.update_price_max_age_sec(instant, e.maxAgeSec.to());
                }
                vec![]
            }
            ExchangeEvents::PriceOutOfRange(_) => self
                .err_ctx(ctx, event)
                .ok() // Used both for orders and mark/oracle prices
                .flatten()
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::PriceOutOfRange))
                .into_iter()
                .collect(),
            ExchangeEvents::PriceTolUpdated(_) => vec![], // Ignored
            ExchangeEvents::ProtocolBalanceDeposit(_) => vec![], // Ignored
            ExchangeEvents::ProtocolBalanceWithdraw(_) => vec![], // Ignored
            ExchangeEvents::RecycleBalanceInsufficientSevere(_) => vec![], // Ignored
            ExchangeEvents::RecycleFeeUpdated(e) => {
                self.recycle_fee = cc.from_unsigned(e.recycleFeeCNS);
                vec![StateEvents::Exchange(ExchangeEvent::RecycleFeeUpdated(
                    self.recycle_fee(),
                ))]
            }
            ExchangeEvents::ReferencePriceAgesExceedMax(_) => vec![], // Ignored
            ExchangeEvents::ReportAgeExceedsLastUpdate(_) => vec![],  // Ignored
            ExchangeEvents::ReportPriceIsNegative(_) => vec![],       // Ignored
            ExchangeEvents::SyntheticPriceError(_) => vec![],         // Ignored
            ExchangeEvents::TakerFeeUpdated(e) => self
                .perpetual(e.perpId)
                .map(|perp| {
                    perp.update_taker_fee(
                        instant,
                        perp.fee_converter().from_unsigned(e.takerFeePer100K),
                    );
                    StateEvents::perpetual(
                        perp,
                        PerpetualEventType::TakerFeeUpdated(perp.taker_fee()),
                    )
                })
                .into_iter()
                .collect(),
            ExchangeEvents::TakerOrderFilled(e) => {
                let c = must_ctx()?;
                chain!(
                    self.perpetuals
                        .get(&c.perpetual_id)
                        .map(|perp| StateEvents::Order(OrderEvent {
                            perpetual_id: perp.id(),
                            account_id: c.account_id,
                            request_id: Some(c.request_id),
                            order_id: None,
                            r#type: OrderEventType::Filled {
                                fill_price: perp.price_converter().from_unsigned(e.pricePNS),
                                fill_size: perp.size_converter().from_unsigned(e.lotLNS),
                                fee: cc.from_unsigned(e.feeCNS),
                                is_maker: false,
                            },
                        })),
                    self.accounts.get_mut(&c.account_id).map(|acc| {
                        acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                        StateEvents::account(
                            acc,
                            ctx,
                            AccountEventType::BalanceUpdated(acc.balance()),
                        )
                    }),
                )
                .collect()
            }
            ExchangeEvents::TransferAccountToProtocol(e) => self
                .account(e.accountId)
                .map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                })
                .into_iter()
                .collect(),
            ExchangeEvents::TransferPerpInsToProtocol(_) => vec![], // Ignored
            ExchangeEvents::TransferProtocolToAccount(e) => self
                .account(e.accountId)
                .map(|acc| {
                    acc.update_balance(instant, cc.from_unsigned(e.balanceCNS));
                    StateEvents::account(acc, ctx, AccountEventType::BalanceUpdated(acc.balance()))
                })
                .into_iter()
                .collect(),
            ExchangeEvents::TransferProtocolToPerp(_) => vec![], // Ignored
            ExchangeEvents::TransferProtocolToRecycleBal(_) => vec![], // Ignored
            ExchangeEvents::UnableToCancelOrder(_) => vec![],    // Ignored
            ExchangeEvents::UnityDescentThreshUpdated(_) => vec![], // Ignored
            ExchangeEvents::UnspecifiedCollateral(_) => vec![],  // Ignored
            ExchangeEvents::UnsupportedOperation(_) => vec![],   // Ignored
            ExchangeEvents::UnwindCompleted(_) => vec![],        // Ignored
            ExchangeEvents::UnwindInitializationCleared(_) => vec![], // Ignored
            ExchangeEvents::UnwindInitialized(_) => vec![],      // Ignored
            ExchangeEvents::UnwindInsufficientBalance(_) => vec![], // Ignored
            ExchangeEvents::UnwindIterationCompleted(_) => vec![], // Ignored
            ExchangeEvents::UpdateOracleFailed(_) => vec![],     // Ignored
            ExchangeEvents::WRLSThousandthsTvlUpdated(_) => vec![], // Ignored
            ExchangeEvents::WithdrawRateLimitReset(_) => vec![], // Ignored
            ExchangeEvents::WrongAccountForOrder(_) => self
                .err_ctx(ctx, event)?
                .map(|ctx| StateEvents::order_error(ctx, OrderErrorType::WrongAccountForOrder))
                .into_iter()
                .collect(),
        })
    }

    fn apply_state_event(
        &mut self,
        instant: types::StateInstant,
        event: &StateEvents,
    ) -> Result<Vec<StateEvents>, DexError> {
        Ok(match event {
            StateEvents::Perpetual(pe) => {
                match pe.r#type {
                    PerpetualEventType::FundingEvent {
                        rate: _,
                        payment_per_unit,
                    } => {
                        // Applying funding to all tracked positions
                        self.accounts
                            .values_mut()
                            .filter_map(|acc| {
                                acc.positions_mut()
                                    .get_mut(&pe.perpetual_id)
                                    .and_then(|pos| {
                                        pos.apply_funding_payment(instant, payment_per_unit).then(
                                            || {
                                                StateEvents::position(
                                                    pos,
                                                    &None,
                                                    PositionEventType::UnrealizedPnLUpdated {
                                                        pnl: pos.pnl(),
                                                        delta_pnl: pos.delta_pnl(),
                                                        premium_pnl: pos.premium_pnl(),
                                                    },
                                                )
                                            },
                                        )
                                    })
                            })
                            .collect()
                    }
                    PerpetualEventType::MaintenanceMarginFractionUpdated(maintenance_margin) => {
                        // Applying new maintenance margin to all tracked positions
                        self.accounts
                            .values_mut()
                            .filter_map(|acc| {
                                acc.positions_mut().get_mut(&pe.perpetual_id).map(|pos| {
                                    pos.apply_maintenance_margin(instant, maintenance_margin);
                                    StateEvents::position(
                                        pos,
                                        &None,
                                        PositionEventType::MaintenanceMarginUpdated(
                                            pos.maintenance_margin_requirement(),
                                        ),
                                    )
                                })
                            })
                            .collect()
                    }
                    _ => vec![],
                }
            }
            _ => vec![],
        })
    }

    fn err_ctx<'c>(
        &self,
        ctx: &'c mut Option<OrderContext>,
        event: &stream::RawEvent,
    ) -> Result<Option<&'c OrderContext>, DexError> {
        let c = ctx.as_ref().ok_or(DexError::OrderContextExpected(
            event.tx_index(),
            event.log_index(),
        ))?;
        Ok(self.accounts.contains_key(&c.account_id).then_some(c))
    }

    fn ensure_account(&mut self, id: U256) {
        let id = id.to::<types::AccountId>();
        if self.track_all_accounts && !self.accounts.contains_key(&id) {
            self.accounts.insert(
                id,
                Account::from_event(types::StateInstant::default(), id, Address::ZERO),
            );
        }
    }

    fn account(&mut self, id: U256) -> Option<&mut Account> {
        self.ensure_account(id);
        self.accounts.get_mut(&id.to::<types::AccountId>())
    }

    fn order(
        &mut self,
        perp_id: U256,
        ord_id: U256,
    ) -> Result<Option<(&mut Perpetual, Order)>, DexError> {
        let ord_id = ord_id.to::<types::OrderId>();
        Ok(
            if let Some(perp) = self.perpetuals.get_mut(&perp_id.to::<types::PerpetualId>()) {
                let ord = perp
                    .orders()
                    .get(&ord_id)
                    .cloned()
                    .ok_or(DexError::OrderNotFound(perp.id(), ord_id))?;
                Some((perp, ord))
            } else {
                None
            },
        )
    }

    fn perpetual(&mut self, id: U256) -> Option<&mut Perpetual> {
        self.perpetuals.get_mut(&id.to::<types::PerpetualId>())
    }

    fn account_perpetual(
        &mut self,
        acc_id: U256,
        perp_id: U256,
    ) -> Option<(&mut Account, &mut Perpetual)> {
        self.ensure_account(acc_id);
        self.accounts
            .get_mut(&acc_id.to::<types::AccountId>())
            .zip(self.perpetuals.get_mut(&perp_id.to::<types::PerpetualId>()))
    }

    fn position(
        &mut self,
        acc_id: U256,
        perp_id: U256,
    ) -> Result<Option<(&mut Position, &mut Perpetual)>, DexError> {
        self.ensure_account(acc_id);
        let acc_id = acc_id.to::<types::AccountId>();
        let perp_id = perp_id.to::<types::PerpetualId>();
        Ok(if let Some(acc) = self.accounts.get_mut(&acc_id) {
            let pos = acc
                .positions_mut()
                .get_mut(&perp_id)
                .ok_or(DexError::PositionNotFound(acc_id, perp_id))?;
            Some((
                pos,
                self.perpetuals.get_mut(&perp_id).expect("perpetual found"),
            ))
        } else {
            None
        })
    }
}

//! `Exchange::apply_events_at`: committed state versus speculative overlay.

use std::collections::HashMap;

use alloy::primitives::{Address, B256, TxHash, U256};
use fastnum::{UD64, udec128};

use crate::{
    Chain,
    abi::dex::Exchange::{AccountCreated, ExchangeEvents},
    error::DexError,
    num::Converter,
    state::{ContractFeatures, Exchange, FeeSchedule, FeeScheduleKey, FeeScheduleRegistry},
    stream::{RawBlockEvents, RawEvent},
    types::{CommitState, StateInstant},
};

fn exchange() -> Exchange {
    Exchange::new(
        Chain::testnet(),
        StateInstant::new(10, 10),
        ContractFeatures::current(),
        Converter::new(4),
        100,
        udec128!(0.001),
        udec128!(0.001),
        udec128!(0.001),
        FeeScheduleRegistry::new(
            FeeSchedule::flat(FeeScheduleKey::Default, UD64::ZERO, UD64::ZERO),
            FeeSchedule::flat(FeeScheduleKey::RwaDefault, UD64::ZERO, UD64::ZERO),
            HashMap::new(),
        ),
        HashMap::new(),
        HashMap::new(),
        false,
        true,
    )
}

fn account_created(id: u64) -> RawEvent {
    RawEvent::new(
        TxHash::ZERO,
        0,
        0,
        ExchangeEvents::AccountCreated(AccountCreated {
            id: U256::from(id),
            account: Address::repeat_byte(id as u8),
        }),
    )
}

fn block(height: u64, hash: u8, state: CommitState, account: u64) -> RawBlockEvents {
    RawBlockEvents::new(StateInstant::new(height, height), vec![account_created(account)])
        .with_commit_state(state)
        .with_block_hash(B256::repeat_byte(hash))
}

#[test]
fn speculative_blocks_build_the_overlay_without_touching_committed_state() {
    let mut ex = exchange();
    let out = ex
        .apply_events_at(&block(11, 1, CommitState::Proposed, 1), CommitState::Finalized)
        .unwrap()
        .expect("state events");
    assert_eq!(out.commit_state(), CommitState::Proposed);
    assert_eq!(out.block_hash(), Some(B256::repeat_byte(1)));
    assert!(ex.accounts().is_empty());
    assert_eq!(ex.instant().block_number(), 10);
    let overlay = ex.speculative().expect("overlay");
    assert_eq!(overlay.instant().block_number(), 11);
    assert!(overlay.accounts().contains_key(&1));
    assert_eq!(ex.speculative_instant().block_number(), 11);

    // Voted re-send of the same block: still below min, ignored.
    assert!(
        ex.apply_events_at(&block(11, 1, CommitState::Voted, 1), CommitState::Finalized)
            .unwrap()
            .is_none()
    );

    // Finalized: commits, the overlay (now empty) is dropped.
    let out = ex
        .apply_events_at(&block(11, 1, CommitState::Finalized, 1), CommitState::Finalized)
        .unwrap()
        .expect("state events");
    assert_eq!(out.commit_state(), CommitState::Finalized);
    assert!(ex.accounts().contains_key(&1));
    assert_eq!(ex.block_hash(), Some(B256::repeat_byte(1)));
    assert!(ex.speculative().is_none());
    assert!(std::ptr::eq(ex.latest(), &ex));
}

#[test]
fn committing_keeps_speculative_blocks_above_and_rebuilds_overlay() {
    let mut ex = exchange();
    ex.apply_events_at(&block(11, 1, CommitState::Proposed, 1), CommitState::Finalized)
        .unwrap();
    ex.apply_events_at(&block(12, 2, CommitState::Proposed, 2), CommitState::Finalized)
        .unwrap();
    ex.apply_events_at(&block(11, 1, CommitState::Finalized, 1), CommitState::Finalized)
        .unwrap();
    assert_eq!(ex.instant().block_number(), 11);
    assert!(ex.accounts().contains_key(&1));
    assert!(!ex.accounts().contains_key(&2));
    // Overlay rebuilt lazily on the next speculative block.
    assert!(ex.speculative().is_none());
    ex.apply_events_at(&block(13, 3, CommitState::Proposed, 3), CommitState::Finalized)
        .unwrap();
    let overlay = ex.speculative().unwrap();
    assert_eq!(overlay.instant().block_number(), 13);
    assert!(overlay.accounts().contains_key(&2));
    assert!(overlay.accounts().contains_key(&3));
}

#[test]
fn superseding_proposal_rewinds_the_overlay() {
    let mut ex = exchange();
    ex.apply_events_at(&block(11, 1, CommitState::Proposed, 1), CommitState::Finalized)
        .unwrap();
    ex.apply_events_at(&block(12, 2, CommitState::Proposed, 2), CommitState::Finalized)
        .unwrap();
    let out = ex
        .apply_events_at(
            &block(11, 9, CommitState::Proposed, 9).superseding(true),
            CommitState::Finalized,
        )
        .unwrap()
        .unwrap();
    assert!(out.supersedes());
    let overlay = ex.speculative().unwrap();
    assert_eq!(overlay.instant().block_number(), 11);
    assert!(overlay.accounts().contains_key(&9));
    assert!(!overlay.accounts().contains_key(&1));
    assert!(!overlay.accounts().contains_key(&2));
    // The finalized block differing from the speculative one is a rewind too.
    ex.apply_events_at(
        &block(11, 9, CommitState::Finalized, 9).superseding(true),
        CommitState::Finalized,
    )
    .unwrap();
    assert!(ex.accounts().contains_key(&9));
    assert!(ex.speculative().is_none());
}

#[test]
fn committed_hash_mismatch_is_an_error_and_resends_are_ignored() {
    let mut ex = exchange();
    ex.apply_events_at(&block(11, 1, CommitState::Voted, 1), CommitState::Voted)
        .unwrap();
    assert!(
        ex.apply_events_at(&block(11, 1, CommitState::Finalized, 1), CommitState::Voted)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        ex.apply_events_at(&block(11, 2, CommitState::Finalized, 1), CommitState::Voted),
        Err(DexError::CommittedBlockMismatch(11))
    ));
    assert!(matches!(
        ex.apply_events_at(&block(13, 3, CommitState::Finalized, 3), CommitState::Voted),
        Err(DexError::BlockOutOfOrder(12, 13))
    ));
    assert!(matches!(
        ex.apply_events_at(&block(13, 3, CommitState::Proposed, 3), CommitState::Finalized),
        Err(DexError::BlockOutOfOrder(12, 13))
    ));
}

#[test]
fn legacy_apply_events_commits_everything() {
    let mut ex = exchange();
    let out = ex
        .apply_events(&block(11, 1, CommitState::Proposed, 1))
        .unwrap()
        .unwrap();
    assert_eq!(out.commit_state(), CommitState::Proposed);
    assert!(ex.accounts().contains_key(&1));
    assert!(ex.speculative().is_none());
    // Hand-built batches default to Finalized with no hash: no mismatch.
    let plain = RawBlockEvents::new(StateInstant::new(11, 11), vec![]);
    assert!(ex.apply_events(&plain).unwrap().is_none());
}

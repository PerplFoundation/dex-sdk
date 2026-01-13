use std::{
    collections::HashSet,
    pin::pin,
    sync::{Arc, RwLock, RwLockReadGuard},
    time::Duration,
};

use alloy::providers::DynProvider;
use futures::{
    SinkExt, StreamExt,
    channel::mpsc::{self, UnboundedReceiver, UnboundedSender},
};

use super::TestExchange;
use crate::{Chain, state, stream, types};
pub struct Indexer {
    chain: Chain,
    provider: DynProvider,
    snapshot: Arc<RwLock<state::Exchange>>,
    raw_events_tx: UnboundedSender<stream::RawBlockEvents>,
    state_events_tx: UnboundedSender<state::StateBlockEvents>,
}

pub struct IndexedState {
    snapshot: Arc<RwLock<state::Exchange>>,
    raw_events_rx: UnboundedReceiver<stream::RawBlockEvents>,
    state_events_rx: UnboundedReceiver<state::StateBlockEvents>,
    request_ids: HashSet<u64>,
}

impl Indexer {
    pub async fn new(exchange: &TestExchange) -> (Self, IndexedState) {
        let snapshot = Arc::new(RwLock::new(
            state::SnapshotBuilder::new(&exchange.chain(), exchange.provider.clone())
                .with_accounts(
                    exchange
                        .account_address
                        .iter()
                        .map(|e| types::AccountAddressOrID::ID(*e.key()))
                        .collect(),
                )
                .build()
                .await
                .unwrap(),
        ));

        let (raw_events_tx, raw_events_rx) = mpsc::unbounded();
        let (state_events_tx, state_events_rx) = mpsc::unbounded();

        (
            Self {
                chain: exchange.chain().clone(),
                provider: exchange.provider.clone(),
                snapshot: snapshot.clone(),
                raw_events_tx,
                state_events_tx,
            },
            IndexedState { snapshot, raw_events_rx, state_events_rx, request_ids: HashSet::new() },
        )
    }

    pub async fn run<S, SFut>(mut self, sleep: S)
    where
        S: Fn(Duration) -> SFut + Copy,
        SFut: Future<Output = ()>,
    {
        let mut stream = pin!(stream::raw(
            &self.chain,
            self.provider,
            self.snapshot.read().unwrap().instant(),
            sleep,
        ));
        while let Some(batch) = stream.next().await {
            let batch = batch.unwrap();
            let res = self.snapshot.write().unwrap().apply_events(&batch);
            if self.raw_events_tx.send(batch).await.is_err() {
                break;
            };
            match res {
                Ok(Some(result)) => {
                    if self.state_events_tx.send(result).await.is_err() {
                        break;
                    }
                },
                Ok(None) => (),
                Err(err) => {
                    println!("failed to apply_events: {:#?}", err);
                    break;
                },
            }
        }
    }
}

impl<'a> IndexedState {
    /// Current state snapshot
    pub fn snapshot(&'a self) -> RwLockReadGuard<'a, state::Exchange> {
        self.snapshot.read().unwrap()
    }

    /// Next available batch of raw events
    pub async fn next_raw_events(&mut self) -> Option<stream::RawBlockEvents> {
        self.raw_events_rx.next().await
    }

    /// Next available batch of state events
    pub async fn next_state_events(&mut self) -> Option<state::StateBlockEvents> {
        let batch = self.state_events_rx.next().await;
        if let Some(be) = &batch {
            be.events().iter().for_each(|ec| {
                ec.event().iter().for_each(|e| {
                    if let Some(oe) = e.as_order_event()
                        && let Some(rid) = oe.request_id
                    {
                        self.request_ids.insert(rid);
                    }
                });
            });
        }
        batch
    }

    /// Checks if particular request ID has been seen in consumed state events
    pub fn request_id_seen(&self, request_id: u64) -> bool {
        self.request_ids.contains(&request_id)
    }

    /// Waits for specific block or order request being applied to state
    /// snapshot, skipping all previous state event batches
    pub async fn wait_for(&mut self, block_num: Option<u64>, request_id: Option<u64>) -> bool {
        while let Some(be) = self.state_events_rx.next().await {
            if block_num.is_some_and(|bn| be.instant().block_number() == bn)
                || request_id.is_some_and(|rid| {
                    be.events().iter().any(|ec| {
                        ec.event().iter().any(|e| {
                            e.as_order_event()
                                .is_some_and(|oe| oe.request_id.unwrap_or_default() == rid)
                        })
                    })
                })
            {
                return true;
            }
        }
        false
    }
}

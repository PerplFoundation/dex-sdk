use std::collections::BTreeMap;

use alloy::primitives::B256;
use futures::{Stream, StreamExt, stream};

use crate::{error::DexError, stream::RawBlockEvents, types::CommitState};

/// Only the `Finalized` batches of a multi-level stream: exactly one per
/// height, strictly sequential, never superseded. The drop-in for consumers
/// that must never see a revertible block.
pub fn finalized(
    events: impl Stream<Item = Result<RawBlockEvents, DexError>>,
) -> impl Stream<Item = Result<RawBlockEvents, DexError>> {
    events.filter(|item| {
        let keep = item
            .as_ref()
            .map(|b| b.commit_state() >= CommitState::Finalized)
            .unwrap_or(true);
        async move { keep }
    })
}

/// The first batch per height of a multi-level stream, whatever its commit
/// state: exactly one per height, sequential, the same cadence as
/// [`crate::stream::raw`]. Yields [`DexError::BlockSuperseded`] if a later
/// batch supersedes an already delivered height, which only a `Proposed`
/// source can do; the error ends the stream, as the consumer's state is then
/// built on an abandoned proposal.
pub fn once(
    events: impl Stream<Item = Result<RawBlockEvents, DexError>>,
) -> impl Stream<Item = Result<RawBlockEvents, DexError>> {
    struct State {
        next: Option<u64>,
        /// Hash delivered per height, until finalized.
        delivered: BTreeMap<u64, Option<B256>>,
        done: bool,
    }
    let state = State { next: None, delivered: BTreeMap::new(), done: false };
    stream::unfold((Box::pin(events), state), |(mut events, mut st)| async move {
        loop {
            if st.done {
                return None;
            }
            let item = events.next().await?;
            let block = match item {
                Ok(block) => block,
                Err(err) => {
                    st.done = true;
                    return Some((Err(err), (events, st)));
                },
            };
            let height = block.instant().block_number();
            let next = *st.next.get_or_insert(height);
            if height < next {
                // Re-send of a delivered height.
                let same = st
                    .delivered
                    .get(&height)
                    .is_none_or(|h| h.is_none() || *h == block.block_hash());
                if block.supersedes() || !same {
                    st.done = true;
                    return Some((Err(DexError::BlockSuperseded(height)), (events, st)));
                }
                if block.commit_state() >= CommitState::Finalized {
                    st.delivered = st.delivered.split_off(&(height + 1));
                }
                continue;
            }
            if height > next {
                st.done = true;
                return Some((Err(DexError::BlockOutOfOrder(next, height)), (events, st)));
            }
            if block.supersedes() {
                st.done = true;
                return Some((Err(DexError::BlockSuperseded(height)), (events, st)));
            }
            st.next = Some(height + 1);
            if block.commit_state() >= CommitState::Finalized {
                st.delivered = st.delivered.split_off(&(height + 1));
            } else {
                st.delivered.insert(height, block.block_hash());
            }
            return Some((Ok(block), (events, st)));
        }
    })
}

#[cfg(test)]
mod tests {
    use alloy::primitives::B256;
    use futures::{StreamExt, stream};

    use super::*;
    use crate::types::StateInstant;

    fn batch(height: u64, state: CommitState, hash: u8, supersedes: bool) -> RawBlockEvents {
        RawBlockEvents::new(StateInstant::new(height, 0), vec![])
            .with_commit_state(state)
            .with_block_hash(B256::repeat_byte(hash))
            .superseding(supersedes)
    }

    #[tokio::test]
    async fn once_delivers_first_batch_per_height_and_errors_on_supersede() {
        let input = vec![
            Ok(batch(1, CommitState::Proposed, 1, false)),
            Ok(batch(2, CommitState::Proposed, 2, false)),
            Ok(batch(1, CommitState::Finalized, 1, false)),
            Ok(batch(2, CommitState::Finalized, 2, false)),
            Ok(batch(3, CommitState::Proposed, 3, false)),
            Ok(batch(3, CommitState::Proposed, 4, true)),
            Ok(batch(4, CommitState::Proposed, 5, false)),
        ];
        let out: Vec<_> = once(stream::iter(input)).collect().await;
        let heights: Vec<_> = out
            .iter()
            .map(|r| {
                r.as_ref()
                    .map(|b| b.block_number())
                    .map_err(|e| e.to_string())
            })
            .collect();
        assert_eq!(
            heights,
            vec![Ok(1), Ok(2), Ok(3), Err(DexError::BlockSuperseded(3).to_string())]
        );
    }

    #[tokio::test]
    async fn finalized_keeps_only_finalized() {
        let input = vec![
            Ok(batch(1, CommitState::Proposed, 1, false)),
            Ok(batch(1, CommitState::Voted, 1, false)),
            Ok(batch(1, CommitState::Finalized, 1, false)),
            Ok(batch(2, CommitState::Finalized, 2, true)),
        ];
        let out: Vec<_> = finalized(stream::iter(input)).collect().await;
        let got: Vec<_> = out
            .iter()
            .map(|r| (r.as_ref().unwrap().block_number(), r.as_ref().unwrap().commit_state()))
            .collect();
        assert_eq!(got, vec![(1, CommitState::Finalized), (2, CommitState::Finalized)]);
    }
}

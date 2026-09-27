//! Read one replaceable or addressable event without waiting for the
//! slowest relay (AGENTS.md, "Working with relays", rule 3).
//!
//! `fetch_events` returns only once every relay has sent EOSE, so one relay
//! sitting on a REQ holds the answer back for the whole timeout. A single
//! replaceable event needs no such quorum: the first copy plus a short grace
//! for a newer one is enough. Ported from the Mostro app
//! (appv2 `rust/src/nostr/first_answer.rs`).

use std::cmp::Reverse;
use std::time::Duration;

use futures_util::Stream;
use nostr_sdk::prelude::*;

use crate::error::{Error, Result};

/// How long to keep listening after the first copy, in case that relay held a
/// stale one. Relays that answer at all do so within moments of each other.
pub const GRACE: Duration = Duration::from_millis(750);

/// The newest copy of the event matching `filter`: the first answer from any
/// relay plus whatever arrives within `GRACE`, ranked by NIP-01 (later
/// `created_at`, then lowest id). `None` if no relay answers within
/// `timeout`. Dropping the stream closes the request on silent relays.
pub async fn newest_event(
    client: &Client,
    filter: Filter,
    timeout: Duration,
) -> Result<Option<Event>> {
    let stream = client
        .stream_events(filter)
        .timeout(timeout)
        .await
        .map_err(|e| Error::Nostr(format!("cannot request event: {e}")))?;
    let events = stream.filter_map(|(relay, item)| async move {
        item.inspect_err(|e| tracing::debug!(%relay, error = %e, "relay answer failed"))
            .ok()
    });
    Ok(newest_answer(Box::pin(events), GRACE, rank).await)
}

/// NIP-01 rank of a replaceable event: newer first, then the lowest id.
pub fn rank(event: &Event) -> (Timestamp, Reverse<EventId>) {
    (event.created_at, Reverse(event.id))
}

/// The best of `answers` by `rank`: the first one, plus whatever else
/// arrives within `grace` of it. `None` when the stream ends empty.
pub async fn newest_answer<T, K: Ord>(
    mut answers: impl Stream<Item = T> + Unpin,
    grace: Duration,
    rank: impl Fn(&T) -> K,
) -> Option<T> {
    let mut newest = answers.next().await?;
    // Elapsing is the expected way out: it means a relay is still silent.
    let _ = tokio::time::timeout(grace, async {
        while let Some(answer) = answers.next().await {
            if rank(&answer) > rank(&newest) {
                newest = answer;
            }
        }
    })
    .await;
    Some(newest)
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use futures_util::stream;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn returns_after_the_grace_while_a_source_stays_silent() {
        let answers = stream::iter([1]).chain(stream::once(pending::<i32>()));

        let best = newest_answer(Box::pin(answers), GRACE, |n| *n).await;

        assert_eq!(best, Some(1));
    }

    #[tokio::test(start_paused = true)]
    async fn a_better_answer_within_the_grace_wins() {
        let answers = stream::iter([1, 3, 2]);

        assert_eq!(newest_answer(answers, GRACE, |n| *n).await, Some(3));
    }

    #[tokio::test(start_paused = true)]
    async fn no_answer_is_none() {
        let answers = stream::iter(Vec::<i32>::new());

        assert_eq!(newest_answer(answers, GRACE, |n| *n).await, None);
    }
}

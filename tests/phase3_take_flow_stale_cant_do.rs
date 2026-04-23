//! Cross-dispute isolation of `run_take_flow`'s refusal path.
//!
//! `run_take_flow` subscribes to `kind=1059, #p=serbero` with a
//! 7-day `since` window (wide enough to tolerate NIP-59's ±2-day
//! random timestamp tweak). The relay replays stale traffic from
//! prior take-flows on every subscribe, including `CantDo` refusals
//! Mostro emitted for earlier disputes. `mostro_core::nip59::
//! validate_response` short-circuits any `CantDo` as an error, so
//! surfacing it unconditionally would let a stale refusal for
//! dispute A abort a brand-new take-flow for dispute B — a
//! cross-dispute false failure.
//!
//! This test pins the correlation gate: the take-flow only honors
//! refusals whose `MessageKind::id` equals the dispute it is
//! currently opening; stale refusals for other disputes are dropped
//! at the per-event loop and the take-flow proceeds to its normal
//! `AdminTookDispute` round-trip.

mod common;

use std::time::Duration;

use common::MostroChatSim;
use mostro_core::error::CantDoReason;
use mostro_core::message::{Message, Payload};
use mostro_core::nip59::{wrap_message, WrapOptions};
use nostr_relay_builder::MockRelay;
use nostr_sdk::prelude::*;
use uuid::Uuid;

use serbero::chat::dispute_chat_flow::{run_take_flow, TakeFlowParams};

#[tokio::test]
async fn stale_cant_do_for_other_dispute_does_not_abort_take_flow() {
    let relay = MockRelay::run().await.expect("start mock relay");
    let relay_url = relay.url().await.to_string();

    let serbero_keys = Keys::generate();
    let buyer_trade = Keys::generate();
    let seller_trade = Keys::generate();

    // MostroChatSim brings up the happy-path reply for the *current*
    // dispute. We pre-publish a stale `CantDo` addressed to serbero
    // from the same Mostro identity, for a DIFFERENT dispute id —
    // exactly the cross-dispute shape the finding calls out.
    let sim = MostroChatSim::start(
        &relay_url,
        buyer_trade.public_key(),
        seller_trade.public_key(),
    )
    .await;

    // Pre-publish the stale CantDo. The sim's own Mostro keys sign
    // the inner tuple so `unwrapped.sender == mostro_pubkey` still
    // holds inside the take-flow — only the dispute-id correlation
    // stands between this stale refusal and a false-failure abort.
    let stale_pub = Client::new(sim.keys());
    stale_pub.add_relay(&relay_url).await.unwrap();
    stale_pub.connect().await;
    stale_pub.wait_for_connection(Duration::from_secs(5)).await;
    let stale_dispute_id = Uuid::new_v4();
    let stale_msg = Message::cant_do(
        Some(stale_dispute_id),
        None,
        Some(Payload::CantDo(Some(CantDoReason::NotAuthorized))),
    );
    let stale_wrap = wrap_message(
        &stale_msg,
        &sim.keys(),
        serbero_keys.public_key(),
        WrapOptions::default(),
    )
    .await
    .expect("wrap stale CantDo");
    stale_pub
        .send_event(&stale_wrap)
        .await
        .expect("publish stale CantDo");

    // Serbero's nostr client. Wait for the relay handshake so the
    // take-flow's REQ lands before `send_event` below could race.
    let serbero_client = Client::new(serbero_keys.clone());
    serbero_client.add_relay(&relay_url).await.unwrap();
    serbero_client.connect().await;
    serbero_client
        .wait_for_connection(Duration::from_secs(5))
        .await;

    // Open a take-flow for a *new* dispute, different from
    // `stale_dispute_id`. The stale refusal is visible on the
    // relay's history replay; the fix keeps it from aborting the
    // round-trip.
    let current_dispute_id = Uuid::new_v4();
    let material = run_take_flow(TakeFlowParams {
        client: &serbero_client,
        serbero_keys: &serbero_keys,
        mostro_pubkey: &sim.pubkey(),
        dispute_id: current_dispute_id,
        timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(200),
    })
    .await
    .expect(
        "take-flow must succeed despite a stale CantDo for an unrelated dispute \
         still on the relay within the 7-day since window",
    );

    // Sanity: the returned material's trade pubkeys match the
    // ones MostroChatSim wired into the canned AdminTookDispute
    // reply — proof the round-trip went through end-to-end and did
    // not just "not abort".
    assert_eq!(material.buyer_pubkey, buyer_trade.public_key().to_hex());
    assert_eq!(material.seller_pubkey, seller_trade.public_key().to_hex());
}

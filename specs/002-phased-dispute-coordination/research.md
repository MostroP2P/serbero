# Research: Phased Dispute Coordination

**Date**: 2026-04-16
**Spec**: [spec.md](spec.md)

## R-001: nostr-sdk v0.44.1 Relay Subscription and Event Handling

**Decision**: Use `Client` with a filter that targets the dispute
event kind (`Kind::Custom(38386)`) plus custom single-letter tag
filters for `#z`, `#s`, and `#y`. Drive the event loop with the
client's notification-handling API (expected to be
`handle_notifications` or its current equivalent).

**Rationale**: nostr-sdk is expected to provide built-in relay
reconnection with backoff and a canonical blocking notification loop
at v0.44.1. Custom single-letter tag filters are expected to be
expressible through the crate's filter builder.

**Verification points** (resolve before or during Phase 1
implementation — do not assume settled):
- Confirm the exact method name and signature used to build custom
  single-letter tag filters (e.g. `Filter::custom_tag` vs. a builder
  variant) in nostr-sdk 0.44.1.
- Confirm that `handle_notifications` (or the current equivalent) is
  present in 0.44.1 with the expected closure-based callback shape.
- Confirm the exact enum path for incoming events (e.g.
  `RelayPoolNotification::Event`) and the disconnection status
  variant that triggers automatic retry.
- If any of these have shifted in 0.44.1, adjust this decision rather
  than forcing the code to match this document.

**Alternatives considered**:
- Manual WebSocket management: rejected — nostr-sdk handles
  reconnection, message parsing, and subscription management.
- nostr-rs-relay: not a client library.

**Candidate types** (subject to verification above):
- `Client`, `Filter`, `Kind::Custom(38386)`
- A single-letter tag filter mechanism for `#z`, `#s`, `#y`
- A relay-pool notification enum with an `Event` variant
- A relay status enum whose "disconnected" variant triggers automatic
  retry

## R-002: Gift-Wrap (NIP-59) for Solver Notifications

**Decision**: For Phase 1 solver notifications, use the highest-level
nostr-sdk helper that sends an encrypted private message to a given
recipient pubkey and produces NIP-59 gift-wrapped output (NIP-17 style
private DMs). In nostr-sdk 0.44.1 this is expected to be a
`send_private_msg`-style client method.

**Rationale**: A high-level private-message helper is expected to
handle NIP-59 gift-wrap construction (ephemeral keys, seal and wrap
layers) internally, which is sufficient for Phase 1's plain-text
notification payloads. Later phases that need structured rumor
content can drop down to a lower-level gift-wrap builder.

**Verification points** (resolve before Phase 1 notifier
implementation — do not assume settled):
- Confirm the exact name, signature, and parameter order of the
  private-message helper in nostr-sdk 0.44.1 (historically
  `send_private_msg(receiver, message, extra_tags)`, but this must be
  re-checked against the released crate).
- Confirm whether the helper internally selects NIP-17 over NIP-59
  gift-wrap in the expected way, and whether any additional
  configuration (e.g., relay hints, expiration tags) is required for
  delivery to solvers.
- Confirm the lower-level gift-wrap entry point (previously
  `client.gift_wrap` / `EventBuilder::gift_wrap`) that later phases
  can target if richer rumor content is required.
- If the helper has been renamed or restructured in 0.44.1, update
  this decision and the notifier plan accordingly rather than forcing
  a match to historical names.

**Alternatives considered**:
- Raw gift-wrap construction via `EventBuilder`: more control but
  unnecessary for Phase 1 text notifications; revisit for Phase 3+.
- Unencrypted DMs (NIP-04): rejected — spec requires gift-wrap.

## R-003: mostro-core Crate for Protocol Types

**Decision**: Use `mostro-core = "0.8.4"` for dispute types and
protocol constants.

**Rationale**: The crate provides `NOSTR_DISPUTE_EVENT_KIND = 38386`,
`DisputeStatus` enum (`Initiated`, `InProgress`, etc.), `Dispute`
struct, and `Action` enum variants (`Dispute`, `AdminTookDispute`,
etc.). Using the official crate ensures protocol compatibility.

**Key types**:
- `mostro_core::prelude::NOSTR_DISPUTE_EVENT_KIND` (38386)
- `mostro_core::dispute::DisputeStatus` (`Initiated`, `InProgress`)
- `mostro_core::message::Action` (`Dispute`, `AdminTookDispute`)

**Alternatives considered**:
- Hardcoded constants: rejected — fragile and diverges from protocol.

## R-004: SQLite Direct Access

**Decision**: Use `rusqlite` crate for direct SQLite access. No ORM,
no abstraction layer.

**Rationale**: The spec requires SQLite with no storage abstraction
(TC-003). `rusqlite` is the standard Rust SQLite binding, mature and
well-maintained. For the small schema (disputes, notifications, audit),
direct SQL is straightforward. Connection pooling via `r2d2` is not
needed for a single-instance daemon.

**Alternatives considered**:
- sqlx: async but heavier; adds compile-time checking overhead that
  is unnecessary for a small fixed schema.
- diesel: ORM layer adds abstraction the spec prohibits.
- r2d2 pool: not needed for single-threaded SQLite access from one
  daemon instance.

## R-005: Daemon Process Model

**Decision**: Serbero runs as a single-process async Rust daemon using
`tokio` as the async runtime (required by nostr-sdk).

**Rationale**: nostr-sdk uses tokio internally. The daemon model is:
startup (config load, SQLite init, relay connect) then enter the
`handle_notifications` loop. Graceful shutdown via tokio signal
handling (SIGTERM/SIGINT).

**Alternatives considered**:
- Multi-process: rejected — single instance per spec assumption.
- Non-async: rejected — nostr-sdk requires async runtime.

## R-006: Configuration Format

**Decision**: Use a TOML configuration file for relay URLs, Mostro
instance pubkey, solver pubkeys, and operational parameters. Parse
with the `toml` and `serde` crates.

**Rationale**: TOML is standard for Rust projects (Cargo uses it).
The configuration surface is small: relay list, Mostro pubkey, solver
list, timeouts. Environment variable overrides for secrets (Serbero's
private key).

**Alternatives considered**:
- YAML: less idiomatic for Rust.
- Environment-only: insufficient for solver list configuration.
- JSON: less readable for human-edited config.

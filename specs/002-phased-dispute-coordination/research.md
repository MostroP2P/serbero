# Research: Phased Dispute Coordination

**Date**: 2026-04-16
**Spec**: [spec.md](spec.md)

## R-001: nostr-sdk v0.44.1 Relay Subscription and Event Handling

**Decision**: Use `Client` with `Filter::new().kind(Kind::Custom(38386))`
and custom tag filters via `SingleLetterTag::lowercase(Alphabet::Z)` etc.
Use `client.handle_notifications()` as the main event loop.

**Rationale**: nostr-sdk v0.44.1 provides built-in relay reconnection
with backoff (automatic by default via `RelayOptions`). The
`handle_notifications` pattern is the canonical blocking loop. Custom
tag filters (`#z`, `#s`, `#y`) are supported via
`Filter::custom_tag(SingleLetterTag, value)`.

**Alternatives considered**:
- Manual WebSocket management: rejected — nostr-sdk handles
  reconnection, message parsing, and subscription management.
- nostr-rs-relay: not a client library.

**Key types**:
- `Client`, `Filter`, `Kind::Custom(38386)`
- `SingleLetterTag::lowercase(Alphabet::Z)` for `#z` tag filter
- `RelayPoolNotification::Event` for incoming events
- `RelayStatus::Disconnected` triggers automatic retry

## R-002: Gift-Wrap (NIP-59) for Solver Notifications

**Decision**: Use `client.send_private_msg(receiver, message, extra_tags)`
for Phase 1 notifications. This uses NIP-17 which wraps messages in
gift wraps internally.

**Rationale**: `send_private_msg` is the high-level API that handles
NIP-59 gift-wrap construction, including ephemeral key generation and
seal/wrap layers. For Phase 1 (text notifications), this is sufficient.
Later phases needing structured rumor content can use the lower-level
`client.gift_wrap(receiver, rumor, extra_tags)`.

**Alternatives considered**:
- Raw `EventBuilder::gift_wrap()`: more control but unnecessary for
  Phase 1 text notifications.
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

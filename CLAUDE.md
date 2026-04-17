# cancerbero Development Guidelines

Auto-generated from all feature plans. Last updated: 2026-04-17

## Active Technologies
- Rust (stable, edition 2021) — same toolchain as Phases 1 and 2. + `nostr-sdk 0.44.1`, `mostro-core 0.8.4`, `rusqlite` (bundled), `tokio`, `serde`, `toml`, `tracing`. **New for Phase 3**: `reqwest` (HTTP client for reasoning providers), `sha2` (prompt-bundle hashing and rationale reference ids), `uuid` (session ids). (003-guided-mediation)
- SQLite via rusqlite (direct, no ORM). Extended schema: `mediation_sessions`, `mediation_messages`, `mediation_summaries`, `mediation_events`, `reasoning_rationales`. Prompt/policy content stays out of SQLite (FR-105 / TC-103); only content hashes and bundle ids are persisted. (003-guided-mediation)

- Rust (stable, edition 2021) + nostr-sdk 0.44.1, mostro-core 0.8.4, rusqlite, tokio, serde, toml, tracing (main)

## Project Structure

```text
src/
tests/
```

## Commands

cargo test && cargo clippy

## Code Style

Rust (stable, edition 2021): Follow standard conventions

## Recent Changes
- 003-guided-mediation: Added Rust (stable, edition 2021) — same toolchain as Phases 1 and 2. + `nostr-sdk 0.44.1`, `mostro-core 0.8.4`, `rusqlite` (bundled), `tokio`, `serde`, `toml`, `tracing`. **New for Phase 3**: `reqwest` (HTTP client for reasoning providers), `sha2` (prompt-bundle hashing and rationale reference ids), `uuid` (session ids).

- main: Added Rust (stable, edition 2021) + nostr-sdk 0.44.1, mostro-core 0.8.4, rusqlite, tokio, serde, toml, tracing

<!-- MANUAL ADDITIONS START -->
<!-- MANUAL ADDITIONS END -->

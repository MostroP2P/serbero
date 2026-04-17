# cancerbero Development Guidelines

Auto-generated from all feature plans. Last updated: 2026-04-17

## Active Technologies

Currently shipped in `main` (Phases 1 and 2 implemented):

- Rust (stable, edition 2021)
- `nostr-sdk 0.44.1`, `mostro-core 0.8.4`, `rusqlite` (bundled), `tokio`, `serde`, `toml`, `tracing`, `thiserror`, `anyhow`
- SQLite schema: `disputes`, `notifications`, `dispute_state_transitions`, `schema_version` (migration v2)

## Planned for Phase 3 (spec + plan only; not yet in `main`)

Branch `003-guided-mediation` contains the spec and plan. Implementation
has not landed in `main` — do not assume these are present at runtime:

- New dependencies (planned): `reqwest` (HTTP to reasoning providers), `sha2` (prompt-bundle hashing, rationale reference ids), `uuid` (mediation session ids)
- New SQLite tables (planned, migration v3): `mediation_sessions`, `mediation_messages`, `mediation_summaries`, `mediation_events`, `reasoning_rationales`
- New module tree (planned): `src/chat/`, `src/reasoning/`, `src/prompts/`, `src/mediation/`
- New repo directory (planned): `prompts/phase3-*.md` for the versioned prompt bundle
- Config surface (planned): `[mediation]`, `[reasoning]`, `[prompts]`, `[chat]` sections

These are the source-of-truth artifacts for what Phase 3 will add, but
the runtime crate in `main` does not yet compile or ship them.

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

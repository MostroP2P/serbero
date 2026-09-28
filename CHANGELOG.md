# Changelog

All notable changes to Serbero. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). A tag `vX.Y.Z` publishes the
section `[X.Y.Z]` as its release notes.

## [Unreleased]

Groundwork for assisted mediation. Mediation stays off; nothing here changes
what a v0.1.0 operator sees.

### Added

- Mostro chat transport: sessions and messages tables, dispute chat keys,
  taking a dispute (`admin-take-dispute`) on nodes that speak protocol v2,
  exchanging validated chat messages with both parties, stopping on human
  takeover, and resuming live sessions after a restart.
- A staging run against a real `mostrod` through Ortsom.
- A provider-neutral judge contract, a recorded judge for tests, and the
  TypeSafe adapter for Jev.
- The judge state built from a session, with Nostr identifiers redacted.
- Party message catalogs in English, Spanish and Portuguese, with rule tests
  for every catalog.
- A Spanish judge spike (`eval/spike/`) and the `qs-1` turn question set it
  informed.

## [0.1.0] - 2026-09-27

The notifier: every Mostro dispute reaches every configured solver.

### Added

- Detection of Mostro dispute events, applying only the newest revision of
  each dispute and staying fast with slow or silent relays.
- NIP-44 direct messages to every solver for a new dispute, reminders while
  it stays unattended, and a notice when it is taken.
- Tracking of each dispute until it resolves, with every status recorded.
- SQLite storage with ordered migrations, configuration validated at
  startup, and an operator README.

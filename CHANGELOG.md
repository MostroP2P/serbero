# Changelog

All notable changes to Serbero. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). A tag `vX.Y.Z` publishes the
section `[X.Y.Z]` as its release notes.

## [Unreleased]

### Changed

- Mediation can run as a pilot with the default thresholds of
  `docs/judgments.md` §3 until a calibration report is published. With
  `validated_languages = []`, which a pilot keeps, it gathers the facts and
  hands every case to a solver, and never guides a fund action (README
  "Enable mediation", `docs/evaluation.md` §3.1).

## [0.2.1] - 2026-10-01

### Added

- Secrets can be read from files: `SERBERO_PRIVATE_KEY_FILE` and
  `TYPESAFE_API_KEY_FILE` (any `*_env` variable with `_FILE` appended), as
  Docker secrets and systemd credentials provide. Setting both forms is an
  error, and a secret file readable by any user is logged as a warning.

### Changed

- `deploy/compose.yml` passes secrets as Docker secrets and
  `deploy/serbero.service` as systemd credentials, so they no longer sit in
  the container or process environment. `serbero.env` now holds only
  non-secret settings.

## [0.2.0] - 2026-10-01

Groundwork for assisted mediation. Mediation stays off, and notification
works as in v0.1.0; the startup log also shows Serbero's npub.

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
- The startup log shows Serbero's npub, the form a Mostro admin uses to
  register it as a solver.
- A README that explains mediation end to end: what the parties and solvers
  see, the guarantees, languages, and how to enable it.
- Optional `[[observers]]`: services such as mostro-watchdog get one line
  per mediation update (mediating, could not start, handed off, guidance
  sent), once per dispute, without any party text. Notices are queued with
  the state they report and delivered by their own task, with retries.
- `Dockerfile`, `deploy/compose.yml`, and a hardened systemd unit
  (`deploy/serbero.service`), with README sections on running them and on
  backing up the database. CI builds the image and smoke-tests it.
- Each release publishes a multi-arch container image (amd64, arm64) to
  `ghcr.io/mostrop2p/serbero` with build provenance. `deploy/compose.yml`
  runs a pinned published version; `deploy/compose.build.yml` builds from a
  checkout instead.
- `cargo release minor -x` on `main` cuts a release in one step: it bumps the
  version, dates the changelog, publishes the crate to crates.io
  (`cargo install serbero`), and pushes the `vX.Y.Z` tag that publishes the
  binaries and the container image.

### Changed

- The evaluation binary is now `serbero-eval`
  (`cargo run --bin serbero-eval`), so `cargo install serbero` does not
  install a command named `eval`.
- Logs are colored only on a terminal and never when `NO_COLOR` is set, so
  `docker logs` and the systemd journal carry no escape codes.
- Solver DMs carry the dispute id in the message `id`, and every solver
  message starts with `Dispute <dispute_id> · <subject>`, so clients such as
  Mostrix can link and classify them without parsing prose
  (`docs/messages.md` §3). The new-dispute, reminder, taken, mediation and
  final-report texts moved to that header.

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

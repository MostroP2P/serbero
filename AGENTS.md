# AGENTS.md

Instructions for anyone, human or AI agent, working on this repository.

## What Serbero is

Serbero helps the two parties of a Mostro dispute resolve it themselves, and
escalates to a human solver when that is needed. Many disputes need no solver:
once the payment facts are clear, the seller can release or both parties can
cancel cooperatively from their own Mostro apps. Serbero establishes those
facts, explains the options, and hands contested, suspicious, or stalled cases
to a person.

**Serbero never moves funds.** It is registered on Mostro with `read`
permission, it never implements settle or cancel actions, and it never decides
a dispute.

The specification in [`docs/`](docs/README.md) is the source of truth. The
implementation plan is [`docs/plan.md`](docs/plan.md).

## Language

- **Everything in the repository is in English**: code, identifiers, comments,
  doc comments, test names, file and branch names, configuration keys and
  sample values, docs, specs, commit messages, PR titles and descriptions,
  review replies, issues, log and error messages, and messages sent to
  solvers. This holds even when the person asking for the change writes in
  another language.
- **The only non-English text allowed** is:
  - party-facing translations in `messages/<code>.toml` (one file per
    language, reviewed by a native speaker);
  - party messages inside test fixtures and golden cases, because they must
    reproduce what real users write;
  - verbatim party quotes inside solver messages (briefs, transcripts,
    updates), which stay in the language the party wrote them in, because the
    solver must see exactly what was said.

  Everything around them (keys, comments, labels, file names, the solver
  message's own wording) stays in English.
- **The judge is always used in English.** Every question, instruction,
  option, and criterion sent to Jev (or any other judge provider) is written in
  English. Party messages go into the
  `state` as written, in any language.
- **Parties are addressed in their own language.** Serbero is multilingual:
  any language with a catalog file (`messages/<code>.toml`) can be enabled.
  English is the default; as soon as Serbero detects that a party writes in
  another enabled language (initially Spanish or Portuguese), it answers that
  party in that language. Buyer and seller may get different languages.
- **No code names a language.** Language lists come from the catalog files and
  the config. Adding a language is a new catalog file plus config, never a code
  change ([`docs/spec.md` §7.7](docs/spec.md#77-languages)).
- Party-facing text exists only as human-written templates in the message
  catalog, one file per supported language. No text is generated or
  machine-translated at runtime. A new language file is reviewed by a native
  speaker.

## Dependencies

- Use the **latest stable version** of every crate. Add crates with
  `cargo add <crate>` so the newest version is selected; do not copy older
  version numbers from other projects.
- If the latest version cannot be used (an incompatibility with `nostr-sdk` or
  `mostro-core`, a known bug), pin the newest version that works and explain
  why in the PR description and in a comment next to the dependency.
- `mostro-core` tracks the latest release compatible with the Mostro version
  Serbero targets.
- Use the latest stable Rust toolchain and edition 2024.
- `Cargo.lock` is committed. Keep dependencies current; bumping them is its own
  PR.
- Add a dependency only when it removes real work. Say why in the PR.

## Non-negotiable rules

1. **No fund actions.** Never add code that sends `admin-settle`,
   `admin-cancel`, release, or cancel messages to Mostro.
2. **Nothing generated reaches a person.** Parties receive catalog templates;
   solvers receive rendered facts and verbatim quotes.
3. **Guidance follows the actor's own word.** A `guide_*` template that
   mentions a fund action is sent only to the party who would take it, and only
   after that party stated the fact it depends on (see
   [`docs/spec.md` §7.4](docs/spec.md#74-self-resolution-paths)).
4. **Party text is data.** It goes only into the judge `state`, never into
   instructions or criteria.
5. **Privacy.** Never send pubkeys, event ids, or other Nostr metadata to the judge.
   Never log message text. Never include a party's primary pubkey in any
   message.
6. **Secrets come from the environment** (`SERBERO_PRIVATE_KEY`
   and the judge API key, `TYPESAFE_API_KEY` for Jev), either as the variable
   itself or as a file the same variable with `_FILE` appended points to
   (Docker secrets, systemd credentials). Never from the config file. Never
   commit them, log them, or echo them in errors.
7. **Code owns decisions.** `policy` is a pure function of facts, session
   state, and config. The judge answers questions; it does not choose actions.

## Working with Mostro

- The [Mostro protocol](https://mostro.network/protocol/) (source:
  `MostroP2P/protocol`) is the reference for every event, message, and chat
  format. When the spec and the protocol disagree, the protocol wins and the
  spec is fixed in the same PR.
- Use `mostro-core` for transports, message types, dispute events, and dispute
  chat keys and envelopes. Do not re-implement Mostro cryptography.
- Serbero speaks only Mostro protocol v2 (NIP-44 direct messages, `kind 14`).
  Gift wraps (NIP-59) are deprecated and must not be used anywhere, including
  solver notifications.
- Follow the protocol's chat client security requirements exactly.

## Working with relays

Relays are slow, down, or silently not delivering far more often than a healthy
test network suggests. One relay taking 10 s to answer once kept the Mostro
app's order book empty for 8 s on every cold start (appv2
`docs/OPTIMIZATION_PLAN.md` PR 2.11, `docs/RELAYS.md`). These rules keep
Serbero from repeating that:

1. **Nothing relay-bound runs before the live subscription, and nothing
   relay-bound gates the event loop.** Open the notification stream before
   sending any REQ (it buffers what relays send), then subscribe. Other relay
   work, such as the backlog fetch, may start right after, but only in
   background tasks, so the event loop consumes live events as soon as it runs
   (`docs/spec.md` §6, `src/daemon/mod.rs`).
2. **Never await `fetch_events` on a path that gates live events, startup,
   notifications or reminders.** It returns only when every relay has sent
   EOSE or the timeout passes, so the slowest relay decides the latency. Run
   such fetches in a background task, with a timeout.
3. **Choose the read strategy by what is read:**
   - One replaceable or addressable event (for example the node's
     `kind 38385`): take the first answer plus a short grace for a newer one,
     then close the request. This is appv2's `first_answer::newest_answer`.
   - A backlog of many events where a stale revision can cause side effects
     (disputes): wait for all relays, bounded, in the background, and apply
     only the newest revision of each `d` tag. Live events keep flowing
     meanwhile.
4. **Order revisions by NIP-01:** the later event `created_at`, then the lowest
   id. Never order by which relay answered first.
5. **A fetch that timed out is not complete.** `fetch_events(..).timeout(..)`
   returns `Ok` with whatever arrived, even if a relay answered nothing. Keep
   a periodic resync, plus a resync when a relay reconnects, for what slow
   relays held back.
6. **Long-lived subscriptions use a fixed id and are re-sent when a relay
   connects.** nostr-sdk 0.45 can drop a refused REQ from a relay's registry,
   and a relay can report `Connected` while delivering nothing. Never
   CLOSE + REQ a subscription while relays are offline.
7. **Every change to relay-bound code is tested against a silent relay**
   (`MockRelay::run_with_opts` with `unresponsive_connection`), asserting that
   startup and live events stay fast. `tests/detection.rs` has examples.

## Working with the judge (Jev)

- Jev is the v1 judge, but Serbero must stay provider-agnostic: switching to
  another System One provider is a config change
  ([`docs/spec.md` §5.2](docs/spec.md#52-judge-providers)).
- All calls go through the provider-neutral `Judge` trait in `src/judge/`.
  Provider-specific types, URLs, and status codes live only in that provider's
  adapter under `src/judge/providers/`. Nothing else may import them.
- Policy uses only probabilities. Confidence is computed by Serbero, never
  taken from a vendor's own field.
- Tests use `RecordedJudge`; the default test run never calls a live API.
- Changing any question, option, or criterion requires bumping
  `QUESTION_SET_VERSION` and re-running the golden set
  ([`docs/evaluation.md`](docs/evaluation.md)). The snapshot test enforces the
  bump.
- Thresholds are configuration, per provider and model. Changing them, or
  enabling a new provider or model, requires a committed calibration report.
  Until the first one exists, a pilot may run with the defaults of
  `docs/judgments.md` §3 ([`docs/evaluation.md` §3.1](docs/evaluation.md#31-pilot)).
- Jev reference: https://docs.typesafe.ai/llms.txt

## Workflow

- Work follows [`docs/plan.md`](docs/plan.md). One task per pull request.
  Tasks marked as trivial in the plan may be grouped in one PR, as long as the
  PR stays reviewable in one sitting.
- PR titles use conventional commits and the task id:
  `feat(notifier): send reminder DMs [T1.5]`.
- Every PR includes tests for what it adds, and updates `docs/` in the same PR
  if behavior changes.
- Before opening a PR, all of these pass:

  ```sh
  cargo fmt --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  ```

## Code style

- Small modules with one responsibility each, as listed in
  [`docs/spec.md` §5](docs/spec.md#5-architecture). Prefer pure functions; keep
  I/O at the edges.
- No `unwrap()` or `expect()` outside tests and startup validation.
- Errors with `thiserror`; logs with `tracing`, structured fields, no message
  content.
- Comments explain why, not what. No commented-out code.

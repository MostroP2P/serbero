# Quickstart: Serbero Phase 3 (Guided Mediation)

This quickstart layers on top of the Phase 1 / Phase 2 quickstart in
`specs/002-phased-dispute-coordination/quickstart.md`. Complete that
setup first; Phase 3 assumes a working Phase 1/2 daemon.

## Prerequisites (Phase 3 additions)

- Phases 1 and 2 installed, configured, and verified per the earlier
  quickstart. Your `serbero.db` already exists and has schema `v2`.
- Serbero's pubkey **registered as a solver on the target Mostro
  instance** with at least `read` permission. This is an operator
  action against Mostro — not something Serbero does for itself.
- A reasoning provider API endpoint you can reach, plus the API key
  available as an environment variable. Phase 3 ships one adapter
  (`openai`) which also covers OpenAI-compatible endpoints.

## Configure Phase 3

Extend `config.toml` with the Phase 3 sections. A minimal working
example:

```toml
[mediation]
enabled = true
max_rounds = 2
party_response_timeout_seconds = 1800

# Scope-controlled solver-auth revalidation; defaults shown.
solver_auth_retry_initial_seconds      = 60
solver_auth_retry_max_interval_seconds = 3600
solver_auth_retry_max_total_seconds    = 86400
solver_auth_retry_max_attempts         = 24

[reasoning]
enabled                 = true
provider                = "openai"              # Phase 3 ships this one
model                   = "gpt-5"
api_base                = "https://api.openai.com/v1"
api_key_env             = "SERBERO_REASONING_API_KEY"
# The reasoning adapter owns its own bounded HTTP retry budget
# (FR-104). Lives here, not under [mediation].
followup_retry_count    = 1
request_timeout_seconds = 30

[prompts]
system_instructions_path   = "./prompts/phase3-system.md"
classification_policy_path = "./prompts/phase3-classification.md"
escalation_policy_path     = "./prompts/phase3-escalation-policy.md"
mediation_style_path       = "./prompts/phase3-mediation-style.md"
message_templates_path     = "./prompts/phase3-message-templates.md"

[chat]
inbound_fetch_interval_seconds = 10
```

Credentials are supplied via the environment, not via the config
file. Whatever variable name you set under `[reasoning].api_key_env`
is the one the daemon reads at startup — the default is
`SERBERO_REASONING_API_KEY` (vendor-neutral on purpose):

```bash
export SERBERO_REASONING_API_KEY="<your key>"
```

If you point `api_key_env` at a vendor-specific variable name you
already use (for example `OPENAI_API_KEY` from another tool), that
works too — the variable name is just configuration.

### Running with Phase 3 disabled

Set `[mediation].enabled = false` (or omit the Phase 3 sections
entirely). The daemon remains a Phase 1 / Phase 2 daemon with no
mediation behavior. This is also what happens when the reasoning
provider is unreachable: Phase 1/2 keeps running.

## Prompt bundle

The first time you enable Phase 3, ensure the files referenced by
`[prompts].*` exist. The default paths are relative to the working
directory of the daemon. A reference bundle is shipped under
`./prompts/phase3-*.md`.

## Run

Rebuild and run as in Phase 1/2:

```bash
cargo build --release
SERBERO_REASONING_API_KEY="<key>" ./target/release/serbero
```

At startup you will see (among the Phase 1/2 log lines):

```text
loaded config                    mostro_pubkey=<hex> db_path=serbero.db relay_count=N solver_count=M ...
Phase 3 prompt bundle loaded     prompt_bundle_id=phase3-default policy_hash=<hex>
reasoning provider health check ok
Phase 3 mediation is fully configured; engine task will be spawned    prompt_bundle_id=phase3-default policy_hash=<hex>
```

If reasoning health-check fails, Phase 3 stays disabled for the run
(SC-105) and Phase 1/2 detection + notification continue unaffected.
You will see instead:

```text
Phase 3 reasoning health check failed; mediation disabled for this run (Phase 1/2 detection and notification continue unaffected)
```

If solver auth fails on the initial check, Phase 3 refuses to open
sessions and the background revalidation loop begins (warns logged
per attempt). Phase 1/2 continues unaffected.

## Verify mediation end-to-end (US1 + US2 + US3)

1. Publish a dispute to Mostro in the usual way (buyer-initiated,
   coordination failure — e.g. payment delay).
2. Phases 1 / 2 detect + notify solvers (already covered by the
   earlier quickstart).
3. Phase 3 enters the mediation-eligibility evaluator. If the
   classification policy tags it as `coordination_failure_resolvable`,
   the mediation engine:
   - performs the dispute-chat interaction flow required by the
     current Mostro / Mostrix implementation (verified at
     implementation time, not assumed from the public spec alone),
   - opens a `mediation_sessions` row with state `awaiting_response`,
   - sends the first clarifying message addressed to the buyer's
     **shared pubkey** and the seller's **shared pubkey** (NOT their
     primary pubkeys).
4. Have the buyer and seller reply through the Mostro chat client.
   Serbero ingests the replies via the gift-wrap pipeline, dedupes
   by inner-event id, and advances `round_count`.
5. **Mid-session follow-up (Phase 11 / FR-125..FR-131).** On the
   same ingest-tick cycle that persists a fresh inbound, Serbero
   now re-classifies the updated transcript and dispatches the
   next step automatically. You should observe, within a few tens
   of seconds of the parties replying:
   - `mediation_events` gains one new `classification_produced`
     row for this round (rationale id references
     `reasoning_rationales`, not the general log — FR-120);
   - if the policy decision is another clarifying question, two
     more `mediation_messages` rows appear with
     `direction = outbound` and a body that starts with
     `"Round 1. Buyer: ..."` / `"Round 1. Seller: ..."`; the
     session stays in `awaiting_response`;
   - `mediation_sessions.round_count_last_evaluated` advances to
     match `round_count` (FR-127 idempotency marker — re-running
     the tick without a new inbound is a no-op by design);
   - if the policy decision is `Summarize`, `deliver_summary`
     fires and the session proceeds to step 6 below;
   - if three consecutive `reasoning.classify` calls fail
     (provider unreachable), the session auto-escalates with
     trigger `reasoning_unavailable` via
     `mediation_sessions.consecutive_eval_failures` (FR-130).
6. On the cooperative convergence path, Serbero generates a summary
   and delivers it to the assigned solver (or broadcasts to all
   configured solvers if none is assigned yet) via the existing
   Phase 1/2 notifier. The session transitions through
   `classified → summary_pending → summary_delivered → closed`.

## Verify escalation (US4)

1. Drive a session into any escalation trigger (e.g. let the
   `party_response_timeout_seconds` elapse without replies, or
   exceed `max_rounds` without convergence, or ensure the reasoning
   provider is unreachable).
2. The session transitions to `escalation_recommended`. A
   `mediation_events` row records the trigger and the
   `handoff_prepared` event records the Phase 4 package.
3. Solver notifications surface a "needs human judgment" message via
   the Phase 1/2 notifier. Phase 4 (not yet implemented) will
   consume the handoff package.

## Verify provider swap (US5)

1. Stop the daemon.
2. Edit `[reasoning].provider`, `[reasoning].model`, and/or
   `[reasoning].api_base` to point at a different OpenAI-compatible
   endpoint. Update `[reasoning].api_key_env` to name the env var
   holding the new key, and export it.
3. Restart. New mediation sessions call the new endpoint; no code
   change was required.

## Inspect

```bash
# Recent mediation sessions
sqlite3 serbero.db "SELECT session_id, dispute_id, state, round_count, \
                    policy_hash FROM mediation_sessions ORDER BY started_at DESC LIMIT 20;"

# Session transcript
sqlite3 serbero.db "SELECT direction, party, inner_event_created_at, substr(content,1,80) \
                    FROM mediation_messages WHERE session_id='<sid>' \
                    ORDER BY inner_event_created_at ASC;"

# Escalation / lifecycle events
sqlite3 serbero.db "SELECT kind, substr(payload_json,1,120), occurred_at \
                    FROM mediation_events WHERE session_id='<sid>' \
                    ORDER BY id ASC;"

# Rationale audit store (operator-only; filesystem-permission gated)
sqlite3 serbero.db "SELECT rationale_id, provider, model, policy_hash, generated_at \
                    FROM reasoning_rationales ORDER BY generated_at DESC LIMIT 20;"
```

## Audit: Phase 3 never executed a dispute-closing action

Re-confirm SC-102 at any time:

```bash
# No admin-settle / admin-cancel events signed by Serbero's pubkey
# anywhere in mediation or phase 1/2 tables (there shouldn't be any).
sqlite3 serbero.db "SELECT COUNT(*) FROM notifications \
                    WHERE notif_type IN ('admin_settle','admin_cancel');"
# Expected: 0.
```

Combined with the constitutional invariant that Serbero holds no
credentials for those actions, Phase 3 satisfies `I. Fund Isolation
First`.

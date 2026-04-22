# Contract — Solver-Facing DM Payload (`escalation_handoff/v1`)

The gift-wrapped DM Phase 4 delivers to each recipient. Format
stability is a public contract with solver-side tooling; any
incompatible change MUST bump the version tag on the first line
(e.g. `escalation_handoff/v2`).

## Body shape

The body is a plain-text UTF-8 string. Lines are `\n`-separated.
Sections are separated by one blank line. The machine-readable
payload is a JSON object inline on its own line, not an attachment.

```text
escalation_handoff/v1
Dispute: <dispute_id>
Session: <session_id or "<none — dispute-scoped handoff>">
Trigger: <trigger>

Escalation required for dispute <dispute_id>. Trigger: <trigger>.
This dispute was evaluated by Serbero's mediation assistance
system and requires human judgment. Please run TakeDispute for
dispute <dispute_id> on your Mostro instance to review the full
context.

Handoff payload (JSON):
{"dispute_id":"<dispute_id>","session_id":<session_id|null>,"trigger":"<trigger>","evidence_refs":[...],"prompt_bundle_id":"<bundle>","policy_hash":"<hex>","rationale_refs":[...],"assembled_at":<unix_seconds>}
```

### Field semantics

| Line / field          | Type     | Required | Description                                                                  |
|-----------------------|----------|----------|------------------------------------------------------------------------------|
| Version prefix        | literal  | yes      | Exactly `escalation_handoff/v1`. First line, no leading whitespace.          |
| Header `Dispute:`     | string   | yes      | Mostro dispute id.                                                           |
| Header `Session:`     | string   | yes      | Phase 3 mediation session id if one existed, otherwise the literal `<none — dispute-scoped handoff>`. |
| Header `Trigger:`     | string   | yes      | Escalation trigger (`conflicting_claims`, `party_unresponsive`, etc.).       |
| Human summary         | prose    | yes      | Two sentences: what happened, what Serbero asks the solver to do.            |
| JSON payload line     | JSON     | yes      | Serialized `HandoffPackage`. One line, no pretty-printing. Session id key is omitted (not `null`) when no session existed, matching Phase 3's `skip_serializing_if` behavior. |

### FR-206 compliance (no rationale text)

The JSON payload carries `rationale_refs` (array of SHA-256 hex
strings) but NEVER the rationale text itself. The controlled audit
store (`reasoning_rationales`) remains the only home for rationale
text; solver tooling that wants the full text MUST read from the
operator database, not from the DM.

### FR-207 compliance (assistance identity)

The human summary line literally contains "Serbero's mediation
assistance system" to make the identity explicit. Solvers reading
the DM see Serbero as an assistant, not a Mostro admin.

### Idempotency (recipient side)

The DM is delivered via Nostr gift-wraps. Duplicate delivery (see
spec.md edge case "Dispatcher crash after DM send, before audit
write") may produce two wrapped events with different outer event
ids but the same inner content. Recipient clients SHOULD dedup by
the inner event id (not the outer gift-wrap id) — this is the
standard Nostr pattern and requires no Serbero-specific handling.

## Example

With a session-backed handoff:

```text
escalation_handoff/v1
Dispute: 0x1a2b3c4d
Session: 5f9e-uuid-v4
Trigger: conflicting_claims

Escalation required for dispute 0x1a2b3c4d. Trigger:
conflicting_claims. This dispute was evaluated by Serbero's
mediation assistance system and requires human judgment. Please
run TakeDispute for dispute 0x1a2b3c4d on your Mostro instance to
review the full context.

Handoff payload (JSON):
{"dispute_id":"0x1a2b3c4d","session_id":"5f9e-uuid-v4","trigger":"conflicting_claims","evidence_refs":["inner_event_abc","inner_event_def"],"prompt_bundle_id":"phase3-default","policy_hash":"abcd...1234","rationale_refs":["9f86d081884c7d659a..."],"assembled_at":1745321400}
```

With a dispute-scoped (FR-122) handoff — session_id key omitted:

```text
escalation_handoff/v1
Dispute: 0x9f8e7d6c
Session: <none — dispute-scoped handoff>
Trigger: suspected_fraud

Escalation required for dispute 0x9f8e7d6c. Trigger:
suspected_fraud. This dispute was evaluated by Serbero's mediation
assistance system and requires human judgment. Please run
TakeDispute for dispute 0x9f8e7d6c on your Mostro instance to
review the full context.

Handoff payload (JSON):
{"dispute_id":"0x9f8e7d6c","trigger":"suspected_fraud","evidence_refs":[],"prompt_bundle_id":"phase3-default","policy_hash":"abcd...1234","rationale_refs":["11d4a8c3..."],"assembled_at":1745321500}
```

## Versioning policy

- Non-breaking additions (new JSON keys, new header lines that
  parsers can ignore) MAY land on `v1`. Parsers MUST treat unknown
  header lines and unknown JSON keys as informational and continue.
- Breaking changes (renaming a required key, changing a value's
  type, removing a required line) MUST bump to `v2`. Consumer
  tooling can branch on the first-line prefix.
- The DM body is a contract with solver-side tooling operating on
  DB-cached copies. Downgrades (`v1` → `v0`) are not supported.

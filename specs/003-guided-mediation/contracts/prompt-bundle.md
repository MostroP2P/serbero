# Contract: Prompt Bundle

**Phase**: 3 (Guided Mediation)
**Status**: Contract definition for the versioned files that control agent behavior (spec TC-103 + Instruction and Policy Storage).

## Purpose

Defines the shape, loading, and hashing of the prompt bundle so that:

- every mediation session can pin the exact bundle that governed it;
- operators can review and diff behavioral changes in git;
- auditors can reconstruct behavior from history + `policy_hash`
  alone.

## Files

The default bundle ships under `prompts/` at the repo root:

| Path                                       | Purpose                                                                    |
|--------------------------------------------|----------------------------------------------------------------------------|
| `prompts/phase3-system.md`                 | System instructions, mediator identity, authority limits, honesty rules    |
| `prompts/phase3-classification.md`         | Classification policy: which label applies when; escalation criteria       |
| `prompts/phase3-escalation-policy.md`      | Escalation triggers, thresholds, what evidence the handoff package needs   |
| `prompts/phase3-mediation-style.md`        | Tone, register, how to surface uncertainty, what NOT to say                |
| `prompts/phase3-message-templates.md`      | Message templates used by the reasoning provider when drafting outbound    |

Paths are configurable via `[prompts].*` in `config.toml`. The
in-repo `prompts/` tree is the default.

## Shape

Each file is a plain UTF-8 Markdown document. No frontmatter, no
templating variables consumed by the loader. Templating for party /
dispute context happens at reasoning-call time inside the adapter
(e.g. the model is passed the template and the context separately);
the file itself is bytes.

Recommended structure inside each file:

```markdown
# <Document Title>

## Scope
- What this document is for.
- What Serbero MUST and MUST NOT do per this policy.

## Rules / Guidance
- Concrete rules, one per bullet, referenced by the reasoning prompt.

## Examples (optional)
- Canonical examples of correct behavior.
```

## Loader

Implemented in `src/prompts/mod.rs`.

- On daemon startup (and on operator-triggered config reload) the
  loader reads all configured paths.
- Missing or unreadable files MUST fail Phase 3 initialization
  loudly (ERROR log). Phase 1/2 MUST continue operating if
  `[mediation].enabled = false` is effectively forced by the failure.
- No caching beyond process lifetime; on restart the bundle is
  re-read and re-hashed.

## Hashing (`policy_hash`)

Implemented in `src/prompts/hash.rs`.

The `policy_hash` is computed as follows:

```text
SHA-256(
    "serbero/phase3\0" ||
    "system\0"           || system_bytes           || "\0" ||
    "classification\0"   || classification_bytes   || "\0" ||
    "escalation\0"       || escalation_bytes       || "\0" ||
    "mediation_style\0"  || mediation_style_bytes  || "\0" ||
    "message_templates\0"|| message_templates_bytes
)
```

- Hex-encoded SHA-256, lowercase.
- Order is fixed in code, not derived from the configured paths, so
  two operators reordering `[prompts].*` keys do NOT produce
  different hashes when the bytes are identical.
- Null-byte delimiters prevent boundary-collision attacks (two
  adjacent files whose concatenation looks like a rearranged pair).

## Pinning

- Every `mediation_sessions` row stores `prompt_bundle_id` and
  `policy_hash` at creation time.
- Changing files in `prompts/` does NOT retroactively alter open
  sessions — the session continues to act against its pinned bundle
  until it terminates. Restart preserves the pin.
- Every `mediation_summaries`, `mediation_events`, and
  `reasoning_rationales` row that the session produces references
  the same `prompt_bundle_id` + `policy_hash`.

## `prompt_bundle_id`

A human-readable identifier of the bundle used. Defaults to
`phase3-default`. Can be overridden in a future operator-facing
config knob (not in Phase 3 scope). `instructions_version` MAY be
set in config to a git rev or semver tag for human-facing audit
without affecting the hash.

## What the bundle MUST specify (non-exhaustive)

From the constitution + spec:

- Assistance-only identity; never final authority.
- No fund movement or dispute closure suggestions, ever.
- Explicit honesty discipline: surface uncertainty; do not fabricate.
- How to escalate and why (`phase3-escalation-policy.md` is the
  authoritative enumeration of triggers, mirroring FR-111).
- Allowed output shapes (classification with confidence, next-message
  drafts, structured summaries, explicit escalation recommendation).
- Disallowed output shapes (autonomous closure, binding decisions,
  fabricated party statements, fund-related instructions).

## What the bundle MUST NOT do

- Overload the hash surface by embedding dispute-specific data.
  Bundle bytes are static per release; per-dispute context is passed
  as `ReasoningContext` at call time (see the reasoning-provider
  contract), not baked into the files.
- Carry credentials or environment-specific URLs. Those live in
  `[reasoning]` config.

## Review discipline

Bundle changes are PR-reviewable as Markdown diffs. Changing any
prompt file bumps `policy_hash`, which means pre-change and
post-change sessions are distinguishable in the audit trail. This is
the intended property (FR-106 / SC-103).

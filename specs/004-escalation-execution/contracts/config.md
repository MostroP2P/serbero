# Contract — `[escalation]` Config Section

Extends the existing `config.toml` schema with one new section.
Back-compat: when the section is absent, Phase 4 behaves as if
`enabled = false` (the dispatcher task is not spawned and Phase
1/2/3 behavior is unchanged — FR-216).

## TOML schema

```toml
[escalation]
# Feature flag. When false, Phase 4 is inert: no dispatcher task is
# spawned, no migration-v4-added table is read or written beyond
# migration-time initialization, and Phase 1/2/3 behavior is
# completely unchanged. Default: false — Phase 4 is opt-in.
enabled = true

# How often the dispatcher scans `mediation_events` for pending
# `handoff_prepared` rows. Integer seconds, positive. The 60-second
# SC-201 delivery target assumes a value of 30 here; larger values
# degrade delivery latency linearly. Default: 30.
dispatch_interval_seconds = 30

# When zero solvers with `Write` permission are configured,
# `fallback_to_all_solvers = true` broadcasts the handoff DM to
# every configured solver regardless of permission; `false` (the
# default) refuses to broadcast and records an
# `escalation_dispatch_unroutable` audit event plus an ERROR log
# line. Operators running read-only deployments intentionally must
# opt in. Default: false.
fallback_to_all_solvers = false
```

## Validation rules

- `enabled`: must be a bool. Missing → `false` (Phase 4 disabled).
- `dispatch_interval_seconds`: must be a positive integer. Values
  ≤ 0 cause a startup config-error (loud fail, not a silent
  clamp). Missing → 30.
- `fallback_to_all_solvers`: must be a bool. Missing → `false`.
- Unknown keys inside `[escalation]` MUST produce a loud warning
  at startup (matches the existing config loader's discipline) so
  a typo does not silently disable the feature.

## Interaction with other config

- `[solvers]` already carries `permission = "Read" | "Write"` per
  entry. Phase 4's routing rules (FR-202) consume this field
  directly; no additional solver-side config is required. A
  deployment that wants Phase 4 MUST have at least one
  `permission = "Write"` solver OR set
  `fallback_to_all_solvers = true` OR accept that every Phase 3
  escalation will produce an `escalation_dispatch_unroutable`
  audit row until the config is corrected.

- `[mediation]`, `[reasoning]`, `[prompts]`, `[chat]` are
  unchanged. Phase 4 does not depend on Phase 3's enable flags;
  it consumes Phase 3 output from the audit table, which is
  durable regardless of whether Phase 3 is currently enabled.

## Change policy

Adding new keys inside `[escalation]` is a backward-compatible
change (parsers that do not know about the key fall back to the
default). Removing or renaming an existing key is a breaking
change and MUST be paired with a daemon-level version bump plus
operator-facing migration notes.

## Default-config reference

A minimal addition to `config.sample.toml` for the documentation
site:

```toml
[escalation]
enabled = true
dispatch_interval_seconds = 30
fallback_to_all_solvers = false
```

Operators running a read-only / observability-only deployment
typically want:

```toml
[escalation]
enabled = false
```

Operators running a single-solver write-permission deployment
want the defaults above; no further tuning is needed for the
60-second SC-201 target.

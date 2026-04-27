# Contract: `[mediation]` Configuration Surface (Cooperative Self-Resolution)

## New Keys

This feature adds two keys under the existing `[mediation]`
section of `config.toml`. Both have `serde(default)` semantics so
operators upgrading without editing their config file inherit
sensible defaults.

| Key | Type | Default | Backed by FR / SC |
|-----|------|---------|-------------------|
| `self_resolution_threshold` | `f32` (range `0.0..=1.0`) | `0.75` | FR-010 |
| `self_resolution_enabled` | `bool` | `true` | FR-011, SC-007 |

## Operator-facing Documentation (suggested `config.example.toml` snippet)

```toml
[mediation]
# ... existing keys ...

# --- Cooperative self-resolution branch (Feature 005) ---

# Confidence floor at which Serbero invites parties to coordinate
# the resolution among themselves on a coordination_failure_resolvable
# classification. Range: 0.0..=1.0.
#
# Default 0.75 is conservative. Set higher (e.g. 0.90) to fire only
# on very-high-confidence cooperative cases. Set lower at your own
# operational risk — the feature is one of two lines of defence
# against false-positive cooperative classifications, the other
# being the static-template / no-fund-action-keyword guarantee.
#
# Setting this to 1.0 effectively disables the branch (no
# real-world classifier emits exactly 1.0); use the kill-switch
# below instead.
#
# Default: 0.75
self_resolution_threshold = 0.75

# Master kill-switch for the cooperative self-resolution branch.
# When false, the branch is bypassed entirely and Serbero behaves
# byte-for-byte as it did before this feature shipped (the legacy
# cooperative-summary path runs unchanged).
#
# Use during incident windows or audit reviews when you want to
# force every cooperative case through human review.
#
# Default: true
self_resolution_enabled = true
```

## Validation

`MediationConfig` validation extends with two checks:

```rust
if !(0.0..=1.0).contains(&cfg.self_resolution_threshold) {
    return Err(...);
}
```

`self_resolution_enabled` is a plain bool — no range validation.

If validation fails at startup, the daemon refuses to start with a
clear error message identifying the offending key. This matches
the existing `MediationConfig` validation pattern for similar
range-constrained f32 fields.

## Interaction With Existing Keys

- **`renotification_seconds`**, **`renotification_check_interval_seconds`**,
  **engine tick interval**: not affected. The new branch fires on
  the same engine tick that would have fired the existing
  cooperative summary; no new schedule.
- **`session_timeout_seconds`** (party-unresponsive timeout): not
  affected. The existing timeout still applies after a
  `self_resolution_offered` event the same way it applies after
  any other outbound; if both parties go silent for longer than
  the configured window, the existing
  `PartyUnresponsiveTimeout` escalation fires (and the SC-004
  silence-rate budget catches systemic patterns).

## Operational Recipes

### Disable the feature globally during an incident

```toml
[mediation]
self_resolution_enabled = false
```

Restart the daemon. The legacy cooperative-summary path runs
unchanged (verified by SC-007 byte-for-byte test).

### Tighten the trigger to very-high-confidence cases only

```toml
[mediation]
self_resolution_threshold = 0.90
```

Restart the daemon. Cases with confidence between 0.75 and 0.90
fall through to the legacy summary-only path.

### Roll out behind a feature flag

`self_resolution_enabled = false` in production while the
templates and code land. Flip to `true` in a follow-up deploy
once translation review and SC-001 / SC-002 baseline metrics are
in place.

## Downgrade Path

If a future deploy reverts this feature:

1. Set `self_resolution_enabled = false` in operator config and
   restart. Legacy behaviour resumes immediately.
2. Optionally roll back the binary. The `mediation_events` rows
   with `kind = 'self_resolution_offered'` left behind by the
   feature-on period are ignored by the legacy code (the column
   is unconstrained TEXT). No DB cleanup required.

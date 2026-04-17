# Data Model: Phased Dispute Coordination

**Date**: 2026-04-16
**Spec**: [spec.md](spec.md)

## SQLite Schema

### Phase 1 Tables

#### disputes

Stores detected dispute events. Primary deduplication table.

| Column          | Type    | Constraints              | Description                                      |
|-----------------|---------|--------------------------|--------------------------------------------------|
| dispute_id      | TEXT    | PRIMARY KEY              | From `d` tag of kind 38386 event                 |
| event_id        | TEXT    | NOT NULL, UNIQUE         | Nostr event ID                                   |
| event_author_pubkey | TEXT | NOT NULL              | Nostr pubkey of the dispute event author         |
| mostro_instance_name | TEXT | NULL                 | Optional Mostro instance name from `y[2]`        |
| initiator_role  | TEXT    | NOT NULL                 | "buyer" or "seller" (from `initiator` tag)       |
| dispute_status  | TEXT    | NOT NULL DEFAULT 'initiated' | From `s` tag: "initiated", "in-progress"     |
| event_timestamp | INTEGER | NOT NULL                 | Unix timestamp from the Nostr event              |
| detected_at     | INTEGER | NOT NULL                 | Unix timestamp when Serbero first saw this event |

#### notifications

Records every notification attempt to a solver.

| Column        | Type    | Constraints     | Description                                       |
|---------------|---------|-----------------|---------------------------------------------------|
| id            | INTEGER | PRIMARY KEY AUTOINCREMENT | Auto-generated row ID                    |
| dispute_id    | TEXT    | NOT NULL, FK → disputes | Reference to the dispute                    |
| solver_pubkey | TEXT    | NOT NULL        | Solver's Nostr public key                         |
| sent_at       | INTEGER | NOT NULL        | Unix timestamp of the attempt                     |
| status        | TEXT    | NOT NULL        | "sent" or "failed"                                |
| error_message | TEXT    | NULL            | Error details if status = "failed"                |
| notif_type    | TEXT    | NOT NULL DEFAULT 'initial' | "initial", "re-notification", "assignment", "escalation" |

### Phase 2 Additions

#### dispute_state_transitions

Tracks lifecycle state changes for coordination visibility.

| Column         | Type    | Constraints     | Description                                      |
|----------------|---------|-----------------|--------------------------------------------------|
| id             | INTEGER | PRIMARY KEY AUTOINCREMENT | Auto-generated row ID                   |
| dispute_id     | TEXT    | NOT NULL, FK → disputes | Reference to the dispute                   |
| from_state     | TEXT    | NULL            | Previous state (NULL for initial)                |
| to_state       | TEXT    | NOT NULL        | New state                                        |
| transitioned_at| INTEGER | NOT NULL        | Unix timestamp of transition                     |
| trigger        | TEXT    | NULL            | What caused the transition (event ID, timeout, etc.) |

New column on `disputes` table:

| Column             | Type    | Description                                      |
|--------------------|---------|--------------------------------------------------|
| lifecycle_state    | TEXT    | "new", "notified", "taken", "waiting", "escalated", "resolved" |
| assigned_solver    | TEXT    | Solver public key if taken, NULL otherwise       |
| last_notified_at   | INTEGER | Timestamp of last notification sent              |
| last_state_change  | INTEGER | Timestamp of last lifecycle state transition     |

### Phase 3+ Additions (Forward-Looking, Provisional)

> **Status**: The tables and columns in this section are **forward-looking
> and provisional**. They are illustrative sketches to help reason about
> schema evolution — not normative schema definitions. Phase 1 and Phase 2
> implementations MUST NOT create these tables, add these columns, or
> depend on their exact shape. When Phase 3 and Phase 4 are planned
> through their own specification amendments, these shapes are expected
> to be revisited and refined; treat anything here as subject to change
> without a migration path from Phase 1/Phase 2 state.

#### mediation_sessions (Phase 3 — provisional sketch)

| Column           | Type    | Description                                    |
|------------------|---------|------------------------------------------------|
| id               | INTEGER | PRIMARY KEY AUTOINCREMENT                      |
| dispute_id       | TEXT    | FK → disputes                                  |
| started_at       | INTEGER | When mediation began                           |
| ended_at         | INTEGER | When mediation concluded                       |
| classification   | TEXT    | Dispute category assigned                      |
| confidence_score | REAL    | 0.0 to 1.0                                    |
| outcome          | TEXT    | "suggestion_sent", "escalated", "timed_out"    |

#### escalation_records (Phase 4 — provisional sketch)

| Column             | Type    | Description                                  |
|--------------------|---------|----------------------------------------------|
| id                 | INTEGER | PRIMARY KEY AUTOINCREMENT                    |
| dispute_id         | TEXT    | FK → disputes                                |
| trigger            | TEXT    | Escalation trigger reason                    |
| target_solver      | TEXT    | Write-permission solver pubkey               |
| summary            | TEXT    | Structured escalation summary                |
| escalated_at       | INTEGER | Timestamp                                    |
| acknowledged       | INTEGER | 0 or 1                                       |
| acknowledged_at    | INTEGER | NULL until acknowledged                      |

## State Machine

### Dispute Lifecycle States (Phase 2)

```
                  ┌─────────┐
    event detected│   new   │
                  └────┬────┘
                       │ solvers notified
                  ┌────▼────┐
              ┌───│ notified│───┐
   timeout    │   └────┬────┘   │ solver takes
   re-notify  │        │        │ via Mostro
              └────────┘   ┌────▼────┐
                           │  taken  │
                           └────┬────┘
                                │ (Phase 3+)
                           ┌────▼────┐
                           │ waiting │
                           └────┬────┘
                      ┌─────────┼─────────┐
                 ┌────▼────┐         ┌────▼────┐
                 │escalated│         │resolved │
                 └─────────┘         └─────────┘
```

**Phase 1 states**: Only `new` → row inserted in `disputes`.
Deduplication is based on `dispute_id` existence.

**Phase 2 states**: Full lifecycle with `lifecycle_state` column.
Transitions recorded in `dispute_state_transitions`.

## Entity Relationships

```
disputes 1──N notifications
disputes 1──N dispute_state_transitions
disputes 1──1 mediation_sessions    (Phase 3 — provisional)
disputes 1──N escalation_records    (Phase 4 — provisional)
```

The Phase 3+ relationships are drawn from the forward-looking sketches
above and are themselves provisional.

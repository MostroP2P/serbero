<p align="center">
  <img src="cancerbero.jpg" alt="Cancerbero" width="400">
</p>

# Cancerbero

Dispute coordination, notification, and assistance system for the [Mostro](https://mostro.network/) ecosystem.

Cancerbero helps operators and users handle disputes more quickly, more consistently, and with better visibility — without expanding the system's fund-risk surface.

## What It Does

Cancerbero sits alongside Mostro as a coordination layer that:

- **Detects disputes** by subscribing to Mostro's dispute events on Nostr relays.
- **Notifies operators** promptly via encrypted gift-wrap messages, with re-notification and escalation when disputes go unattended.
- **Tracks dispute state** so operators can see whether a dispute is new, taken, being assisted, waiting, or escalated.
- **Guides mediation** for common coordination failures (payment delays, unresponsive counterparties, process confusion) by communicating with parties and collecting context.
- **Escalates to humans** when claims conflict, fraud is suspected, confidence is low, or policy requires human judgment.

## What It Does Not Do

Cancerbero never moves funds. It cannot sign `admin-settle` or `admin-cancel`, and it is never granted credentials that would allow it to do so. Dispute closure authority belongs to Mostro and its human operators.

Mostro operates normally with or without Cancerbero. If Cancerbero is offline, operators continue resolving disputes manually as they always have.

## Architecture

```
┌─────────────┐       Nostr Events        ┌─────────────────┐
│   Mostro    │ ──────────────────────────>│   Cancerbero    │
│             │                            │                 │
│  - Escrow   │                            │  - Detection    │
│  - Settle   │       Gift Wraps           │  - Notification │
│  - Cancel   │ <─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ │  - Mediation    │
│  - Perms    │                            │  - Escalation   │
└─────────────┘                            │  - Audit Log    │
                                           └────────┬────────┘
                                                    │
                                           ┌────────▼────────┐
                                           │    Reasoning     │
                                           │    Backend       │
                                           │  (replaceable)   │
                                           └─────────────────┘
```

- **Mostro** owns escrow state, permissions, and dispute-closing authority.
- **Cancerbero** owns notification, coordination, assistance, and escalation support.
- The **reasoning backend** (direct API by default, OpenClaw optional) is behind a defined interface and replaceable without changing core logic.

## Technical Constraints

- Written in **Rust**
- Uses **nostr-sdk v0.44.1** for all Nostr communication, subscriptions, event handling, and gift-wrap messaging
- Prefers **Nostr-native** communication (encrypted gift wraps) over external bridges or dashboards

## Project Principles

Cancerbero is governed by a [constitution](.specify/memory/constitution.md) that defines non-negotiable rules. The key principles:

1. **Fund Isolation First** — never touch funds or sign dispute-closing actions
2. **Protocol-Enforced Security** — safety boundaries enforced by Mostro, not by prompts or model behavior
3. **Human Final Authority** — complex, adversarial, or ambiguous disputes always go to a human operator
4. **Operator Notification as Core** — detecting and notifying operators is a primary responsibility, not a secondary feature
5. **Assistance Without Authority** — assist and guide, never impose outcomes
6. **Auditability by Design** — every action, classification, and state transition is logged
7. **Graceful Degradation** — Mostro works fine without Cancerbero
8. **Privacy by Default** — minimum necessary information to each participant
9. **Nostr-Native Coordination** — encrypted messaging first, external integrations second
10. **Portable Reasoning Backends** — no lock-in to any single AI provider or runtime
11. **Incremental Scope** — evolve in stages through explicit specifications
12. **Honest System Behavior** — surface uncertainty, never fabricate evidence
13. **Mostro Compatibility** — complement Mostro, never duplicate or weaken its authority

## Status

Early development. The initial specification covers dispute detection, operator notification, assignment visibility, basic guided mediation, and escalation support.

## License

Cancerbero is licensed under the [MIT License](LICENSE).

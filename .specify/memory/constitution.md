<!--
Sync Impact Report
===================
Version change: N/A (initial) → 1.0.0
Modified principles: None (initial adoption)
Added sections:
  - 13 Core Principles (Fund Isolation First through Mostro Compatibility)
  - Scope and Context section
  - Governance section with amendment procedure
Removed sections: None
Templates requiring updates:
  - .specify/templates/plan-template.md — ✅ no update needed
    (Constitution Check section is generic; will be filled per-feature)
  - .specify/templates/spec-template.md — ✅ no update needed
    (Template is domain-agnostic; principles apply at review time)
  - .specify/templates/tasks-template.md — ✅ no update needed
    (Task phases are generic; constitution constraints apply at task creation)
Follow-up TODOs: None
-->

# Cancerbero Constitution

## Scope and Context

Cancerbero is a dispute coordination, notification, and assistance
system for the Mostro ecosystem. Its purpose is to help operators
and users handle disputes more quickly, more consistently, and with
better visibility, without expanding the system's fund-risk surface.

This constitution defines the non-negotiable principles that MUST
guide all Cancerbero specifications, plans, tasks, and
implementations.

## Core Principles

### I. Fund Isolation First

Cancerbero implementations MUST never directly move funds, settle
escrows, cancel escrows, or sign administrative actions that close
disputes.

- Cancerbero components MUST NOT execute or sign `admin-settle` or
  `admin-cancel`, and they MUST never be granted credentials or
  permissions that would allow them to do so.
- Any action that can release, refund, or otherwise move value MUST
  remain outside Cancerbero.

### II. Protocol-Enforced Security Boundaries

All security-critical boundaries MUST be enforced by Mostro at the
protocol, authorization, and permission layers — never by prompts,
model instructions, UI restrictions, or operator assumptions alone.

- Cancerbero may operate with read solver permissions, but its safety
  model MUST NOT depend on an AI model behaving correctly.
- If a boundary matters for funds, dispute closure, or operator
  authority, that boundary MUST be enforced by the system that owns
  those responsibilities.

### III. Human Final Authority

Cancerbero may assist, classify, summarize, notify, guide, and
escalate, but final authority over ambiguous, adversarial, fraudulent,
or non-cooperative disputes MUST remain with a human operator holding
write permissions.

Cancerbero specifications and implementations MUST support clean
handoff to human operators whenever:

- Cooperative resolution is not emerging.
- Fraud or deception is suspected.
- User claims materially conflict.
- Available evidence is insufficient.
- Policy requires human judgment.

### IV. Operator Notification Is a Core Responsibility

Operator notification is not an auxiliary feature. It is a core
responsibility of the Cancerbero system.

- Cancerbero implementations MUST detect new disputes and notify
  relevant operators reliably and promptly.
- They MUST support re-notification and escalation when disputes
  remain unattended or require higher-authority intervention.
- Designs that treat operator awareness as optional, best-effort,
  or secondary MUST be rejected.

### V. Assistance Without Authority

Cancerbero components may communicate with users, collect context,
ask clarifying questions, summarize dispute state, and guide parties
toward safe cooperative outcomes.

- They MUST NOT present themselves as the final authority on dispute
  outcomes, and they MUST NOT imply powers they do not possess.
- Cancerbero may support dispute resolution, but it MUST NOT
  autonomously impose dispute closure.

### VI. Auditability by Design

Cancerbero specifications and implementations MUST preserve enough
traceability for operators to understand:

- What happened.
- What Cancerbero observed.
- What Cancerbero asked.
- How Cancerbero classified or summarized a dispute.
- Why Cancerbero escalated or re-notified.
- What information was available at each step.

Auditability is required for trust, debugging, postmortems, and
safe human oversight.

### VII. Graceful Degradation

The Mostro system MUST remain operational if Cancerbero is
unavailable, degraded, misconfigured, or offline.

- Cancerbero is an assistance and coordination layer, not a hard
  dependency for dispute closure.
- Cancerbero designs MUST preserve manual resolution paths so that
  human operators can continue resolving disputes through Mostro
  even when Cancerbero is absent.

### VIII. Privacy by Default

Cancerbero components MUST expose only the minimum information
necessary to the relevant participants and operators.

- Notifications, summaries, and mediation flows MUST avoid
  unnecessary disclosure.
- Private dispute details MUST NOT be broadcast more widely than
  required for resolution.
- Any future support for shared channels, operator groups, or
  broader coordination surfaces MUST explicitly justify its privacy
  tradeoffs.

### IX. Nostr-Native Coordination

Cancerbero SHOULD prefer Nostr-native communication primitives for
notifications and dispute assistance, especially direct encrypted
messaging such as gift wraps.

- External bridges, dashboards, or integrations may exist, but they
  MUST remain secondary to the Nostr-native operating model unless a
  specification justifies otherwise.

### X. Portable Reasoning Backends

Cancerbero MUST NOT be architecturally bound to a single agent
runtime or reasoning provider.

- Cancerbero implementations may use LLMs or agentic systems for
  classification, mediation support, summarization, and escalation
  decisions, but these capabilities MUST remain replaceable behind
  clear interfaces.
- The default deployment path SHOULD favor portability and ease of
  adoption. Direct API-based reasoning backends MUST be supported as
  a first-class option.
- OpenClaw integration may be supported as an optional backend, but
  it MUST NOT be a mandatory dependency of the system.

### XI. Incremental Scope and Clear Boundaries

Cancerbero specifications and implementations MUST evolve in stages.

Initial scopes SHOULD focus on:

- Dispute detection.
- Operator notification.
- Dispute intake and assignment visibility.
- Basic guided mediation.
- Reliable escalation.

More advanced automation, richer operator coordination, group-based
workflows, or expanded intelligence layers MUST be introduced only
through explicit specifications that preserve the principles of this
constitution.

### XII. Honest System Behavior

Cancerbero components MUST NOT fabricate evidence, imply certainty
they do not have, or misrepresent the basis for their
classifications, summaries, or recommendations.

- When information is incomplete, conflicting, or unclear, Cancerbero
  implementations MUST surface uncertainty and escalate appropriately
  rather than pretending to know more than they do.

### XIII. Mostro Compatibility and Separation of Concerns

Cancerbero exists to complement Mostro, not to duplicate or weaken
its authority boundaries.

Cancerbero specifications and implementations MUST preserve a clear
division of responsibility:

- **Mostro** owns escrow state, permissions, and dispute-closing
  authority.
- **Cancerbero** owns notification, coordination, assistance, and
  escalation support.

Any design that blurs that boundary MUST be rejected or revised.

## Governance

This constitution supersedes all other practices for the Cancerbero
project. All specifications, plans, tasks, and implementations MUST
comply with the principles defined above.

### Amendment Procedure

1. Amendments MUST be proposed as a documented change with rationale.
2. Each amendment MUST be reviewed against the existing principles
   to confirm it does not introduce contradictions.
3. Amendments MUST be versioned according to semantic versioning:
   - **MAJOR**: Backward-incompatible principle removals or
     redefinitions.
   - **MINOR**: New principle or section added, or materially
     expanded guidance.
   - **PATCH**: Clarifications, wording, typo fixes, non-semantic
     refinements.

### Compliance Review

- All specifications MUST include a constitution compliance check
  before approval.
- All implementation plans MUST verify alignment with these
  principles at the Constitution Check gate.
- Violations MUST be documented and justified in the Complexity
  Tracking section of the relevant plan.

**Version**: 1.0.0 | **Ratified**: 2026-04-16 | **Last Amended**: 2026-04-16

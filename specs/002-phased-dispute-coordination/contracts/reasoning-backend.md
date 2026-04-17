# Contract: Reasoning Backend Interface

**Phase**: 5 (defined early for architectural alignment)
**Status**: Interface definition only — not implemented until Phase 5

## Purpose

Defines the boundary between Serbero's policy/orchestration layer and
any reasoning backend. The reasoning backend is advisory only. Serbero's
policy layer owns all decisions about escalation, routing, and
permissions.

## Trait Definition

```rust
/// Reasoning backend trait.
///
/// Implementations provide dispute classification, mediation support,
/// and escalation summary generation. All outputs are advisory —
/// Serbero's policy layer independently validates them before acting.
#[async_trait]
pub trait ReasoningBackend: Send + Sync {
    /// Classify a dispute and suggest actions.
    async fn classify(
        &self,
        request: ClassificationRequest,
    ) -> Result<ClassificationResponse, ReasoningError>;

    /// Generate a mediation message for a dispute party.
    async fn suggest_mediation_message(
        &self,
        request: MediationRequest,
    ) -> Result<MediationResponse, ReasoningError>;

    /// Generate an escalation summary for a human operator.
    async fn summarize_for_escalation(
        &self,
        request: EscalationSummaryRequest,
    ) -> Result<EscalationSummary, ReasoningError>;
}
```

## Request/Response Types

### ClassificationRequest

```rust
pub struct ClassificationRequest {
    pub dispute_id: String,
    pub initiator_role: String,        // "buyer" or "seller"
    pub dispute_context: String,       // available dispute metadata
    pub party_messages: Vec<Message>,  // messages collected during mediation
}
```

### ClassificationResponse

```rust
pub struct ClassificationResponse {
    pub classification: DisputeCategory,
    pub confidence: f64,               // 0.0 to 1.0
    pub suggested_actions: Vec<SuggestedAction>,
    pub rationale: Vec<String>,        // key factors considered
    pub flags: Vec<Flag>,              // fraud-risk, conflicting-claims, low-info
}
```

### DisputeCategory

```rust
pub enum DisputeCategory {
    CoordinationFailure,   // payment delay, confusion, unresponsive
    ConflictingClaims,     // parties disagree on facts
    SuspectedFraud,        // fraud indicators detected
    Unclear,               // insufficient information to classify
}
```

### Flag

```rust
pub enum Flag {
    FraudRisk,
    ConflictingClaims,
    LowInfo,
    UnresponsiveParty,
}
```

### SuggestedAction

```rust
pub enum SuggestedAction {
    AskClarification(String),    // question to ask a party
    SuggestResolution(String),   // cooperative resolution suggestion
    Escalate(String),            // reason to escalate
}
```

## Policy Layer Validation Rules

The policy layer MUST independently validate all reasoning output:

1. If `flags` contains `FraudRisk` or `ConflictingClaims` →
   escalate regardless of suggested actions.
2. If `confidence < configurable_threshold` → escalate.
3. If `suggested_actions` contains `Escalate` → escalate.
4. If reasoning backend returns error → escalate immediately.
5. Reasoning backend MUST NOT be called for `admin-settle`,
   `admin-cancel`, or any fund-related decision.

## Backend Implementations

### Direct API Backend (Default)

Calls an LLM API (e.g., Claude) with structured prompts and parses
structured JSON responses into `ClassificationResponse`.

### OpenClaw Backend (Optional)

Delegates to an OpenClaw agent. Same trait interface, different
transport and orchestration.

### Null Backend (Testing/Fallback)

Returns `ReasoningError::Unavailable` for all requests. Used for
testing the escalation fallback path.

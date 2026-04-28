# Contract: Self-Resolution Template Bundle

**File**: `prompts/phase3-self-resolution.md`
**Loaded by**: `src/prompts/mod.rs::load_bundle` together with the
other Phase 3 prompt files; the parser implementation lives in
`src/prompts/self_resolution_parser.rs`.
**Pinned via**: the existing `prompt_bundle_id` + `policy_hash` on
`mediation_sessions`. The hash extends over the cooperative-self-
resolution bytes via `prompts::hash::policy_hash_v2` when the file
is present; legacy deployments without the file fall back to
`policy_hash` (v1) so the hash does not rotate. Sessions opened
against bundle v1 see v1 templates even after a v2 deploys.

## File Format

The file is Markdown with one section per supported language. Each
section is introduced by an ISO-639-1 code in square brackets and
contains exactly two key-value pairs.

```markdown
# Self-Resolution Templates

[en]
template = "Thanks for the information. The typical resolution in
this kind of case depends on an action that both parties can
coordinate themselves."
human_assistance_optin = "If you'd prefer human assistance, let me
know and I'll route you to the assigned solver."

[es]
template = "Gracias por la información. La resolución típica en
este tipo de casos depende de una acción que ambas partes pueden
coordinar entre ustedes."
human_assistance_optin = "Si prefieres asistencia humana, dímelo y
te redirijo al solver asignado."

[pt]
template = "Obrigado pela informação. A resolução típica neste
tipo de caso depende de uma ação que ambas as partes podem
coordenar entre si."
human_assistance_optin = "Se preferires assistência humana, diz-me
e te redireciono para o solver designado."
```

## Rendering

The full message a party receives is, byte-for-byte:

```
{template} {human_assistance_optin}
```

The two strings are concatenated with exactly one ASCII space.
Parties never see any framing prefix (no `Buyer:`, no `Round N.`),
consistent with the chat-scaffolding cleanup shipped earlier in
`main`.

## Parsed Representation

```rust
pub struct SelfResolutionTemplates {
    pub by_language: HashMap<String, SelfResolutionLanguageEntry>,
    pub fallback_language: String,  // typically "en"
}

pub struct SelfResolutionLanguageEntry {
    pub template: String,
    pub human_assistance_optin: String,
}
```

`fallback_language` MUST be present in `by_language`. Initial
shipping value: `"en"`.

## Banned Substrings (FR-004 / SC-003)

The keyword-audit unit test (`tests/phase3_self_resolution_template_audit.rs`)
loads the bundle and walks every `(language, entry)` cell, asserting
that the rendered string `format!("{} {}", template,
human_assistance_optin)` does **NOT** contain any of the language's
banned substrings.

Comparison normalization: both the rendered string and each banned
substring are passed through `str::to_ascii_lowercase` before the
substring check. ASCII byte case is folded; non-ASCII bytes
(diacritics like `ñ`, `ç`, `á`) are preserved verbatim in both
sides of the comparison. This is intentional — adding Unicode
normalization would pull a new dependency for negligible coverage
gain (the banned list already enumerates the diacritic-bearing
forms, and translators submit copy in NFC the keyboard input
methods produce). New languages MUST follow the same rule: list the
diacritic-bearing forms verbatim.

| Language tag | Banned substrings (representative; canonical list lives in the test file) |
|--------------|---------------------------------------------------------------------------|
| `en` | `release`, `settle`, `cancel`, `disburse`, `transfer`, `refund`, `payout`, `wire`, `force-close`, `admin-settle`, `admin-cancel` |
| `es` | `liberar`, `liberen`, `cancelar`, `cancelen`, `saldar`, `transferir`, `transferencia`, `reembolsar`, `reembolso`, `pagar`, `paguen`, `envíen el fiat`, `envíen los sats`, `cerrar la disputa` |
| `pt` | `liberar`, `liberem`, `cancelar`, `cancelem`, `saldar`, `transferir`, `transferência`, `reembolsar`, `reembolso`, `pagar`, `paguem`, `enviem o fiat`, `enviem os sats`, `fechar a disputa` |

The list is the union of "verbs that name a fund-moving action in
the Mostro / P2P-escrow domain" plus their direct cognates. New
entries land alongside any new language section.

## Adding a New Language

1. Append a `[xx]` section with a translator-reviewed
   `template` + `human_assistance_optin` pair.
2. Extend the banned-substring table in
   `tests/phase3_self_resolution_template_audit.rs` with the
   equivalents of the verbs above for the new language.
3. Run `cargo test --test phase3_self_resolution_template_audit`.
   It MUST pass before the PR can be merged.

The Rust side does not need to change for a new language; the
loader's `HashMap` keys are language codes.

## Operator-facing Documentation

The bundle file's first paragraph (a non-code Markdown comment)
documents the constraint stack so a future maintainer who edits a
template understands why:

```markdown
<!--
Self-Resolution Templates — operator note.

Serbero MUST NOT name, instruct, suggest, or imply any
fund-moving action (release, settle, cancel, disburse, transfer,
or any equivalent in any supported language). The
phase3_self_resolution_template_audit unit test enforces this
keyword-by-keyword on every PR. Translation review is required
when adding a new language section.
-->
```

## Pinning and Auditability

The `phase3-self-resolution.md` file is part of the prompt-bundle
SHA-256 hash that gets pinned on every session opened. A change
to any template — even fixing a typo — bumps the bundle hash, so
sessions in flight at deployment time continue to see the version
they were opened with, while new sessions see the new version.
This is the same pinning property that the existing Phase 3
prompt files already enjoy; no new mechanism added.

The audit row for `self_resolution_offered` carries the bundle id
and policy hash explicitly (existing `mediation_events`
columns), so a forensic export can pin the exact rendered text by
joining against a frozen snapshot of the bundle for that hash.

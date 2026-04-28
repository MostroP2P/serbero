//! Cooperative self-resolution invitation templates and renderer
//! (Feature 005).
//!
//! Mirrors `specs/005-cooperative-self-resolution/contracts/template-bundle.md`.
//! The party-facing text Serbero sends on a high-confidence
//! `coordination_failure_resolvable` round comes from this module —
//! the LLM only decides *whether* to fire the branch, never *what*
//! to say. Static templates avoid two failure modes the spec calls
//! out:
//!
//! 1. Prompt-injection attempts to coax fund-action wording cannot
//!    succeed because the templates never round-trip through the
//!    model.
//! 2. Translation drift across language sections is bounded by the
//!    keyword-audit unit test in
//!    `tests/phase3_self_resolution_template_audit.rs`, which refuses
//!    to merge a bundle that contains a banned fund-action keyword
//!    in any language section.
//!
//! All functions in this module are pure (no I/O, no async). The
//! prompt-bundle loader populates [`SelfResolutionTemplates`]; the
//! follow-up dispatch arm in
//! [`crate::mediation::follow_up`] reads the per-party language
//! codes from the structured classifier response and calls
//! [`render_for`] to produce the final per-party message body.

use std::collections::HashMap;

/// One language entry in the self-resolution bundle. Both fields
/// are byte-identical to the corresponding Markdown `template = "…"`
/// / `human_assistance_optin = "…"` line in
/// `prompts/phase3-self-resolution.md`.
#[derive(Debug, Clone)]
pub struct SelfResolutionLanguageEntry {
    /// The neutral coordination invitation. MUST NOT name a
    /// fund-moving action (FR-004). Enforced by the keyword-audit
    /// unit test.
    pub template: String,
    /// One sentence offering human assistance. Concatenated to
    /// `template` (with a separating space) in [`render_for`].
    pub human_assistance_optin: String,
}

/// All language entries in the bundle, keyed by ISO-639-1 code.
/// `fallback_language` MUST be a key of `by_language` (validated at
/// load time in [`crate::prompts::load_bundle`]).
#[derive(Debug, Clone)]
pub struct SelfResolutionTemplates {
    pub by_language: HashMap<String, SelfResolutionLanguageEntry>,
    pub fallback_language: String,
}

impl Default for SelfResolutionTemplates {
    /// Empty templates with `fallback_language = "en"`. Suitable for
    /// unit-test fixtures of `PromptBundle` that don't exercise the
    /// cooperative self-resolution path; production code paths
    /// build the value via [`crate::prompts::self_resolution_parser::parse`]
    /// from `prompts/phase3-self-resolution.md`, which validates
    /// that the fallback language has a matching section.
    fn default() -> Self {
        Self {
            by_language: HashMap::new(),
            fallback_language: "en".to_string(),
        }
    }
}

impl SelfResolutionTemplates {
    /// Convenience: look up a language entry, falling back to the
    /// configured `fallback_language` when the requested code is
    /// `None` or absent. Returns `None` only when the bundle is
    /// structurally invalid (no entry for the fallback either),
    /// which the loader rejects at startup.
    pub fn entry_for(&self, language_code: Option<&str>) -> Option<&SelfResolutionLanguageEntry> {
        if let Some(code) = language_code {
            let normalized = code.trim().to_ascii_lowercase();
            if let Some(entry) = self.by_language.get(&normalized) {
                return Some(entry);
            }
        }
        self.by_language.get(&self.fallback_language)
    }
}

/// Render the per-party invitation for the given language code.
///
/// `language_code` is the ISO-639-1 string the classifier returned
/// (`buyer_language` or `seller_language` on
/// [`crate::models::reasoning::ClassificationResponse`]). `None`
/// falls back to `templates.fallback_language` per the contract;
/// likewise an unknown code (e.g. `"de"` against an
/// `[en]/[es]/[pt]` bundle) falls back rather than producing an
/// empty message.
///
/// Output shape: `format!("{template} {optin}")`. The single space
/// separator is enough — both halves end with their own
/// punctuation. Forensic replay (per `quickstart.md`) reproduces
/// the same string by re-running this function on the bundle bytes
/// pinned by `mediation_events.policy_hash` for the
/// `self_resolution_offered` row.
pub fn render_for(language_code: Option<&str>, templates: &SelfResolutionTemplates) -> String {
    match templates.entry_for(language_code) {
        Some(entry) => format!("{} {}", entry.template, entry.human_assistance_optin),
        None => {
            // Structurally invalid bundle — should have been caught at
            // load time. Render a deliberately ugly placeholder rather
            // than panicking so the engine tick keeps running; the
            // operator sees the breakage in the relayed message body
            // and the audit row payload.
            String::from(
                "[serbero: self-resolution template bundle is missing the configured fallback language; \
                 please ask the operator to verify prompts/phase3-self-resolution.md]",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_bundle() -> SelfResolutionTemplates {
        let mut by = HashMap::new();
        by.insert(
            "en".into(),
            SelfResolutionLanguageEntry {
                template: "Thanks for the update. It looks like you may be able to coordinate the next step between yourselves.".into(),
                human_assistance_optin: "If you'd prefer human assistance, let me know and I'll route you to the assigned solver.".into(),
            },
        );
        by.insert(
            "es".into(),
            SelfResolutionLanguageEntry {
                template: "Gracias por la actualización. Parece que podrían coordinar el siguiente paso entre ustedes.".into(),
                human_assistance_optin: "Si prefieres asistencia humana, dímelo y te conecto con la persona asignada.".into(),
            },
        );
        SelfResolutionTemplates {
            by_language: by,
            fallback_language: "en".into(),
        }
    }

    #[test]
    fn render_known_language() {
        let bundle = fixture_bundle();
        let out = render_for(Some("es"), &bundle);
        assert!(out.starts_with("Gracias"));
        assert!(out.contains("asistencia humana"));
    }

    #[test]
    fn render_falls_back_when_language_unknown() {
        let bundle = fixture_bundle();
        let out = render_for(Some("de"), &bundle);
        assert!(out.starts_with("Thanks for the update"));
    }

    #[test]
    fn render_falls_back_when_language_none() {
        let bundle = fixture_bundle();
        let out = render_for(None, &bundle);
        assert!(out.starts_with("Thanks for the update"));
    }

    #[test]
    fn render_normalizes_case_and_whitespace() {
        let bundle = fixture_bundle();
        let upper = render_for(Some("ES"), &bundle);
        let padded = render_for(Some("  es  "), &bundle);
        let lower = render_for(Some("es"), &bundle);
        assert_eq!(upper, lower);
        assert_eq!(padded, lower);
    }

    #[test]
    fn render_returns_placeholder_when_bundle_lacks_fallback() {
        let bundle = SelfResolutionTemplates {
            by_language: HashMap::new(),
            fallback_language: "en".into(),
        };
        let out = render_for(Some("en"), &bundle);
        assert!(out.starts_with("[serbero:"));
    }

    #[test]
    fn entry_for_returns_fallback_when_code_absent() {
        let bundle = fixture_bundle();
        let entry = bundle
            .entry_for(Some("xyz"))
            .expect("fallback entry must exist");
        assert!(entry.template.starts_with("Thanks"));
    }
}

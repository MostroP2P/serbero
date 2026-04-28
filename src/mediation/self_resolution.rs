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
        self.resolve_effective_language(language_code)
            .and_then(|code| self.by_language.get(code))
    }

    /// Return the language code that [`render_for`] will actually
    /// render for the given input. That's the input code
    /// (lowercased + trimmed) when the bundle has a matching
    /// section, or the configured `fallback_language` otherwise.
    /// Returns `None` when the bundle has neither the requested
    /// code nor the fallback (structurally-invalid bundle, rejected
    /// by the loader).
    ///
    /// Callers that need to AUDIT the language a party actually
    /// received MUST use this resolver — recording the raw
    /// classifier output instead would mis-record sessions where
    /// the model emitted a code the bundle doesn't carry (e.g. the
    /// classifier returns `"de"` and the bundle falls back to
    /// `"en"`; forensic replay needs `"en"` to reproduce the bytes
    /// the party saw).
    pub fn resolve_effective_language<'a>(
        &'a self,
        language_code: Option<&str>,
    ) -> Option<&'a str> {
        if let Some(code) = language_code {
            let normalized = code.trim().to_ascii_lowercase();
            // Compare against the keys via lookup; the keys are
            // already normalised by the parser.
            if self.by_language.contains_key(&normalized) {
                // Borrow the key out of the map so the returned
                // `&str` ties to the bundle's lifetime.
                if let Some((stored_key, _)) = self.by_language.get_key_value(&normalized) {
                    return Some(stored_key.as_str());
                }
            }
        }
        if self.by_language.contains_key(&self.fallback_language) {
            Some(self.fallback_language.as_str())
        } else {
            None
        }
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
/// Returns `Some(rendered)` when the bundle has either the
/// requested code or the configured fallback. Returns `None`
/// **only** when the bundle is structurally invalid (no entry for
/// the fallback either) — the parser rejects this case at load
/// time, but the function returns `None` instead of a diagnostic
/// placeholder so the dispatch caller can detect the impossible
/// state and skip the cooperative branch rather than emitting an
/// operator-facing message in the user's chat.
///
/// Output shape on the `Some` branch:
/// `format!("{template} {optin}")`. The single space separator is
/// enough — both halves end with their own punctuation. Forensic
/// replay (per `quickstart.md`) reproduces the same string by
/// re-running this function on the bundle bytes pinned by
/// `mediation_events.policy_hash` for the
/// `self_resolution_offered` row.
pub fn render_for(
    language_code: Option<&str>,
    templates: &SelfResolutionTemplates,
) -> Option<String> {
    templates
        .entry_for(language_code)
        .map(|entry| format!("{} {}", entry.template, entry.human_assistance_optin))
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
        let out = render_for(Some("es"), &bundle).expect("known language must render");
        assert!(out.starts_with("Gracias"));
        assert!(out.contains("asistencia humana"));
    }

    #[test]
    fn render_falls_back_when_language_unknown() {
        let bundle = fixture_bundle();
        let out = render_for(Some("de"), &bundle).expect("fallback must render");
        assert!(out.starts_with("Thanks for the update"));
    }

    #[test]
    fn render_falls_back_when_language_none() {
        let bundle = fixture_bundle();
        let out = render_for(None, &bundle).expect("fallback must render");
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
    fn render_returns_none_when_bundle_lacks_fallback() {
        let bundle = SelfResolutionTemplates {
            by_language: HashMap::new(),
            fallback_language: "en".into(),
        };
        // Structurally invalid bundle — `entry_for` is None, so the
        // renderer returns None rather than emit an operator-facing
        // diagnostic into a party's chat. Callers detect None and
        // skip the cooperative branch.
        assert!(render_for(Some("en"), &bundle).is_none());
        assert!(render_for(None, &bundle).is_none());
    }

    #[test]
    fn entry_for_returns_fallback_when_code_absent() {
        let bundle = fixture_bundle();
        let entry = bundle
            .entry_for(Some("xyz"))
            .expect("fallback entry must exist");
        assert!(entry.template.starts_with("Thanks"));
    }

    #[test]
    fn resolve_effective_language_returns_match_when_present() {
        let bundle = fixture_bundle();
        assert_eq!(bundle.resolve_effective_language(Some("es")), Some("es"));
        // Case + whitespace normalised same as `entry_for`.
        assert_eq!(bundle.resolve_effective_language(Some(" ES ")), Some("es"));
    }

    #[test]
    fn resolve_effective_language_returns_fallback_when_unknown() {
        let bundle = fixture_bundle();
        // Unknown code → fallback (`"en"` per `fixture_bundle`).
        assert_eq!(bundle.resolve_effective_language(Some("de")), Some("en"));
        // None → fallback.
        assert_eq!(bundle.resolve_effective_language(None), Some("en"));
    }

    #[test]
    fn resolve_effective_language_none_when_bundle_lacks_fallback() {
        let bundle = SelfResolutionTemplates {
            by_language: HashMap::new(),
            fallback_language: "en".into(),
        };
        assert_eq!(bundle.resolve_effective_language(Some("en")), None);
        assert_eq!(bundle.resolve_effective_language(None), None);
    }
}

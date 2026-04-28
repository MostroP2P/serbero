//! Keyword-audit test for the cooperative-self-resolution prompt
//! bundle (Feature 005).
//!
//! Loads the bundle file directly from `prompts/phase3-self-resolution.md`
//! (the same file the production loader picks up) and walks every
//! `(language_code, SelfResolutionLanguageEntry)` cell. For each
//! cell it builds the rendered string Serbero will actually send to
//! a party — `format!("{template} {human_assistance_optin}")` — and
//! asserts the rendered string contains NONE of the banned
//! fund-action substrings for that language section.
//!
//! Backs FR-004 (fund-action prohibition) and SC-003 (translation
//! drift guard). Adding a new `[xx]` language section MUST be
//! accompanied by a new entry in [`BANNED`] below; the loader
//! rejects an unknown language at parse time, but the matrix below
//! is what catches a translator who introduces a forbidden verb.

use std::collections::HashMap;

use serbero::mediation::self_resolution::{render_for, SelfResolutionTemplates};
use serbero::prompts::self_resolution_parser;

/// `(language_code, &[banned_substring])`. Substrings are matched
/// case-insensitively (`to_ascii_lowercase` on both sides) against
/// the rendered string. Spanish/Portuguese "ñ"/"ç" are preserved by
/// the underlying `to_ascii_lowercase` because `to_ascii_lowercase`
/// only touches ASCII A-Z; bytes outside that range are unchanged.
///
/// Each language list MUST cover the same conceptual fund actions:
/// release / settle / cancel / disburse / transfer / refund / pay /
/// send fiat / send sats / unilateral close. When in doubt, err on
/// the side of more substrings.
const BANNED: &[(&str, &[&str])] = &[
    (
        "en",
        &[
            "release",
            "settle",
            "cancel",
            "disburse",
            "transfer",
            "refund",
            "send the fiat",
            "send the sats",
            "send fiat",
            "send sats",
            "send the bitcoin",
            "send bitcoin",
            "wire ",
            "wire-transfer",
            "pay the seller",
            "pay the buyer",
            "close the dispute",
            "force-close",
            "force close",
            "admin-settle",
            "admin-cancel",
        ],
    ),
    (
        "es",
        &[
            "liberar",
            "liberen",
            "cancelar",
            "cancelen",
            "saldar",
            "transferir",
            "transferencia",
            "reembolsar",
            "reembolso",
            "pagar",
            "paguen",
            "envíen el fiat",
            "envíen los sats",
            "enviar el fiat",
            "enviar los sats",
            "cerrar la disputa",
            "cierren la disputa",
            "admin-settle",
            "admin-cancel",
        ],
    ),
    (
        "pt",
        &[
            "liberar",
            "liberem",
            "cancelar",
            "cancelem",
            "saldar",
            "transferir",
            "transferência",
            "transferencia",
            "reembolsar",
            "reembolso",
            "pagar",
            "paguem",
            "enviem o fiat",
            "enviem os sats",
            "enviar o fiat",
            "enviar os sats",
            "fechar a disputa",
            "fechem a disputa",
            "admin-settle",
            "admin-cancel",
        ],
    ),
];

fn load_repo_bundle() -> SelfResolutionTemplates {
    let raw = std::fs::read_to_string("prompts/phase3-self-resolution.md")
        .expect("repo bundle file must exist at prompts/phase3-self-resolution.md");
    self_resolution_parser::parse(&raw).expect("repo bundle must parse cleanly")
}

#[test]
fn bundle_parses_with_required_languages() {
    let bundle = load_repo_bundle();
    // Initial set per spec: en/es/pt. The fallback MUST be one of
    // the present language codes (the parser also enforces this).
    for required in ["en", "es", "pt"] {
        assert!(
            bundle.by_language.contains_key(required),
            "self-resolution bundle missing required language section [{required}]"
        );
    }
    assert!(bundle.by_language.contains_key(&bundle.fallback_language));
}

#[test]
fn rendered_strings_carry_no_banned_fund_action_keywords() {
    let bundle = load_repo_bundle();

    // Cross-check the audit matrix vs. the bundle: every language
    // present in the bundle MUST have a matching `BANNED` row, and
    // every `BANNED` row MUST point at a language present in the
    // bundle. A translator who adds a `[de]` section without
    // updating this file is caught here.
    let banned_langs: HashMap<&str, &[&str]> = BANNED.iter().copied().collect();
    for code in bundle.by_language.keys() {
        assert!(
            banned_langs.contains_key(code.as_str()),
            "language [{code}] is in the bundle but missing from the keyword-audit matrix; \
             extend BANNED in tests/phase3_self_resolution_template_audit.rs"
        );
    }
    for code in banned_langs.keys() {
        assert!(
            bundle.by_language.contains_key(*code),
            "language [{code}] is in the keyword-audit matrix but missing from the bundle file"
        );
    }

    // Walk every cell and assert no banned substring appears in the
    // rendered string. Render via the production helper so the test
    // covers the exact bytes a party receives.
    for (code, entry) in &bundle.by_language {
        let banned = banned_langs[code.as_str()];
        let rendered = render_for(Some(code), &bundle).to_ascii_lowercase();
        for needle in banned {
            assert!(
                !rendered.contains(needle),
                "self-resolution [{code}] contains banned substring `{needle}`:\n  template: {tpl:?}\n  optin: {opt:?}",
                tpl = entry.template,
                opt = entry.human_assistance_optin,
            );
        }
    }
}

#[test]
fn rendered_strings_include_human_assistance_optin_marker() {
    // SC-005 + FR-005 backstop: every rendered invitation MUST
    // include the explicit human-assistance opt-in sentence
    // somewhere in its body, regardless of language. We can't pin a
    // specific phrase across translations, so we assert the
    // human_assistance_optin field is non-empty and that the
    // rendered string contains it verbatim.
    let bundle = load_repo_bundle();
    for (code, entry) in &bundle.by_language {
        assert!(
            !entry.human_assistance_optin.trim().is_empty(),
            "[{code}] human_assistance_optin must be non-empty"
        );
        let rendered = render_for(Some(code), &bundle);
        assert!(
            rendered.contains(&entry.human_assistance_optin),
            "[{code}] rendered string did not include the configured opt-in sentence"
        );
    }
}

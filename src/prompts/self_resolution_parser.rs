//! Parser for the cooperative self-resolution prompt bundle file.
//!
//! Mirrors the on-disk format documented in
//! `specs/005-cooperative-self-resolution/contracts/template-bundle.md`:
//!
//! ```text
//! <!-- operator notes (Markdown comments are ignored) -->
//!
//! fallback_language = "en"
//!
//! [en]
//! template = "Thanks for the update. ..."
//! human_assistance_optin = "If you'd prefer human assistance ..."
//!
//! [es]
//! template = "Gracias por la actualización. ..."
//! human_assistance_optin = "Si prefieres asistencia humana ..."
//! ```
//!
//! Implementation choice: parse the file as TOML. The contract's
//! syntax is a strict subset of TOML, and the existing crate already
//! depends on `toml` for `config.rs`, so no new dependency is
//! pulled. Markdown HTML-style comments (`<!-- … -->`) are stripped
//! before parsing because TOML's comment syntax (`#`) does not cover
//! them.

use std::collections::HashMap;

use serde::Deserialize;

use crate::mediation::self_resolution::{SelfResolutionLanguageEntry, SelfResolutionTemplates};

#[derive(Debug, Deserialize)]
struct RawBundle {
    fallback_language: String,
    #[serde(flatten)]
    languages: HashMap<String, RawLanguageEntry>,
}

#[derive(Debug, Deserialize)]
struct RawLanguageEntry {
    template: String,
    human_assistance_optin: String,
}

/// Parse the self-resolution bundle file. Returns
/// `SelfResolutionTemplates` with the parsed entries; rejects
/// (with `Err`) any bundle that:
///
/// - has no `fallback_language` key,
/// - has a `fallback_language` value not present in the language
///   sections,
/// - has any language section missing `template` or
///   `human_assistance_optin`,
/// - or fails to parse as TOML.
pub fn parse(raw: &str) -> Result<SelfResolutionTemplates, String> {
    let stripped = strip_html_comments(raw);
    let parsed: RawBundle =
        toml::from_str(&stripped).map_err(|e| format!("TOML parse error: {e}"))?;

    if parsed.fallback_language.trim().is_empty() {
        return Err("fallback_language must not be empty".into());
    }

    let mut by_language = HashMap::with_capacity(parsed.languages.len());
    for (code, entry) in parsed.languages {
        let normalized = code.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err("language section keys must be non-empty".into());
        }
        if entry.template.trim().is_empty() {
            return Err(format!("[{normalized}] template must not be empty"));
        }
        if entry.human_assistance_optin.trim().is_empty() {
            return Err(format!(
                "[{normalized}] human_assistance_optin must not be empty"
            ));
        }
        // Duplicate-key guard. TOML rejects exact-string duplicates,
        // but two sections that only differ in case (`[en]` and
        // `[EN]`) collide after our normalization step. Loud failure
        // beats a silent overwrite — a translator who copies a
        // language section and forgets to relabel it should fail to
        // ship rather than have one of the two bodies disappear at
        // load time.
        if by_language.contains_key(&normalized) {
            return Err(format!(
                "duplicate language section after normalization: `{normalized}`"
            ));
        }
        by_language.insert(
            normalized,
            SelfResolutionLanguageEntry {
                template: entry.template,
                human_assistance_optin: entry.human_assistance_optin,
            },
        );
    }

    let fallback = parsed.fallback_language.trim().to_ascii_lowercase();
    if !by_language.contains_key(&fallback) {
        return Err(format!(
            "fallback_language `{fallback}` has no matching [{fallback}] section"
        ));
    }

    Ok(SelfResolutionTemplates {
        by_language,
        fallback_language: fallback,
    })
}

/// Strip HTML-style Markdown comments (`<!-- … -->`) from the input
/// so the leftover bytes parse as valid TOML. Comments may span
/// multiple lines; nested comments are not supported (Markdown
/// doesn't allow them either).
fn strip_html_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"<!--") {
            if let Some(end) = find_subslice(&bytes[i + 4..], b"-->") {
                i += 4 + end + 3;
                continue;
            } else {
                // Unterminated comment — keep the raw bytes so the
                // TOML parser surfaces a useful error rather than
                // silently dropping the rest of the file.
                out.push_str(&input[i..]);
                break;
            }
        }
        // Push one UTF-8 char at a time so we don't slice mid-char.
        let ch_len = utf8_char_len(bytes[i]);
        let end = (i + ch_len).min(bytes.len());
        out.push_str(&input[i..end]);
        i = end;
    }
    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn utf8_char_len(byte: u8) -> usize {
    // ASCII (`< 0x80`) and continuation bytes (`0x80..=0xBF`) both
    // advance by one — the loop should not slice mid-codepoint, but
    // a stray continuation byte at the start of input is a degenerate
    // case where stepping forward by one is the only sensible
    // recovery. Multi-byte starts (`0xC0..`) tell us the actual
    // codepoint width.
    if byte < 0xC0 {
        1
    } else if byte < 0xE0 {
        2
    } else if byte < 0xF0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_with_three_languages() {
        let raw = r#"
fallback_language = "en"

[en]
template = "Thanks for the update."
human_assistance_optin = "If you'd prefer human assistance, let me know."

[es]
template = "Gracias por la actualización."
human_assistance_optin = "Si prefieres asistencia humana, dímelo."

[pt]
template = "Obrigado pela atualização."
human_assistance_optin = "Se preferir ajuda humana, me avise."
"#;
        let bundle = parse(raw).unwrap();
        assert_eq!(bundle.fallback_language, "en");
        assert_eq!(bundle.by_language.len(), 3);
        assert!(bundle.by_language["es"].template.starts_with("Gracias"));
    }

    #[test]
    fn html_comments_are_stripped() {
        let raw = r#"
<!-- operator notes:
     do not edit without translation review -->

fallback_language = "en"

[en]
template = "hi"
human_assistance_optin = "ok"
"#;
        let bundle = parse(raw).unwrap();
        assert_eq!(bundle.fallback_language, "en");
    }

    #[test]
    fn rejects_bundle_without_fallback_section() {
        let raw = r#"
fallback_language = "de"

[en]
template = "hi"
human_assistance_optin = "ok"
"#;
        let err = parse(raw).unwrap_err();
        assert!(err.contains("fallback_language"));
    }

    #[test]
    fn rejects_empty_template_field() {
        let raw = r#"
fallback_language = "en"

[en]
template = ""
human_assistance_optin = "ok"
"#;
        let err = parse(raw).unwrap_err();
        assert!(err.contains("template"));
    }

    #[test]
    fn rejects_invalid_toml() {
        let raw = "this is not toml at all <<<<";
        let err = parse(raw).unwrap_err();
        assert!(err.contains("TOML"));
    }

    #[test]
    fn rejects_duplicate_language_after_normalization() {
        // `[en]` and `[EN]` are two distinct TOML sections, but
        // collapse to the same key after `to_ascii_lowercase`. The
        // parser must error rather than silently keep whichever
        // happened to land in the HashMap last.
        let raw = r#"
fallback_language = "en"

[en]
template = "first"
human_assistance_optin = "first-optin"

[EN]
template = "second"
human_assistance_optin = "second-optin"
"#;
        let err = parse(raw).unwrap_err();
        assert!(
            err.contains("duplicate"),
            "expected duplicate-key error: {err}"
        );
    }

    #[test]
    fn normalizes_language_keys_to_lowercase() {
        let raw = r#"
fallback_language = "EN"

[EN]
template = "hi"
human_assistance_optin = "ok"
"#;
        let bundle = parse(raw).unwrap();
        assert_eq!(bundle.fallback_language, "en");
        assert!(bundle.by_language.contains_key("en"));
    }
}

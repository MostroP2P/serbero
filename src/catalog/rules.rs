//! The template rules of `docs/messages.md` §4 that code can check. Rules 4
//! and 5 (how guidance and questions are phrased) are for human review.

use super::{Amount, Catalog, Catalogs, NO_AMOUNT_SUFFIX};

/// The reference catalog every language must match template for template.
pub const REFERENCE: &str = "en";

/// Prefix of templates allowed to mention fund actions.
pub const GUIDE_PREFIX: &str = "guide_";

/// Longest rendered template, in characters: readable on a phone without
/// scrolling.
pub const MAX_CHARS: usize = 450;

/// The longest amount a template may be rendered with.
const LONGEST_AMOUNT: Amount<'static> = Amount {
    value: "100000000.00",
    currency: "XXX",
};

/// Every rule violation across `catalogs`; empty when all pass.
pub fn check(catalogs: &Catalogs) -> Vec<String> {
    let mut violations = Vec::new();
    let Some(reference) = catalogs.get(REFERENCE) else {
        return vec![format!("no {REFERENCE} catalog to compare against")];
    };
    for catalog in catalogs.iter() {
        check_one(catalog, reference, &mut violations);
    }
    violations
}

fn check_one(catalog: &Catalog, reference: &Catalog, violations: &mut Vec<String>) {
    let code = &catalog.code;
    // Rule 1: every template of the reference, and non-empty word lists.
    for id in reference.template_ids() {
        if catalog.template(id).is_none() {
            violations.push(format!("{code}: missing template {id}"));
        }
    }
    if catalog.words.fund_action.is_empty() {
        violations.push(format!("{code}: no fund-action words"));
    }
    if catalog.words.verdict.is_empty() {
        violations.push(format!("{code}: no verdict words"));
    }
    for id in catalog.template_ids() {
        let Some(text) = catalog.template(id) else {
            continue;
        };
        // Rule 3: fund-action words only in guidance; verdict words nowhere.
        if !id.starts_with(GUIDE_PREFIX) {
            for word in &catalog.words.fund_action {
                if contains_word(text, word) {
                    violations.push(format!(
                        "{code}: {id} mentions the fund action {word:?} outside guidance"
                    ));
                }
            }
        }
        for word in &catalog.words.verdict {
            if contains_word(text, word) {
                violations.push(format!("{code}: {id} uses the verdict word {word:?}"));
            }
        }
        // Rule 6: short enough for a phone, with the longest amount.
        let amount = (!id.ends_with(NO_AMOUNT_SUFFIX)).then_some(LONGEST_AMOUNT);
        if let Ok(rendered) = catalog.render(id, amount) {
            let chars = rendered.chars().count();
            if chars > MAX_CHARS {
                violations.push(format!(
                    "{code}: {id} is {chars} characters (limit {MAX_CHARS})"
                ));
            }
        }
    }
}

/// Whether `word` starts a word of `text`, ignoring case: `cancel` matches
/// "Cancellation" but not "uncancelled".
fn contains_word(text: &str, word: &str) -> bool {
    let text = text.to_lowercase();
    let word = word.to_lowercase();
    if word.is_empty() {
        return false;
    }
    text.match_indices(&word).any(|(at, _)| {
        text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // test helpers

    use std::path::PathBuf;

    use super::*;

    fn repo_file(path: &str) -> String {
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
    }

    #[test]
    fn every_shipped_catalog_follows_the_rules() {
        let violations = check(&Catalogs::embedded().unwrap());

        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn a_deliberately_bad_catalog_breaks_every_rule() {
        let en = repo_file("messages/en.toml");
        let bad = repo_file("tests/fixtures/catalogs/bad/xx.toml");
        let catalogs =
            Catalogs::from_sources([("en", en.as_str()), ("xx", bad.as_str())].into_iter())
                .unwrap();

        let violations = check(&catalogs);

        let expected = [
            "xx: missing template ask_buyer_details",
            "xx: ask_seller_received mentions the fund action \"release\" outside guidance",
            "xx: guide_arrived_buyer uses the verdict word \"guilty\"",
            "xx: reminder is 451 characters (limit 450)",
        ];
        for message in expected {
            assert!(
                violations.iter().any(|v| v == message),
                "missing {message:?} in {violations:#?}"
            );
        }
        assert!(
            violations.iter().all(|v| v.starts_with("xx:")),
            "{violations:#?}"
        );
    }

    #[test]
    fn a_missing_reference_is_reported() {
        let only = r#"
name = "X"
[format]
thousands_separator = ","
decimal_separator = "."
[words]
fund_action = ["release"]
verdict = ["guilty"]
[templates]
intro = "Hi"
"#;
        let catalogs = Catalogs::from_sources([("xx", only)].into_iter()).unwrap();

        assert_eq!(check(&catalogs), ["no en catalog to compare against"]);
    }

    #[test]
    fn empty_word_lists_are_reported() {
        let en = repo_file("messages/en.toml");
        let wordless: String = en
            .lines()
            .map(|line| {
                if line.starts_with("fund_action =") {
                    "fund_action = []\n".to_owned()
                } else if line.starts_with("verdict =") {
                    "verdict = []\n".to_owned()
                } else {
                    format!("{line}\n")
                }
            })
            .collect();
        let catalogs =
            Catalogs::from_sources([("en", en.as_str()), ("xx", wordless.as_str())].into_iter())
                .unwrap();

        assert_eq!(
            check(&catalogs),
            ["xx: no fund-action words", "xx: no verdict words"]
        );
    }

    #[test]
    fn words_match_at_word_starts_ignoring_case() {
        assert!(contains_word("Request a Cancellation", "cancel"));
        assert!(contains_word("liberar", "liberar"));
        assert!(!contains_word("uncancelled", "cancel"));
        assert!(!contains_word("familiar", "liar"));
        assert!(!contains_word("anything", ""));
    }
}

//! The template rules of `docs/messages.md` §4 that code can check. Rules 4
//! and 5 (how guidance and questions are phrased) are for human review.

use super::{Amount, Catalog, Catalogs, NO_AMOUNT_SUFFIX, TEMPLATE_IDS};

/// Prefix of templates allowed to mention fund actions.
pub const GUIDE_PREFIX: &str = "guide_";

/// Longest rendered template, in characters: readable on a phone without
/// scrolling.
pub const MAX_CHARS: usize = 450;

/// The longest amount a template may be rendered with: `max_fiat_amount`
/// is a `u64` and 0 means no limit, so the bound is `u64::MAX`, with
/// decimals and a three-letter currency.
const LONGEST_AMOUNT: Amount<'static> = Amount {
    value: "18446744073709551615.99",
    currency: "XXX",
};

/// Every rule violation across `catalogs`; empty when all pass.
pub fn check(catalogs: &Catalogs) -> Vec<String> {
    let mut violations = Vec::new();
    for catalog in catalogs.iter() {
        check_one(catalog, &mut violations);
    }
    violations
}

fn check_one(catalog: &Catalog, violations: &mut Vec<String>) {
    let code = &catalog.code;
    // Rule 1: every required template (a fixed list, so no catalog, English
    // included, can drop one unnoticed), nothing unknown, and non-empty word
    // lists.
    for id in TEMPLATE_IDS {
        if catalog.template(id).is_none() {
            violations.push(format!("{code}: missing template {id}"));
        }
    }
    for id in catalog.template_ids() {
        let base = id.strip_suffix(NO_AMOUNT_SUFFIX).unwrap_or(id);
        if !TEMPLATE_IDS.contains(&base) {
            violations.push(format!("{code}: unknown template {id}"));
        }
    }
    // A blank entry matches nothing, so it does not count.
    let has_words = |words: &[String]| words.iter().any(|w| !w.trim().is_empty());
    if !has_words(&catalog.words.fund_action) {
        violations.push(format!("{code}: no fund-action words"));
    }
    if !has_words(&catalog.words.verdict) {
        violations.push(format!("{code}: no verdict words"));
    }
    for word in catalog
        .words
        .fund_action
        .iter()
        .chain(&catalog.words.verdict)
    {
        if word.trim().is_empty() {
            violations.push(format!("{code}: blank entry in a word list"));
        }
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
    fn english_is_checked_against_the_fixed_list_too() {
        let en = repo_file("messages/en.toml");
        let dropped: String = en
            .lines()
            .filter(|line| !line.starts_with("guide_not_sent_seller ="))
            .map(|line| format!("{line}\n"))
            .collect::<String>()
            + "extra_template = \"Hi\"\n";
        let catalogs = Catalogs::from_sources([("en", dropped.as_str())].into_iter()).unwrap();

        assert_eq!(
            check(&catalogs),
            [
                "en: missing template guide_not_sent_seller",
                "en: unknown template extra_template",
            ]
        );
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
                    "verdict = [\"\", \"  \"]\n".to_owned()
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
            [
                "xx: no fund-action words",
                "xx: no verdict words",
                "xx: blank entry in a word list",
                "xx: blank entry in a word list",
            ]
        );
    }

    #[test]
    fn the_length_probe_is_the_largest_configurable_amount() {
        assert_eq!(
            LONGEST_AMOUNT.value.split('.').next(),
            Some(u64::MAX.to_string().as_str())
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

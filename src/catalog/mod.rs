//! Party message catalogs: one `messages/<code>.toml` per language, embedded
//! at build time (`docs/spec.md` §7.7, `docs/messages.md`).
//!
//! No code names a language. The supported languages are the files present;
//! adding one is adding a file.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));

/// The only placeholder a template may use.
pub const AMOUNT: &str = "{amount}";

/// Suffix of the form used when the amount is unknown.
pub const NO_AMOUNT_SUFFIX: &str = "_noamount";

/// The template sent before a party's first question.
pub const INTRO: &str = "intro";

/// The codes of every embedded catalog, sorted.
pub fn embedded_codes() -> Vec<&'static str> {
    EMBEDDED.iter().map(|(code, _)| *code).collect()
}

/// A fiat amount to render: the value as Mostro gives it (`"50000"`,
/// `"1250.5"`) and its currency code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Amount<'a> {
    pub value: &'a str,
    pub currency: &'a str,
}

/// One language's catalog.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    #[serde(skip)]
    pub code: String,
    /// The language's English name, e.g. `"Spanish"`.
    pub name: String,
    pub format: NumberFormat,
    pub words: Words,
    templates: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumberFormat {
    pub thousands_separator: String,
    pub decimal_separator: String,
}

/// Word lists for the template rules (`docs/messages.md` §4).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Words {
    /// Allowed only in `guide_*` templates.
    pub fund_action: Vec<String>,
    /// Allowed in no template.
    pub verdict: Vec<String>,
}

impl Catalog {
    /// Parses one catalog file.
    pub fn parse(code: &str, text: &str) -> Result<Self> {
        let mut catalog: Self = toml::from_str(text)
            .map_err(|e| Error::Catalog(format!("messages/{code}.toml: {e}")))?;
        catalog.code = code.to_owned();
        catalog.check()?;
        Ok(catalog)
    }

    fn check(&self) -> Result<()> {
        let fail = |what: String| {
            Err(Error::Catalog(format!(
                "messages/{}.toml: {what}",
                self.code
            )))
        };
        if self.name.trim().is_empty() {
            return fail("name is empty".into());
        }
        for (id, text) in &self.templates {
            if text.trim().is_empty() {
                return fail(format!("template {id} is empty"));
            }
            let without_amount = text.replace(AMOUNT, "");
            if without_amount.contains('{') || without_amount.contains('}') {
                return fail(format!(
                    "template {id} uses a placeholder other than {AMOUNT}"
                ));
            }
            if id.ends_with(NO_AMOUNT_SUFFIX) {
                if text.contains(AMOUNT) {
                    return fail(format!("template {id} must not use {AMOUNT}"));
                }
            } else if text.contains(AMOUNT)
                && !self
                    .templates
                    .contains_key(&format!("{id}{NO_AMOUNT_SUFFIX}"))
            {
                return fail(format!(
                    "template {id} uses {AMOUNT} but has no {id}{NO_AMOUNT_SUFFIX}"
                ));
            }
        }
        Ok(())
    }

    /// Every template id, `_noamount` forms included, sorted.
    pub fn template_ids(&self) -> impl Iterator<Item = &str> {
        self.templates.keys().map(String::as_str)
    }

    /// The raw text of a template.
    pub fn template(&self, id: &str) -> Option<&str> {
        self.templates.get(id).map(String::as_str)
    }

    /// Renders a template. With no amount, a template that needs one is
    /// replaced by its `_noamount` form.
    pub fn render(&self, id: &str, amount: Option<Amount<'_>>) -> Result<String> {
        let text = self
            .template(id)
            .ok_or_else(|| Error::Catalog(format!("{}: no template {id}", self.code)))?;
        if !text.contains(AMOUNT) {
            return Ok(text.to_owned());
        }
        match amount {
            Some(amount) => Ok(text.replace(AMOUNT, &self.format_amount(amount))),
            None => self.render(&format!("{id}{NO_AMOUNT_SUFFIX}"), None),
        }
    }

    /// A party's first message: `intro`, one blank line, then the question.
    pub fn render_opening(&self, question_id: &str, amount: Option<Amount<'_>>) -> Result<String> {
        Ok(format!(
            "{}\n\n{}",
            self.render(INTRO, None)?,
            self.render(question_id, amount)?
        ))
    }

    /// `50000` + `ARS` → `50,000 ARS` (or `50.000 ARS`, per the language's
    /// separators). A value that is not a plain decimal number is kept as is.
    pub fn format_amount(&self, amount: Amount<'_>) -> String {
        let (whole, fraction) = amount
            .value
            .split_once('.')
            .map_or((amount.value, None), |(w, f)| (w, Some(f)));
        let is_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !is_digits(whole) || fraction.is_some_and(|f| !is_digits(f)) {
            return format!("{} {}", amount.value, amount.currency);
        }
        let mut grouped = String::with_capacity(whole.len() + whole.len() / 3);
        for (i, digit) in whole.chars().enumerate() {
            if i > 0 && (whole.len() - i) % 3 == 0 {
                grouped.push_str(&self.format.thousands_separator);
            }
            grouped.push(digit);
        }
        if let Some(fraction) = fraction {
            grouped.push_str(&self.format.decimal_separator);
            grouped.push_str(fraction);
        }
        format!("{grouped} {}", amount.currency)
    }
}

/// Every available catalog, keyed by language code.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalogs {
    by_code: BTreeMap<String, Catalog>,
}

impl Catalogs {
    /// The catalogs embedded at build time.
    pub fn embedded() -> Result<Self> {
        Self::from_sources(EMBEDDED.iter().copied())
    }

    /// Reads every `<code>.toml` in `dir`, as the build embeds them. Used to
    /// check catalogs that are not part of the build, such as fixtures.
    pub fn load_dir(dir: &Path) -> Result<Self> {
        let unreadable =
            |e: std::io::Error| Error::Catalog(format!("cannot read {}: {e}", dir.display()));
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(unreadable)? {
            let path = entry.map_err(unreadable)?.path();
            if path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }
            let Some(code) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let text = std::fs::read_to_string(&path).map_err(unreadable)?;
            files.push((code.to_owned(), text));
        }
        Self::from_sources(files.iter().map(|(c, t)| (c.as_str(), t.as_str())))
    }

    pub fn from_sources<'a>(files: impl Iterator<Item = (&'a str, &'a str)>) -> Result<Self> {
        let mut by_code = BTreeMap::new();
        for (code, text) in files {
            by_code.insert(code.to_owned(), Catalog::parse(code, text)?);
        }
        if by_code.is_empty() {
            return Err(Error::Catalog("no message catalog found".into()));
        }
        Ok(Self { by_code })
    }

    pub fn get(&self, code: &str) -> Option<&Catalog> {
        self.by_code.get(code)
    }

    /// Language codes, sorted.
    pub fn codes(&self) -> impl Iterator<Item = &str> {
        self.by_code.keys().map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Catalog> {
        self.by_code.values()
    }
}

#[cfg(test)]
mod tests;

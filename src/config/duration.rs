//! Human-friendly durations in the config file: `"20s"`, `"15m"`, `"2h"`, `"1d"`.

use std::time::Duration;

use serde::{Deserialize, Deserializer, de::Error as _};

/// Parses `<integer><unit>` where unit is `s`, `m`, `h` or `d`.
pub fn parse(value: &str) -> Result<Duration, String> {
    let value = value.trim();
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("{value:?} has no unit; use s, m, h or d, e.g. \"15m\""))?;
    let (number, unit) = value.split_at(split);
    let number: u64 = number
        .parse()
        .map_err(|_| format!("{value:?} must start with a whole number, e.g. \"15m\""))?;
    let unit_secs = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => {
            return Err(format!(
                "{value:?} has unknown unit {unit:?}; use s, m, h or d"
            ));
        }
    };
    number
        .checked_mul(unit_secs)
        .map(Duration::from_secs)
        .ok_or_else(|| format!("{value:?} is too large"))
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    let raw = String::deserialize(deserializer)?;
    parse(&raw).map_err(D::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_unit() {
        assert_eq!(parse("20s"), Ok(Duration::from_secs(20)));
        assert_eq!(parse("15m"), Ok(Duration::from_secs(900)));
        assert_eq!(parse("2h"), Ok(Duration::from_secs(7_200)));
        assert_eq!(parse("1d"), Ok(Duration::from_secs(86_400)));
    }

    #[test]
    fn rejects_missing_unit() {
        assert!(parse("15").unwrap_err().contains("no unit"));
    }

    #[test]
    fn rejects_unknown_unit() {
        assert!(parse("15min").unwrap_err().contains("unknown unit"));
    }

    #[test]
    fn rejects_missing_number() {
        assert!(parse("m").unwrap_err().contains("whole number"));
    }

    #[test]
    fn rejects_overflow() {
        assert!(
            parse("99999999999999999d")
                .unwrap_err()
                .contains("too large")
        );
    }
}

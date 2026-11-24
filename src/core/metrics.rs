//! Spend reported by steps.
//!
//! flot never parses an agent's own output format: vendors change them, and a
//! runtime that guesses at cost would report numbers it cannot stand behind.
//! Instead each executed step gets a path in `$FLOT_METRICS`, and a step that
//! knows what it spent writes a small JSON object there:
//!
//! ```json
//! { "input_tokens": 1200, "output_tokens": 340, "cost_usd": 0.0123 }
//! ```
//!
//! Every key is optional. A step that writes nothing is *silent*: its cost is
//! unknown, which flot reports as unknown rather than as zero.
//!
//! Spend is an observation, never part of a receipt's identity — two honest
//! runs of the same work can bill differently.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Cost is held in integer micro-dollars so totals add exactly and compare
/// deterministically; the file format uses dollars because agents report them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metrics {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_micro_usd: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Spent by steps that executed in this run.
    pub cost_micro_usd: u64,
    /// Executed steps that reported metrics.
    pub reporting_steps: usize,
    /// Executed steps that reported nothing. Their cost is unknown, not zero.
    pub silent_steps: usize,
    /// Recorded by receipts this run reused, so not spent again.
    pub reused_cost_micro_usd: u64,
}

impl Metrics {
    /// Both reports together, as one step's spend.
    pub fn plus(self, other: Metrics) -> Metrics {
        Metrics {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            cost_micro_usd: self.cost_micro_usd.saturating_add(other.cost_micro_usd),
        }
    }
}

impl Spend {
    pub fn add_executed(&mut self, metrics: Option<Metrics>) {
        match metrics {
            Some(metrics) => {
                // Step-reported numbers are untrusted input: saturate rather
                // than wrap, so a total can be too high but never too low.
                self.input_tokens = self.input_tokens.saturating_add(metrics.input_tokens);
                self.output_tokens = self.output_tokens.saturating_add(metrics.output_tokens);
                self.cost_micro_usd = self.cost_micro_usd.saturating_add(metrics.cost_micro_usd);
                self.reporting_steps += 1;
            }
            None => self.silent_steps += 1,
        }
    }

    pub fn add_reused(&mut self, metrics: Option<Metrics>) {
        if let Some(metrics) = metrics {
            self.reused_cost_micro_usd = self
                .reused_cost_micro_usd
                .saturating_add(metrics.cost_micro_usd);
        }
    }
}

const KNOWN_KEYS: [&str; 3] = ["input_tokens", "output_tokens", "cost_usd"];

/// Parse a metrics file. Returns the metrics and notes about ignored keys.
/// Malformed values are an error, so a typo cannot silently read as zero spend.
pub fn parse(text: &str) -> Result<(Metrics, Vec<String>)> {
    if text.trim().is_empty() {
        bail!("metrics file is empty");
    }
    let value: Value = serde_json::from_str(text).context("metrics file is not JSON")?;
    let Value::Object(map) = value else {
        bail!("metrics file must hold a JSON object");
    };

    let mut notes = Vec::new();
    for key in map.keys() {
        if !KNOWN_KEYS.contains(&key.as_str()) {
            notes.push(format!(
                "ignored unknown metrics key `{key}`; known keys are {}",
                KNOWN_KEYS.join(", ")
            ));
        }
    }

    let tokens = |key: &str| -> Result<u64> {
        match map.get(key) {
            None => Ok(0),
            Some(value) => value
                .as_u64()
                .with_context(|| format!("`{key}` must be a non-negative integer, got {value}")),
        }
    };
    let cost_micro_usd = match map.get("cost_usd") {
        None => 0,
        Some(value) => {
            let dollars = value
                .as_f64()
                .with_context(|| format!("`cost_usd` must be a number, got {value}"))?;
            usd_to_micro(dollars).context("`cost_usd` is out of range")?
        }
    };

    Ok((
        Metrics {
            input_tokens: tokens("input_tokens")?,
            output_tokens: tokens("output_tokens")?,
            cost_micro_usd,
        },
        notes,
    ))
}

/// Convert a dollar amount to micro-dollars, refusing values that cannot be
/// a cost or a ceiling.
pub fn usd_to_micro(dollars: f64) -> Result<u64> {
    if !dollars.is_finite() || dollars < 0.0 {
        bail!("{dollars} is not a non-negative finite dollar amount");
    }
    let micro = (dollars * 1_000_000.0).round();
    // u64::MAX as f64 rounds up to 2^64, which does not fit.
    if micro >= u64::MAX as f64 {
        bail!("{dollars} is too large");
    }
    Ok(micro as u64)
}

pub fn format_usd(micro: u64) -> String {
    format!("${}.{:06}", micro / 1_000_000, micro % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_known_key() {
        let (metrics, notes) =
            parse(r#"{"input_tokens": 1200, "output_tokens": 340, "cost_usd": 0.0123}"#).unwrap();
        assert_eq!(
            metrics,
            Metrics {
                input_tokens: 1200,
                output_tokens: 340,
                cost_micro_usd: 12_300,
            }
        );
        assert!(notes.is_empty());
    }

    #[test]
    fn missing_keys_read_as_zero_for_that_key_only() {
        let (metrics, _) = parse(r#"{"cost_usd": 1.5}"#).unwrap();
        assert_eq!(metrics.cost_micro_usd, 1_500_000);
        assert_eq!(metrics.input_tokens, 0);
    }

    #[test]
    fn unknown_keys_are_ignored_out_loud() {
        let (_, notes) = parse(r#"{"cost_usd": 1, "total_cost_usd": 9}"#).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("total_cost_usd"), "{}", notes[0]);
    }

    #[test]
    fn a_malformed_value_is_an_error_not_zero_spend() {
        assert!(parse(r#"{"cost_usd": -1}"#).is_err());
        assert!(parse(r#"{"cost_usd": "0.5"}"#).is_err());
        assert!(parse(r#"{"input_tokens": 1.5}"#).is_err());
        assert!(parse(r#"{"input_tokens": -3}"#).is_err());
        assert!(parse("[1, 2]").is_err());
        assert!(parse("not json").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn dollars_round_to_the_nearest_micro_dollar() {
        assert_eq!(usd_to_micro(0.0000004).unwrap(), 0);
        assert_eq!(usd_to_micro(0.0000005).unwrap(), 1);
        assert_eq!(usd_to_micro(2.0).unwrap(), 2_000_000);
        assert!(usd_to_micro(f64::NAN).is_err());
        assert!(usd_to_micro(f64::INFINITY).is_err());
    }

    #[test]
    fn spend_separates_reported_silent_and_reused_cost() {
        let mut spend = Spend::default();
        spend.add_executed(Some(Metrics {
            input_tokens: 10,
            output_tokens: 5,
            cost_micro_usd: 300,
        }));
        spend.add_executed(None);
        spend.add_reused(Some(Metrics {
            cost_micro_usd: 700,
            ..Metrics::default()
        }));
        assert_eq!(spend.cost_micro_usd, 300);
        assert_eq!(spend.reporting_steps, 1);
        assert_eq!(spend.silent_steps, 1);
        assert_eq!(spend.reused_cost_micro_usd, 700);
        assert_eq!(spend.input_tokens, 10);
    }

    #[test]
    fn formats_micro_dollars_exactly() {
        assert_eq!(format_usd(12_300), "$0.012300");
        assert_eq!(format_usd(2_500_000), "$2.500000");
    }
}

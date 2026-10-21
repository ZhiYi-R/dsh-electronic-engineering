//! SCPI command header parsing and short/long mnemonic matching.
//!
//! SCPI manuals write the required short form in uppercase and the remainder
//! of the long form in lowercase (e.g. `MEASure` matches `MEAS` and `MEASURE`),
//! optionally followed by a numeric suffix (`CHANnel1`, `SOURce2`).
//! [`HeaderPattern`] keeps both forms so callers can pass either spelling while
//! outgoing wire commands always use the declared long form.

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Mnemonic {
    full: String,
    short: String,
    suffix: Option<u32>,
}

impl Mnemonic {
    fn parse(text: &str) -> Result<Self> {
        if text.is_empty() {
            return Err(Error::InvalidHeader("empty mnemonic".into()));
        }
        let split = text
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_digit())
            .last()
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        let (base, digits) = text.split_at(split);
        if base.is_empty() {
            return Err(Error::InvalidHeader(format!(
                "mnemonic '{text}' has no letters"
            )));
        }
        let suffix = if digits.is_empty() {
            None
        } else {
            Some(
                digits
                    .parse::<u32>()
                    .map_err(|_| Error::InvalidHeader(format!("bad numeric suffix in '{text}'")))?,
            )
        };
        let short: String = base.chars().filter(|c| c.is_ascii_uppercase()).collect();
        Ok(Self {
            full: base.to_string(),
            short: if short.is_empty() {
                base.to_ascii_uppercase()
            } else {
                short
            },
            suffix,
        })
    }

    fn mnemonic_eq(&self, other: &Self) -> bool {
        self.suffix == other.suffix
            && (self.full.eq_ignore_ascii_case(&other.full)
                || self.short.eq_ignore_ascii_case(&other.full)
                || self.full.eq_ignore_ascii_case(&other.short)
                || self.short.eq_ignore_ascii_case(&other.short))
    }
}

/// A parsed SCPI command header, optionally with a trailing query marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderPattern {
    nodes: Vec<Mnemonic>,
    common: Option<String>,
    pub query: bool,
}

impl HeaderPattern {
    /// Parse a header such as `:MEASure:VOLTage:DC?` or `*IDN?`.
    pub fn parse(header: &str) -> Result<Self> {
        let trimmed = header.trim();
        if trimmed.is_empty() {
            return Err(Error::InvalidHeader("empty header".into()));
        }
        if trimmed.contains(char::is_whitespace) {
            return Err(Error::InvalidHeader(format!(
                "'{trimmed}' contains whitespace; pass arguments separately"
            )));
        }
        if trimmed.starts_with('*') {
            let query = trimmed.ends_with('?');
            let token = trimmed.trim_end_matches('?').trim().to_ascii_uppercase();
            if token.len() < 2
                || !token[1..]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(Error::InvalidHeader(format!(
                    "bad common command '{trimmed}'"
                )));
            }
            return Ok(Self {
                nodes: Vec::new(),
                common: Some(token),
                query,
            });
        }
        let query = trimmed.ends_with('?');
        let core = trimmed.trim_end_matches('?').trim();
        let core = core.strip_prefix(':').unwrap_or(core);
        let nodes = core
            .split(':')
            .filter(|part| !part.is_empty())
            .map(Mnemonic::parse)
            .collect::<Result<Vec<_>>>()?;
        if nodes.is_empty() {
            return Err(Error::InvalidHeader(format!("bad header '{trimmed}'")));
        }
        Ok(Self {
            nodes,
            common: None,
            query,
        })
    }

    /// True when both headers address the same SCPI node path, ignoring the
    /// query marker and letter case.
    pub fn matches_path(&self, other: &Self) -> bool {
        match (&self.common, &other.common) {
            (Some(a), Some(b)) => a == b,
            (None, None) => {
                self.nodes.len() == other.nodes.len()
                    && self
                        .nodes
                        .iter()
                        .zip(other.nodes.iter())
                        .all(|(a, b)| a.mnemonic_eq(b))
            }
            _ => false,
        }
    }

    /// Render the documented long form, with a query marker when present.
    pub fn canonical(&self) -> String {
        let mut out = if let Some(common) = &self.common {
            common.clone()
        } else {
            let mut text = String::new();
            for node in &self.nodes {
                text.push(':');
                text.push_str(&node.full);
                if let Some(suffix) = node.suffix {
                    text.push_str(&suffix.to_string());
                }
            }
            text
        };
        if self.query {
            out.push('?');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(a: &str, b: &str) -> bool {
        HeaderPattern::parse(a)
            .unwrap()
            .matches_path(&HeaderPattern::parse(b).unwrap())
    }

    #[test]
    fn accepts_short_and_long_forms() {
        assert!(same(":MEASure:VOLTage:DC", ":meas:volt:dc"));
        assert!(same(":MEASure:VOLTage:DC", ":measure:voltage:dc"));
        assert!(same(":SYSTem:ERRor", ":syst:err"));
        assert!(!same(":MEASure:VOLTage:DC", ":FUNCtion:VOLTage:DC"));
    }

    #[test]
    fn parses_query_and_common_commands() {
        let h = HeaderPattern::parse("*IDN?").unwrap();
        assert!(h.query);
        assert_eq!(h.canonical(), "*IDN?");
        assert!(h.matches_path(&HeaderPattern::parse("*idn").unwrap()));
    }

    #[test]
    fn keeps_numeric_suffixes() {
        let h = HeaderPattern::parse(":SOURce1:VOLTage").unwrap();
        assert_eq!(h.canonical(), ":SOURce1:VOLTage");
        assert!(h.matches_path(&HeaderPattern::parse(":sour1:volt").unwrap()));
        assert!(!h.matches_path(&HeaderPattern::parse(":sour2:volt").unwrap()));
    }

    #[test]
    fn rejects_arguments_in_headers() {
        assert!(HeaderPattern::parse(":VOLTage:DC 2").is_err());
    }
}

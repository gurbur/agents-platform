//! Sequence numbers travel as JSON strings matching `^(0|[1-9][0-9]*)$`
//! with values in `0..=i64::MAX` (`IF-ID-001`).

use std::fmt;
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeqError {
    #[error(
        "sequence must be a non-empty string of decimal digits without sign, spaces or leading zeros"
    )]
    InvalidFormat,
    #[error("sequence is out of range 0..={}", i64::MAX)]
    OutOfRange,
}

/// A non-negative sequence value (`event_seq`, `inbox_seq`, `read_marker`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Seq(i64);

impl Seq {
    pub const ZERO: Seq = Seq(0);

    pub fn new(value: i64) -> Result<Self, SeqError> {
        if value < 0 {
            return Err(SeqError::OutOfRange);
        }
        Ok(Self(value))
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

impl FromStr for Seq {
    type Err = SeqError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
            return Err(SeqError::InvalidFormat);
        }
        if bytes.len() > 1 && bytes[0] == b'0' {
            return Err(SeqError::InvalidFormat);
        }
        // Only ASCII digits remain, so the sole possible parse failure is overflow.
        s.parse::<i64>().map(Seq).map_err(|_| SeqError::OutOfRange)
    }
}

impl TryFrom<i64> for Seq {
    type Error = SeqError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Seq> for i64 {
    fn from(seq: Seq) -> Self {
        seq.0
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for Seq {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Seq {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(SeqVisitor)
    }
}

struct SeqVisitor;

impl Visitor<'_> for SeqVisitor {
    type Value = Seq;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a sequence as a decimal string")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        v.parse().map_err(E::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_strings() {
        assert_eq!("0".parse::<Seq>(), Ok(Seq::ZERO));
        assert_eq!("42".parse::<Seq>().map(Seq::get), Ok(42));
        assert_eq!(
            "9223372036854775807".parse::<Seq>().map(Seq::get),
            Ok(i64::MAX)
        );
    }

    #[test]
    fn rejects_non_canonical_strings() {
        for invalid in ["", "007", "00", "-1", "+1", " 1", "1 ", "1.0", "1e3", "abc"] {
            assert_eq!(
                invalid.parse::<Seq>(),
                Err(SeqError::InvalidFormat),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn rejects_values_above_i64_max() {
        assert_eq!(
            "9223372036854775808".parse::<Seq>(),
            Err(SeqError::OutOfRange)
        );
    }

    #[test]
    fn rejects_negative_construction() {
        assert_eq!(Seq::new(-1), Err(SeqError::OutOfRange));
        assert_eq!(Seq::try_from(7).map(i64::from), Ok(7));
    }

    #[test]
    fn serializes_as_string() {
        let seq = Seq::new(42).unwrap();
        assert_eq!(serde_json::to_string(&seq).unwrap(), "\"42\"");
    }

    #[test]
    fn deserializes_only_from_canonical_string() {
        assert_eq!(serde_json::from_str::<Seq>("\"42\"").unwrap().get(), 42);
        assert!(serde_json::from_str::<Seq>("\"007\"").is_err());
        assert!(serde_json::from_str::<Seq>("\"-1\"").is_err());
        assert!(serde_json::from_str::<Seq>("\"9223372036854775808\"").is_err());
        assert!(serde_json::from_str::<Seq>("42").is_err());
        assert!(serde_json::from_str::<Seq>("null").is_err());
    }
}

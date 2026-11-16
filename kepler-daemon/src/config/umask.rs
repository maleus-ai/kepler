//! File-mode creation mask (`umask`) applied to spawned processes.
//!
//! Configured with `kepler.services.default_umask` (all services of a config)
//! and `services.<name>.umask` (one service). The value must be a quoted octal
//! string such as `'027'`.
//!
//! YAML numbers are rejected on purpose: a parser following YAML 1.2 reads an
//! unquoted `027` as the decimal 27, i.e. `0o033`, which would silently apply
//! the wrong mask. (serde_yaml 0.9 happens to keep leading-zero scalars such as
//! `027` as strings, so they parse correctly; `27`, `777` or `0o27` arrive as
//! numbers and are refused.)

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A validated umask (`0o000`..=`0o777`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Umask(u32);

impl Umask {
    /// Parse a 3- or 4-digit octal string (`^[0-7]{3,4}$`), at most `0777`.
    pub fn parse(s: &str) -> Result<Self, String> {
        if !(3..=4).contains(&s.len()) || !s.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return Err(format!(
                "invalid umask '{}': expected 3 or 4 octal digits, e.g. '027'",
                s
            ));
        }
        let value = u32::from_str_radix(s, 8)
            .map_err(|e| format!("invalid umask '{}': {}", s, e))?;
        if value > 0o777 {
            return Err(format!(
                "invalid umask '{}': only permission bits (at most '0777') can be masked",
                s
            ));
        }
        Ok(Self(value))
    }

    /// The mask as a raw mode value.
    pub fn bits(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Umask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:03o}", self.0)
    }
}

impl Serialize for Umask {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

const QUOTE_HINT: &str =
    "umask must be a quoted octal string, e.g. umask: '027' \
     (YAML numbers are refused: they may be read as decimal, not octal)";

struct UmaskVisitor;

impl<'de> Visitor<'de> for UmaskVisitor {
    type Value = Umask;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a quoted octal string such as '027'")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Umask, E> {
        Umask::parse(v).map_err(E::custom)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Umask, E> {
        Err(E::custom(format!("got the number {}; {}", v, QUOTE_HINT)))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Umask, E> {
        Err(E::custom(format!("got the number {}; {}", v, QUOTE_HINT)))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Umask, E> {
        Err(E::custom(format!("got the number {}; {}", v, QUOTE_HINT)))
    }
}

impl<'de> Deserialize<'de> for Umask {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UmaskVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_three_and_four_digit_octal() {
        assert_eq!(Umask::parse("027").unwrap().bits(), 0o027);
        assert_eq!(Umask::parse("0027").unwrap().bits(), 0o027);
        assert_eq!(Umask::parse("777").unwrap().bits(), 0o777);
        assert_eq!(Umask::parse("000").unwrap().bits(), 0);
    }

    #[test]
    fn rejects_non_octal_or_wrong_length() {
        for bad in ["", "27", "7", "028", "abc", "0o27", "00027", " 027", "-27"] {
            assert!(Umask::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn rejects_bits_above_permission_mask() {
        assert!(Umask::parse("1027").is_err());
        assert!(Umask::parse("0777").is_ok());
    }

    #[test]
    fn yaml_string_is_accepted() {
        let u: Umask = serde_yaml::from_str("'027'").unwrap();
        assert_eq!(u.bits(), 0o027);
    }

    /// serde_yaml 0.9 reads plain scalars with a leading zero (`027`) as
    /// strings, so they reach `visit_str` and get the octal value. Pin it: a
    /// parser that read `027` as the integer 27 would hit the integer error,
    /// never apply `0o033`.
    #[test]
    fn yaml_unquoted_leading_zero_is_never_misread() {
        match serde_yaml::from_str::<Umask>("027") {
            Ok(u) => assert_eq!(u.bits(), 0o027),
            Err(e) => assert!(e.to_string().contains("quoted"), "{e}"),
        }
    }

    #[test]
    fn yaml_integer_is_rejected_with_quote_hint() {
        for src in ["27", "0o27", "777", "0x17", "27.0"] {
            let err = serde_yaml::from_str::<Umask>(src).unwrap_err().to_string();
            assert!(err.contains("quoted"), "{src}: {err}");
        }
    }

    #[test]
    fn serializes_as_round_trippable_string() {
        let u = Umask::parse("0027").unwrap();
        let s = serde_yaml::to_string(&u).unwrap();
        let back: Umask = serde_yaml::from_str(&s).unwrap();
        assert_eq!(back, u);
        assert_eq!(u.to_string(), "027");
    }
}

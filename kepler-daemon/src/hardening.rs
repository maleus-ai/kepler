//! Hardening levels for privilege escalation prevention.
//!
//! The `--hardening` flag controls how strictly the daemon enforces
//! privilege boundaries between config owners and spawned processes.

use std::fmt;
use std::str::FromStr;

/// Hardening level for the daemon.
///
/// Variants have explicit `#[repr(u8)]` discriminants so that `PartialOrd`/`Ord`
/// ordering is guaranteed to match the security level (higher = more restrictive).
///
/// | Level (u8) | Privilege restriction                                       |
/// |------------|-------------------------------------------------------------|
/// | `none` (0) | No restrictions                                             |
/// | `no-root` (1) | Non-root config owners cannot run as root/daemon        |
/// | `strict` (2) | Non-root config owners can only run as themselves         |
// SAFETY: Variant order is security-critical. Do not reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
#[repr(u8)]
pub enum HardeningLevel {
    #[default]
    None = 0,
    NoRoot = 1,
    Strict = 2,
}

impl FromStr for HardeningLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(HardeningLevel::None),
            "no-root" => Ok(HardeningLevel::NoRoot),
            "strict" => Ok(HardeningLevel::Strict),
            _ => Err(format!(
                "invalid hardening level '{}': expected 'none', 'no-root', or 'strict'",
                s
            )),
        }
    }
}

impl fmt::Display for HardeningLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HardeningLevel::None => write!(f, "none"),
            HardeningLevel::NoRoot => write!(f, "no-root"),
            HardeningLevel::Strict => write!(f, "strict"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_levels() {
        assert_eq!("none".parse::<HardeningLevel>().unwrap(), HardeningLevel::None);
        assert_eq!("no-root".parse::<HardeningLevel>().unwrap(), HardeningLevel::NoRoot);
        assert_eq!("strict".parse::<HardeningLevel>().unwrap(), HardeningLevel::Strict);
    }

    #[test]
    fn parse_invalid_level() {
        assert!("invalid".parse::<HardeningLevel>().is_err());
    }

    #[test]
    fn display_roundtrip() {
        for level in [HardeningLevel::None, HardeningLevel::NoRoot, HardeningLevel::Strict] {
            assert_eq!(level.to_string().parse::<HardeningLevel>().unwrap(), level);
        }
    }

    #[test]
    fn ordering() {
        assert!(HardeningLevel::None < HardeningLevel::NoRoot);
        assert!(HardeningLevel::NoRoot < HardeningLevel::Strict);
    }

    #[test]
    fn default_is_none() {
        assert_eq!(HardeningLevel::default(), HardeningLevel::None);
    }
}

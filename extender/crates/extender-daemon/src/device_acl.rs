//! Device Access Control Lists (ACL) for filtering USB devices by VID:PID patterns.
//!
//! Patterns follow the format `"VVVV:PPPP"` where each side is either a
//! 4-digit hex value or `"*"` for wildcard matching.
//!
//! # Examples
//!
//! ```toml
//! [security]
//! allowed_devices = []           # empty = all allowed
//! denied_devices = ["0bda:*"]    # block all Realtek devices
//! ```

use crate::config::SecurityConfig;

/// A parsed VID:PID pattern where each component is either a specific value or a wildcard.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DevicePattern {
    vendor_id: Option<u16>,
    product_id: Option<u16>,
}

impl DevicePattern {
    /// Parse a VID:PID pattern string.
    ///
    /// Accepts patterns like `"0bda:8153"`, `"0bda:*"`, `"*:8153"`, or `"*:*"`.
    /// Returns `None` if the pattern is malformed.
    fn parse(pattern: &str) -> Option<Self> {
        let parts: Vec<&str> = pattern.split(':').collect();
        if parts.len() != 2 {
            return None;
        }

        let vendor_id = Self::parse_component(parts[0])?;
        let product_id = Self::parse_component(parts[1])?;

        Some(DevicePattern {
            vendor_id,
            product_id,
        })
    }

    /// Parse a single hex component or wildcard. Returns `Some(Some(value))` for a
    /// hex value, `Some(None)` for a wildcard, or `None` for an invalid string.
    fn parse_component(s: &str) -> Option<Option<u16>> {
        let s = s.trim();
        if s == "*" {
            Some(None)
        } else {
            u16::from_str_radix(s, 16).ok().map(Some)
        }
    }

    /// Check whether this pattern matches a given VID and PID.
    fn matches(&self, vid: u16, pid: u16) -> bool {
        let vid_ok = self.vendor_id.is_none_or(|v| v == vid);
        let pid_ok = self.product_id.is_none_or(|p| p == pid);
        vid_ok && pid_ok
    }
}

/// Check whether a device with the given VID and PID is allowed by the ACL policy.
///
/// Rules:
/// - If `allowed_devices` is empty, all devices are allowed (unless denied).
/// - If `allowed_devices` is non-empty, the device must match at least one allow pattern.
/// - If the device matches any `denied_devices` pattern, it is denied.
/// - Deny takes priority over allow.
pub fn is_device_allowed(vid: u16, pid: u16, config: &SecurityConfig) -> bool {
    // Check deny list first — deny always takes priority.
    for pattern_str in &config.denied_devices {
        if let Some(pattern) = DevicePattern::parse(pattern_str) {
            if pattern.matches(vid, pid) {
                return false;
            }
        } else {
            tracing::warn!(pattern = %pattern_str, "ignoring malformed denied_devices pattern");
        }
    }

    // Check allow list — empty means everything is allowed.
    if config.allowed_devices.is_empty() {
        return true;
    }

    for pattern_str in &config.allowed_devices {
        if let Some(pattern) = DevicePattern::parse(pattern_str) {
            if pattern.matches(vid, pid) {
                return true;
            }
        } else {
            tracing::warn!(pattern = %pattern_str, "ignoring malformed allowed_devices pattern");
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Pattern parsing tests --

    #[test]
    fn test_parse_exact_pattern() {
        let p = DevicePattern::parse("0bda:8153").unwrap();
        assert_eq!(p.vendor_id, Some(0x0bda));
        assert_eq!(p.product_id, Some(0x8153));
    }

    #[test]
    fn test_parse_wildcard_vendor() {
        let p = DevicePattern::parse("*:8153").unwrap();
        assert_eq!(p.vendor_id, None);
        assert_eq!(p.product_id, Some(0x8153));
    }

    #[test]
    fn test_parse_wildcard_product() {
        let p = DevicePattern::parse("0bda:*").unwrap();
        assert_eq!(p.vendor_id, Some(0x0bda));
        assert_eq!(p.product_id, None);
    }

    #[test]
    fn test_parse_full_wildcard() {
        let p = DevicePattern::parse("*:*").unwrap();
        assert_eq!(p.vendor_id, None);
        assert_eq!(p.product_id, None);
    }

    #[test]
    fn test_parse_invalid_pattern() {
        assert!(DevicePattern::parse("invalid").is_none());
        assert!(DevicePattern::parse("0bda:zzzz").is_none());
        assert!(DevicePattern::parse("0bda:8153:extra").is_none());
        assert!(DevicePattern::parse("").is_none());
    }

    #[test]
    fn test_pattern_matches_exact() {
        let p = DevicePattern::parse("0bda:8153").unwrap();
        assert!(p.matches(0x0bda, 0x8153));
        assert!(!p.matches(0x0bda, 0x8154));
        assert!(!p.matches(0x1234, 0x8153));
    }

    #[test]
    fn test_pattern_matches_wildcard_vendor() {
        let p = DevicePattern::parse("*:8153").unwrap();
        assert!(p.matches(0x0bda, 0x8153));
        assert!(p.matches(0x1234, 0x8153));
        assert!(!p.matches(0x0bda, 0x9999));
    }

    #[test]
    fn test_pattern_matches_wildcard_product() {
        let p = DevicePattern::parse("0bda:*").unwrap();
        assert!(p.matches(0x0bda, 0x8153));
        assert!(p.matches(0x0bda, 0x0001));
        assert!(!p.matches(0x1234, 0x8153));
    }

    // -- ACL logic tests --

    #[test]
    fn test_empty_lists_allows_all() {
        let config = SecurityConfig {
            allowed_devices: vec![],
            denied_devices: vec![],
        };
        assert!(is_device_allowed(0x0bda, 0x8153, &config));
        assert!(is_device_allowed(0x1234, 0x5678, &config));
    }

    #[test]
    fn test_allow_list_only() {
        let config = SecurityConfig {
            allowed_devices: vec!["0bda:8153".to_string(), "1234:*".to_string()],
            denied_devices: vec![],
        };
        assert!(is_device_allowed(0x0bda, 0x8153, &config));
        assert!(is_device_allowed(0x1234, 0x5678, &config));
        assert!(!is_device_allowed(0xAAAA, 0xBBBB, &config));
    }

    #[test]
    fn test_deny_list_only() {
        let config = SecurityConfig {
            allowed_devices: vec![],
            denied_devices: vec!["0bda:*".to_string()],
        };
        assert!(!is_device_allowed(0x0bda, 0x8153, &config));
        assert!(!is_device_allowed(0x0bda, 0x0001, &config));
        assert!(is_device_allowed(0x1234, 0x5678, &config));
    }

    #[test]
    fn test_deny_overrides_allow() {
        let config = SecurityConfig {
            allowed_devices: vec!["0bda:*".to_string()],
            denied_devices: vec!["0bda:8153".to_string()],
        };
        // 0bda:8153 matches allow but also matches deny — deny wins.
        assert!(!is_device_allowed(0x0bda, 0x8153, &config));
        // 0bda:0001 matches allow and not deny — allowed.
        assert!(is_device_allowed(0x0bda, 0x0001, &config));
        // Other vendor not in allow list — denied.
        assert!(!is_device_allowed(0x1234, 0x5678, &config));
    }

    #[test]
    fn test_full_wildcard_deny_blocks_everything() {
        let config = SecurityConfig {
            allowed_devices: vec!["*:*".to_string()],
            denied_devices: vec!["*:*".to_string()],
        };
        assert!(!is_device_allowed(0x0bda, 0x8153, &config));
    }

    #[test]
    fn test_wildcard_product_in_deny() {
        let config = SecurityConfig {
            allowed_devices: vec![],
            denied_devices: vec!["*:8153".to_string()],
        };
        assert!(!is_device_allowed(0x0bda, 0x8153, &config));
        assert!(!is_device_allowed(0x1234, 0x8153, &config));
        assert!(is_device_allowed(0x0bda, 0x0001, &config));
    }

    #[test]
    fn test_malformed_patterns_are_ignored() {
        let config = SecurityConfig {
            allowed_devices: vec!["invalid".to_string(), "0bda:8153".to_string()],
            denied_devices: vec!["alsobad".to_string()],
        };
        // The valid allow pattern should still work.
        assert!(is_device_allowed(0x0bda, 0x8153, &config));
        // Device not in valid allow patterns is denied.
        assert!(!is_device_allowed(0x1234, 0x5678, &config));
    }
}

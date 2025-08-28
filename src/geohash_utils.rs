//! Geohash utility functions for location-based routing
//! 
//! This module provides validation and normalization for geohash strings
//! used in location-based event routing. Events are routed to exact geohash
//! scopes only - no hierarchical propagation.

use geohash::decode;

/// Maximum allowed geohash precision (7 characters = ~152m)
pub const MAX_GEOHASH_LENGTH: usize = 7;

/// Valid characters in a geohash string
const VALID_GEOHASH_CHARS: &str = "0123456789bcdefghjkmnpqrstuvwxyz";

/// Validates a geohash string
/// 
/// A valid geohash:
/// - Contains only valid geohash characters (base32 subset)
/// - Is not empty
/// - Does not exceed MAX_GEOHASH_LENGTH
pub fn is_valid_geohash(gh: &str) -> bool {
    if gh.is_empty() || gh.len() > MAX_GEOHASH_LENGTH {
        return false;
    }
    
    // Check all characters are valid
    gh.chars().all(|c| VALID_GEOHASH_CHARS.contains(c.to_ascii_lowercase()))
}

/// Normalizes a geohash string to lowercase
/// 
/// Returns None if the geohash is invalid
pub fn normalize_geohash(gh: &str) -> Option<String> {
    if !is_valid_geohash(gh) {
        return None;
    }
    Some(gh.to_lowercase())
}

/// Validates a geohash using the georust library's decoder
/// 
/// This provides additional validation beyond character checking,
/// ensuring the geohash represents a valid geographic location
pub fn is_valid_geohash_strict(gh: &str) -> bool {
    if !is_valid_geohash(gh) {
        return false;
    }
    
    // Try to decode - if it fails, the geohash is invalid
    decode(gh).is_ok()
}

/// Extracts geohash tags from a Nostr event's tags array
/// 
/// Looks for tags with ["g", "geohash"] format and validates them.
/// Returns normalized (lowercase) geohashes.
pub fn extract_geohash_tags(tags: &[Vec<String>]) -> Vec<String> {
    tags.iter()
        .filter_map(|tag| {
            if tag.len() >= 2 && tag[0] == "g" {
                normalize_geohash(&tag[1])
            } else {
                None
            }
        })
        .collect()
}

/// Checks if a subdomain string is a valid geohash
/// 
/// Used to determine if a subdomain should be treated as a geohash scope
/// or a regular team/group name
pub fn is_geohash_subdomain(subdomain: &str) -> bool {
    // Must be valid geohash and use strict validation to ensure it's geographic
    is_valid_geohash_strict(subdomain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_geohash_basic() {
        // Valid geohashes at different precisions
        assert!(is_valid_geohash("d"));
        assert!(is_valid_geohash("dr"));
        assert!(is_valid_geohash("drt"));
        assert!(is_valid_geohash("drt2"));
        assert!(is_valid_geohash("drt2z"));
        assert!(is_valid_geohash("drt2zb"));
        assert!(is_valid_geohash("drt2zby"));
    }

    #[test]
    fn test_valid_geohash_lowercase() {
        assert!(is_valid_geohash("drt2zby"));
        assert!(is_valid_geohash("9q8yyk9"));
        assert!(is_valid_geohash("gbsuv7z"));
    }

    #[test]
    fn test_valid_geohash_uppercase() {
        // Should accept uppercase but normalize later
        assert!(is_valid_geohash("DRT2ZBY"));
        assert!(is_valid_geohash("9Q8YYK9"));
        assert!(is_valid_geohash("GBSUV7Z"));
    }

    #[test]
    fn test_invalid_geohash_too_long() {
        // Exceeds MAX_GEOHASH_LENGTH (7 chars)
        assert!(!is_valid_geohash("drt2zby8"));
        assert!(!is_valid_geohash("9q8yyk9t"));
        assert!(!is_valid_geohash("gbsuv7zt4m"));
    }

    #[test]
    fn test_invalid_geohash_empty() {
        assert!(!is_valid_geohash(""));
    }

    #[test]
    fn test_invalid_geohash_bad_chars() {
        // Invalid characters (a, i, l, o are not valid in geohash)
        assert!(!is_valid_geohash("art2z"));  // 'a' is invalid
        assert!(!is_valid_geohash("dri2z"));  // 'i' is invalid  
        assert!(!is_valid_geohash("drl2z"));  // 'l' is invalid
        assert!(!is_valid_geohash("dro2z"));  // 'o' is invalid
        assert!(!is_valid_geohash("dr!2z"));  // Special chars invalid
        assert!(!is_valid_geohash("dr 2z"));  // Space invalid
    }

    #[test]
    fn test_normalize_geohash() {
        // Valid geohashes get normalized to lowercase
        assert_eq!(normalize_geohash("DRT2Z"), Some("drt2z".to_string()));
        assert_eq!(normalize_geohash("drt2z"), Some("drt2z".to_string()));
        assert_eq!(normalize_geohash("9Q8YYK9"), Some("9q8yyk9".to_string()));
        
        // Invalid geohashes return None
        assert_eq!(normalize_geohash(""), None);
        assert_eq!(normalize_geohash("drt2zby8"), None);  // Too long
        assert_eq!(normalize_geohash("dr!2z"), None);     // Invalid char
    }

    #[test]
    fn test_strict_validation() {
        // These are syntactically valid but might not be decodable
        assert!(is_valid_geohash_strict("drt2z"));   // San Francisco area
        assert!(is_valid_geohash_strict("9q8yy"));   // Los Angeles area
        assert!(is_valid_geohash_strict("gbsuv"));   // London area
        assert!(is_valid_geohash_strict("u"));       // Northern hemisphere
        
        // Empty string should fail strict validation
        assert!(!is_valid_geohash_strict(""));
        
        // Too long should fail
        assert!(!is_valid_geohash_strict("drt2zby8"));
    }

    #[test]
    fn test_max_length_enforcement() {
        // Exactly at max length (7 chars)
        assert!(is_valid_geohash("1234567"));
        assert_eq!(normalize_geohash("1234567"), Some("1234567".to_string()));
        
        // One over max length
        assert!(!is_valid_geohash("12345678"));
        assert_eq!(normalize_geohash("12345678"), None);
    }

    #[test]
    fn test_case_insensitive_validation() {
        let mixed_case = "DrT2zBy";
        assert!(is_valid_geohash(mixed_case));
        assert_eq!(normalize_geohash(mixed_case), Some("drt2zby".to_string()));
    }

    #[test]
    fn test_all_valid_chars() {
        // Test string with all valid geohash characters
        let all_valid = "0123456789bcdefghjkmnpqrstuvwxyz";
        for c in all_valid.chars() {
            let gh = c.to_string();
            assert!(is_valid_geohash(&gh), "Character '{}' should be valid", c);
            assert_eq!(normalize_geohash(&gh), Some(gh.clone()));
        }
    }

    #[test]
    fn test_georust_decode_integration() {
        // Test that our validation aligns with georust's decoder
        let valid_geohashes = vec!["u", "u0", "u09", "u09t", "u09tu", "u09tun", "u09tunq"];
        
        for gh in valid_geohashes {
            assert!(is_valid_geohash(gh));
            assert!(is_valid_geohash_strict(gh));
            
            // Verify georust can actually decode it
            let decoded = decode(gh);
            assert!(decoded.is_ok(), "Georust should decode '{}' successfully", gh);
        }
    }

    #[test]
    fn test_extract_geohash_tags() {
        let tags = vec![
            vec!["g".to_string(), "drt2z".to_string()],
            vec!["g".to_string(), "9Q8YY".to_string()],  // Uppercase
            vec!["p".to_string(), "pubkey123".to_string()],  // Not a geohash tag
            vec!["g".to_string(), "invalid!".to_string()],  // Invalid geohash
            vec!["g".to_string(), "toolonggeohash".to_string()],  // Too long
            vec!["g".to_string()],  // Missing value
        ];
        
        let extracted = extract_geohash_tags(&tags);
        assert_eq!(extracted, vec!["drt2z", "9q8yy"]);  // Only valid, normalized
    }

    #[test]
    fn test_extract_multiple_valid_geohashes() {
        let tags = vec![
            vec!["g".to_string(), "drt2z".to_string()],
            vec!["g".to_string(), "9q8yy".to_string()],
            vec!["g".to_string(), "gbsuv".to_string()],
        ];
        
        let extracted = extract_geohash_tags(&tags);
        assert_eq!(extracted.len(), 3);
        assert!(extracted.contains(&"drt2z".to_string()));
        assert!(extracted.contains(&"9q8yy".to_string()));
        assert!(extracted.contains(&"gbsuv".to_string()));
    }

    #[test]
    fn test_is_geohash_subdomain() {
        // Valid geohash subdomains
        assert!(is_geohash_subdomain("drt2z"));
        assert!(is_geohash_subdomain("9q8yy"));
        assert!(is_geohash_subdomain("u"));
        
        // Invalid - not valid geohashes
        assert!(!is_geohash_subdomain("team1"));  // Contains invalid chars
        assert!(!is_geohash_subdomain("alice"));  // Contains invalid chars
        assert!(!is_geohash_subdomain(""));       // Empty
        assert!(!is_geohash_subdomain("12345678")); // Too long
        
        // Edge case - valid chars but might not be geographic
        // This is why we use strict validation
        assert!(is_geohash_subdomain("d"));  // Valid geohash
    }
}
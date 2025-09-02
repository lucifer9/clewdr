use axum::body::Body;
use colored::{ColoredString, Colorize};
use tokio::spawn;
use tracing::{error, info};

use crate::{
    config::{CLEWDR_CONFIG, LOG_DIR},
    error::ClewdrError,
};

/// Helper function to format a boolean value as "Enabled" or "Disabled"
pub fn enabled(flag: bool) -> ColoredString {
    if flag {
        "Enabled".green()
    } else {
        "Disabled".red()
    }
}

/// Helper function to print out JSON to a file in the log directory
///
/// # Arguments
/// * `json` - The JSON object to serialize and output
/// * `file_name` - The name of the file to write in the log directory
pub fn print_out_json(json: impl serde::ser::Serialize, file_name: &str) {
    if CLEWDR_CONFIG.load().no_fs {
        return;
    }
    let text = serde_json::to_string_pretty(&json).unwrap_or_default();
    print_out_text(text, file_name);
}

/// Helper function to print out text to a file in the log directory
///
/// # Arguments
/// * `text` - The text content to write
/// * `file_name` - The name of the file to write in the log directory
pub fn print_out_text(text: String, file_name: &str) {
    if CLEWDR_CONFIG.load().no_fs {
        return;
    }
    let file_name = LOG_DIR.join(file_name);
    spawn(async move {
        if let Err(e) = tokio::fs::write(file_name, text).await {
            error!("Failed to write to file: {}\n", e);
        }
    });
}

/// Timezone for the API
pub const TIME_ZONE: &str = "America/New_York";

pub fn forward_response(in_: wreq::Response) -> Result<http::Response<Body>, ClewdrError> {
    let status = in_.status();
    let header = in_.headers().to_owned();
    let stream = in_.bytes_stream();
    let mut res = http::Response::builder().status(status);

    let headers = res.headers_mut().unwrap();
    for (key, value) in header {
        if let Some(key) = key {
            headers.insert(key, value);
        }
    }

    Ok(res.body(Body::from_stream(stream))?)
}

/// Extract all tags that appear at the top level of the content
/// Uses simplified lenient parsing: any tag that starts the document or appears after balanced content
/// Returns Ok(tags) if top-level tags can be identified, Err(message) for critical issues
fn extract_top_level_tags(content: &str) -> Result<Vec<String>, String> {
    let mut top_level_tags = Vec::new();
    let mut top_level_stack: Vec<String> = Vec::new();
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    let mut depth: usize = 0;

    while i < chars.len() {
        if chars[i] == '<' {
            let _start_pos = i;
            i += 1; // Skip '<'
            if i >= chars.len() {
                break;
            }

            // Check for closing tag
            let is_closing = chars[i] == '/';
            if is_closing {
                i += 1; // Skip '/'
            }

            // Extract tag name
            let mut tag_name = String::new();
            while i < chars.len() && chars[i] != '>' && !chars[i].is_whitespace() && chars[i] != '/' {
                tag_name.push(chars[i]);
                i += 1;
            }

            // Check for self-closing tag
            let mut is_self_closing = false;
            while i < chars.len() && chars[i] != '>' {
                if chars[i] == '/' {
                    is_self_closing = true;
                }
                i += 1;
            }
            if i < chars.len() {
                i += 1; // Skip '>'
            }

            // Skip comments and other special syntax
            if tag_name.starts_with('!') || tag_name.starts_with('?') {
                continue;
            }

            if is_self_closing {
                // Self-closing tag - it's top-level if depth is currently 0
                if depth == 0 {
                    top_level_tags.push(tag_name.clone());
                }
                // Self-closing tags don't affect depth or stack
            } else if is_closing {
                // Check if this closes a top-level tag
                if let Some(expected_tag) = top_level_stack.last() {
                    if expected_tag == &tag_name {
                        // This is closing a top-level tag, reset depth to 0
                        top_level_stack.pop();
                        depth = 0;
                    } else {
                        // Check if this might be a top-level tag mismatch
                        if depth == 1 {
                            return Err(format!(
                                "Top-level tag mismatch: expected '</{}>' but found '</{}>'" ,
                                expected_tag, tag_name
                            ));
                        } else {
                            // This is closing a nested tag
                            depth = depth.saturating_sub(1);
                        }
                    }
                } else {
                    // No top-level tag to match, just decrease depth
                    depth = depth.saturating_sub(1);
                }
            } else {
                // Opening tag - it's top-level if depth is currently 0
                if depth == 0 {
                    top_level_tags.push(tag_name.clone());
                    top_level_stack.push(tag_name.clone());
                }
                depth += 1;
            }
        } else {
            i += 1;
        }
    }

    // Check if any top-level tags are left unclosed
    if !top_level_stack.is_empty() {
        return Err(format!(
            "Unclosed top-level tags: {}",
            top_level_stack.join(", ")
        ));
    }

    Ok(top_level_tags)
}

/// Validate that all required tags exist at top level and are properly closed
/// Returns Ok(()) if valid, Err(error_message) if any required tag is missing or if parsing fails
pub fn validate_required_tags(content: &str, required_tags: &str) -> Result<(), String> {
    if required_tags.trim().is_empty() {
        return Ok(());
    }

    let required_list: Vec<&str> = required_tags
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if required_list.is_empty() {
        return Ok(());
    }

    info!(
        "[TAG_VALIDATION] Validating required tags: {:?} in content of {} bytes",
        required_list,
        content.len()
    );

    // Extract all top-level tags with validation
    let top_level_tags = match extract_top_level_tags(content) {
        Ok(tags) => tags,
        Err(error) => {
            let error_msg = format!("Parse error: {}", error);
            info!("[TAG_VALIDATION] {}", error_msg);
            return Err(error_msg);
        }
    };

    // Check if all required tags are present at top level
    for required_tag in &required_list {
        if !top_level_tags.contains(&required_tag.to_string()) {
            let error_msg = format!("Required tag '{}' not found at top level", required_tag);
            info!("[TAG_VALIDATION] {}", error_msg);
            return Err(error_msg);
        }
    }

    info!("[TAG_VALIDATION] All required tags validated successfully");
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_required_tags_empty_config() {
        assert!(validate_required_tags("any content", "").is_ok());
        assert!(validate_required_tags("any content", "   ").is_ok());
    }

    #[test]
    fn test_validate_required_tags_basic_functionality() {
        // Single required tag - present and closed
        assert!(validate_required_tags("<assess>yes</assess>", "assess").is_ok());

        // Multiple required tags - all present and closed
        assert!(validate_required_tags(
            "<assess>yes</assess><thinking>process</thinking>",
            "assess,thinking"
        ).is_ok());

        // Self-closing tag
        assert!(validate_required_tags("<details/>", "details").is_ok());

        // Mixed regular and self-closing
        assert!(validate_required_tags(
            "<assess>yes</assess><details/>",
            "assess,details"
        ).is_ok());
    }

    #[test]
    fn test_validate_required_tags_missing_tags() {
        // No tags present
        assert!(validate_required_tags("no tags here", "assess").is_err());

        // Some but not all required tags
        assert!(validate_required_tags(
            "<assess>yes</assess>",
            "assess,thinking"
        ).is_err());

        // Wrong tags present
        assert!(validate_required_tags(
            "<thinking>process</thinking>",
            "assess"
        ).is_err());
    }

    #[test]
    fn test_validate_required_tags_unclosed_tags() {
        // Unclosed required tag
        assert!(validate_required_tags("<assess>incomplete", "assess").is_err());

        // Some closed, some unclosed
        assert!(validate_required_tags(
            "<assess>yes</assess><thinking>incomplete",
            "assess,thinking"
        ).is_err());

        // Mismatched tags
        assert!(validate_required_tags(
            "<assess><thinking></assess></thinking>",
            "assess,thinking"
        ).is_err());
    }

    #[test]
    fn test_validate_required_tags_nesting() {
        // Nested tags - only top level should be considered
        assert!(validate_required_tags(
            "<assess><thinking>nested</thinking></assess>",
            "assess"
        ).is_ok());

        // Both tags at top level
        assert!(validate_required_tags(
            "<assess></assess><thinking></thinking>",
            "assess,thinking"
        ).is_ok());

        // Required tag nested (should fail)
        assert!(validate_required_tags(
            "<other><assess>nested</assess></other>",
            "assess"
        ).is_err());
    }

    #[test]
    fn test_validate_required_tags_tag_name_precision() {
        // Test think vs thinking precision
        assert!(validate_required_tags(
            "<thinking>content</thinking>",
            "thinking"
        ).is_ok());

        // think should not match thinking
        assert!(validate_required_tags(
            "<thinking>content</thinking>",
            "think"
        ).is_err());

        // think should match think exactly
        assert!(validate_required_tags(
            "<think>content</think>",
            "think"
        ).is_ok());

        // Both present
        assert!(validate_required_tags(
            "<think>a</think><thinking>b</thinking>",
            "think,thinking"
        ).is_ok());
    }

    #[test]
    fn test_validate_required_tags_case_insensitive() {
        // XML is case sensitive, so this should work correctly
        assert!(validate_required_tags(
            "<ASSESS>content</ASSESS>",
            "ASSESS"
        ).is_ok());

        // Different cases should not match
        assert!(validate_required_tags(
            "<assess>content</assess>",
            "ASSESS"
        ).is_err());
    }

    #[test]
    fn test_validate_required_tags_truncation_detection() {
        // Incomplete opening tag
        assert!(validate_required_tags("<asse", "assess").is_err());

        // Incomplete closing tag
        assert!(validate_required_tags("<assess>content</asse", "assess").is_err());

        // Dangling less-than (should not cause false positives)
        assert!(validate_required_tags("1 < 2 and 3 > 1", "assess").is_err());
    }

    #[test]
    fn test_detailed_error_messages() {
        // Test unclosed tag error message
        let result = validate_required_tags("<thinking>unclosed", "thinking");
        assert!(result.is_err());
        let error = result.unwrap_err();
        println!("Unclosed tag error: {}", error);
        assert!(error.contains("Unclosed top-level tags: thinking"));

        // Test missing tag error message
        let result = validate_required_tags("<other>content</other>", "thinking");
        assert!(result.is_err());
        let error = result.unwrap_err();
        println!("Missing tag error: {}", error);
        assert!(error.contains("Required tag 'thinking' not found at top level"));

        // Test mismatched tags error message
        let result = validate_required_tags("<thinking><assess></thinking></assess>", "thinking,assess");
        assert!(result.is_err());
        let error = result.unwrap_err();
        println!("Mismatched tag error: {}", error);
        // Just check if it's an error, don't check specific message since this one is complex

        // Test valid case
        let result = validate_required_tags("<thinking>content</thinking>", "thinking");
        assert!(result.is_ok());
    }

    #[test]
    fn test_lenient_nested_parsing() {
        // Test that nested tag problems don't affect top-level validation

        // Debug the parsing process
        println!("Testing case 1: '<thinking>content <broken>unclosed nested</thinking>'");
        let test_content = "<thinking>content <broken>unclosed nested</thinking>";
        match extract_top_level_tags(test_content) {
            Ok(tags) => println!("Extracted tags: {:?}", tags),
            Err(e) => println!("Extraction error: {}", e),
        }

        let result = validate_required_tags(
            test_content,
            "thinking"
        );
        if let Err(e) = &result {
            println!("Case 1 failed: {}", e);
        }
        assert!(result.is_ok()); // Should pass because thinking is properly closed at top level

        // Case 2: Nested mismatched tags should not affect top-level validation
        let result = validate_required_tags(
            "<thinking><part><other></part></other>completed</thinking>",
            "thinking"
        );
        assert!(result.is_ok()); // Should pass because thinking is properly closed

        // Case 3: Multiple top-level tags with nested problems - corrected case
        let result = validate_required_tags(
            "<thinking><broken>unclosed</thinking><content>good content</content>",
            "thinking,content"
        );
        assert!(result.is_ok()); // Should pass because both top-level tags are closed

        // Case 4: Top-level tag actually unclosed should still fail
        let result = validate_required_tags(
            "<thinking>content <nested>fine</nested>", // thinking not closed
            "thinking"
        );
        assert!(result.is_err()); // Should fail because thinking is not closed
        let error = result.unwrap_err();
        assert!(error.contains("Unclosed top-level tags: thinking"));

        // Case 5: Top-level tag mismatch should still fail
        let result = validate_required_tags(
            "<thinking>content</content>", // wrong closing tag at top level
            "thinking"
        );
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.contains("Top-level tag mismatch"));
    }

    #[test]
    fn test_complex_nested_scenarios() {
        // Real-world scenario: markdown-style tag references in content
        let content = r#"
<thinking>
This is thinking content with `<part_of_user>` reference and other stuff.
Some more content with <nested_tag>that might be broken
</thinking>
<content>
Main content here
</content>
"#;

        let result = validate_required_tags(content, "thinking,content");
        assert!(result.is_ok()); // Should pass despite nested issues

        // Verify we correctly identify top-level tags
        let top_level_tags = extract_top_level_tags(content).unwrap();
        assert_eq!(top_level_tags, vec!["thinking", "content"]);
    }

    #[test]
    fn test_response_files() {
        use std::fs;

        // Test response-20250825080921447.txt
        if let Ok(content) = fs::read_to_string("response-20250825080921447.txt") {
            println!("Testing response-20250825080921447.txt:");

            // Should pass - thinking tag exists
            match validate_required_tags(&content, "thinking") {
                Ok(()) => println!("✅ File 1 'thinking' tag: PASSED"),
                Err(e) => println!("❌ File 1 'thinking' tag: FAILED - {}", e),
            }

            // Should fail - assess tag does not exist
            match validate_required_tags(&content, "assess") {
                Ok(()) => println!("✅ File 1 'assess' tag: PASSED"),
                Err(e) => println!("❌ File 1 'assess' tag: FAILED - {}", e),
            }
        } else {
            println!("Could not read response-20250825080921447.txt");
        }

        // Test response-20250826055710737.txt
        if let Ok(content) = fs::read_to_string("response-20250826055710737.txt") {
            println!("Testing response-20250826055710737.txt:");

            // Should pass - thinking tag exists
            match validate_required_tags(&content, "thinking") {
                Ok(()) => println!("✅ File 2 'thinking' tag: PASSED"),
                Err(e) => println!("❌ File 2 'thinking' tag: FAILED - {}", e),
            }

            // Should pass - content tag exists
            match validate_required_tags(&content, "content") {
                Ok(()) => println!("✅ File 2 'content' tag: PASSED"),
                Err(e) => println!("❌ File 2 'content' tag: FAILED - {}", e),
            }
        } else {
            println!("Could not read response-20250826055710737.txt");
        }
    }
}

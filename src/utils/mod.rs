use axum::body::Body;
use colored::{ColoredString, Colorize};
use std::fs;
use tokio::{io::AsyncWriteExt, spawn};
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
        let Ok(mut file) = tokio::fs::File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&file_name)
            .await
        else {
            error!("Failed to open file: {}", file_name.display());
            return;
        };
        if let Err(e) = file.write_all(text.as_bytes()).await {
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

/// Save response content to a timestamped file for debugging
///
/// # Arguments
/// * `content` - The content to save
/// * `prefix` - Optional prefix for the filename (default: "response")
///
/// # Returns
/// * `String` - The filename that was created
fn save_response_content(content: &str, prefix: Option<&str>) -> String {
    let now = chrono::Utc::now();
    let timestamp = now.format("%Y%m%d%H%M%S%3f").to_string();
    let prefix = prefix.unwrap_or("response");
    let filename = format!("{prefix}-{timestamp}.txt");

    if let Err(e) = fs::write(&filename, content) {
        error!(
            "[SAVE_RESPONSE] Failed to save content to {}: {}",
            filename, e
        );
    } else {
        info!(
            "[SAVE_RESPONSE] Content saved to: {} ({} bytes)",
            filename,
            content.len()
        );
    }

    filename
}

/// Check if all tags in the content are properly closed
///
/// # Arguments
/// * `content` - The content to check
/// * `tags_to_check` - Comma-separated list of tags to check (e.g., "div,span,p")
///
/// # Returns
/// * `true` if all specified tags are properly closed or not present
/// * `false` if any tag is unclosed
pub fn check_tags_closed(content: &str, tags_to_check: &str) -> bool {
    if tags_to_check.trim().is_empty() {
        return true;
    }

    let tags: Vec<&str> = tags_to_check
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if tags.is_empty() {
        return true;
    }

    // Save content to file for debugging before checking if enabled
    let config = CLEWDR_CONFIG.load();
    if config.save_response_before_tag_check {
        let filename = save_response_content(content, Some("response"));
        info!("[TAG_CHECK] Response content saved to: {}", filename);
    }

    info!(
        "[TAG_CHECK] Checking tags: {:?} in content of {} bytes",
        tags,
        content.len()
    );

    // Check comment balance first before checking individual tags
    if content.contains("<!--") {
        let comment_starts = content.matches("<!--").count();
        let comment_ends = content.matches("-->").count();
        if comment_starts != comment_ends {
            info!(
                "[TAG_CHECK] Unbalanced comments detected: {} starts, {} ends",
                comment_starts, comment_ends
            );
            info!("[TAG_CHECK] Cannot check tags due to unbalanced HTML comments");
            return false;
        }
    }

    // Analyze top-level tags using the new logic
    let top_level_analysis = analyze_top_level_tags(content, &tags);
    
    // Check if any top-level tag is unbalanced
    for (tag_name, is_balanced) in &top_level_analysis {
        if !is_balanced {
            info!("[TAG_CHECK] Top-level tag '{}' is not balanced", tag_name);
            return false;
        }
    }

    info!("[TAG_CHECK] All top-level tags are properly balanced");
    true
}

/// Check if all required tags exist and are properly closed in the content
/// # Arguments
/// * `content` - The content to check
/// * `required_tags` - Comma-separated list of tags that must be present
/// # Returns  
/// * `true` if all required tags exist and are properly closed
/// * `false` if any required tag is missing or not properly closed
pub fn check_required_tags_exist(content: &str, required_tags: &str) -> bool {
    if required_tags.trim().is_empty() {
        return true;
    }

    let tags: Vec<&str> = required_tags
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if tags.is_empty() {
        return true;
    }

    info!(
        "[REQUIRED_TAGS] Checking required tags: {:?} in content of {} bytes",
        tags,
        content.len()
    );

    for tag in &tags {
        // First check if the tag exists at all
        let opening_tag = format!("<{tag}");
        if !content.contains(&opening_tag) {
            info!("[REQUIRED_TAGS] Required tag '{}' is missing", tag);
            return false;
        }
    }

    // Analyze top-level tags to check if required tags appear at top level and are balanced
    let top_level_analysis = analyze_top_level_tags(content, &tags);
    
    // Check if all required tags appear at top level and are balanced
    for tag in tags {
        if let Some((_, is_balanced)) = top_level_analysis.iter().find(|(name, _)| name == tag) {
            if !is_balanced {
                info!("[REQUIRED_TAGS] Required tag '{}' appears at top level but is not properly balanced", tag);
                return false;
            }
        } else {
            info!("[REQUIRED_TAGS] Required tag '{}' does not appear at top level", tag);
            return false;
        }
    }

    info!("[REQUIRED_TAGS] All required tags appear at top level and are properly balanced");
    true
}

/// Analyze which tags appear at the top level and whether they are balanced
/// Returns a vector of (tag_name, is_balanced) for all tags that appear at top level
fn analyze_top_level_tags(content: &str, tags_to_check: &[&str]) -> Vec<(String, bool)> {
    // Remove comments to avoid false positives
    let content_without_comments = remove_comments(content);
    
    // If remove_comments returned empty string due to malformed comments,
    // treat as incomplete content
    if content.contains("<!--") && content_without_comments.is_empty() {
        return vec![];
    }

    let mut result = Vec::new();
    let mut nesting_depth = 0; // Depth of nesting in any of the specified tags
    let mut i = 0;
    let chars: Vec<char> = content_without_comments.chars().collect();

    // Track balance count for each top-level tag (opening - closing)
    let mut top_level_balance: std::collections::HashMap<String, i32> = std::collections::HashMap::new();

    while i < chars.len() {
        if chars[i] == '<' {
            let mut matched_tag = None;
            let mut is_closing = false;
            let mut is_self_closing = false;

            // Check for closing tag first
            if i + 1 < chars.len() && chars[i + 1] == '/' {
                // First try to match our specified tags
                for &tag in tags_to_check {
                    let closing_tag = format!("</{}>", tag);
                    if let Some(end_pos) = find_tag_end(&chars, i, &closing_tag) {
                        matched_tag = Some((tag.to_string(), true)); // true means it's a tracked tag
                        is_closing = true;
                        i = end_pos + 1;
                        break;
                    }
                }
                
                // If not found, check if it's any other tag that could affect nesting
                if matched_tag.is_none() {
                    // Try to match any tag pattern for nesting purposes
                    if i + 2 < chars.len() && chars[i + 2].is_alphabetic() {
                        let mut j = i + 2;
                        while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '-' || chars[j] == '_') {
                            j += 1;
                        }
                        if j < chars.len() && chars[j] == '>' {
                            // Found a non-tracked closing tag
                            matched_tag = Some(("_other".to_string(), false)); // false means it's not tracked
                            is_closing = true;
                            i = j + 1;
                        }
                    }
                }
            } else {
                // Check for opening or self-closing tag
                for &tag in tags_to_check {
                    let opening_tag = format!("<{}", tag);
                    let self_closing_tag = format!("<{}/>", tag);
                    
                    // Try self-closing first
                    if let Some(end_pos) = find_tag_end(&chars, i, &self_closing_tag) {
                        matched_tag = Some((tag.to_string(), true));
                        is_self_closing = true;
                        i = end_pos + 1;
                        break;
                    }
                    
                    // Try opening tag
                    if let Some(end_pos) = find_opening_tag_end(&chars, i, &opening_tag) {
                        matched_tag = Some((tag.to_string(), true));
                        i = end_pos + 1;
                        break;
                    }
                }
                
                // If not found, check if it's any other opening tag
                if matched_tag.is_none()
                    && chars[i + 1].is_alphabetic() {
                        let mut j = i + 1;
                        while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '-' || chars[j] == '_') {
                            j += 1;
                        }
                        // Find the closing > of this tag
                        while j < chars.len() && chars[j] != '>' {
                            if chars[j] == '"' || chars[j] == '\'' {
                                let quote = chars[j];
                                j += 1;
                                while j < chars.len() && chars[j] != quote {
                                    j += 1;
                                }
                                if j < chars.len() { j += 1; }
                            } else {
                                j += 1;
                            }
                        }
                        if j < chars.len() && chars[j] == '>' {
                            // Check if it's self-closing
                            if j > 0 && chars[j - 1] == '/' {
                                // Self-closing non-tracked tag - doesn't affect nesting
                                i = j + 1;
                                continue;
                            } else {
                                // Found a non-tracked opening tag
                                matched_tag = Some(("_other".to_string(), false));
                                i = j + 1;
                            }
                        }
                    }
            }

            if let Some((tag_name, is_tracked)) = matched_tag {
                if is_self_closing && is_tracked {
                    // Self-closing tracked tags at top level are always balanced
                    if nesting_depth == 0 {
                        // This is a top-level tag - always balanced for self-closing
                        if !result.iter().any(|(name, _)| name == &tag_name) {
                            result.push((tag_name.clone(), true));
                        }
                    }
                } else if is_closing {
                    if is_tracked {
                        // Always decrease nesting depth first
                        nesting_depth -= 1;
                        
                        // Handle closing tag - check if this was a top-level closing tag
                        if nesting_depth == 0 {
                            // This was a top-level closing tag
                            let count = top_level_balance.entry(tag_name.clone()).or_insert(0);
                            *count -= 1;
                            
                            // Make sure we have an entry in result for this tag
                            if !result.iter().any(|(name, _)| name == &tag_name) {
                                result.push((tag_name.clone(), false)); // Will be updated later
                            }
                        }
                    } else {
                        // Non-tracked closing tag
                        if nesting_depth > 0 {
                            nesting_depth -= 1;
                        }
                    }
                } else {
                    // Handle opening tag
                    if is_tracked {
                        if nesting_depth == 0 {
                            // This is a top-level opening tag
                            let count = top_level_balance.entry(tag_name.clone()).or_insert(0);
                            *count += 1;
                            
                            // Add to result if not already there
                            if !result.iter().any(|(name, _)| name == &tag_name) {
                                result.push((tag_name.clone(), false)); // Will be updated later
                            }
                        }
                        
                        // Increase nesting depth for any tracked tag
                        nesting_depth += 1;
                    } else {
                        // Non-tracked opening tag
                        nesting_depth += 1;
                    }
                }
                continue;
            }
        }
        i += 1;
    }

    // Update balance status based on counts
    for (tag_name, is_balanced) in &mut result {
        if let Some(&balance) = top_level_balance.get(tag_name) {
            *is_balanced = balance == 0;
        }
    }

    result
}


/// Find the end position of a specific tag
fn find_tag_end(chars: &[char], start: usize, tag: &str) -> Option<usize> {
    let tag_chars: Vec<char> = tag.chars().collect();

    if start + tag_chars.len() > chars.len() {
        return None;
    }

    for (i, &ch) in tag_chars.iter().enumerate() {
        if !chars[start + i].eq_ignore_ascii_case(&ch) {
            return None;
        }
    }

    // For exact tags like "</div>", we need the closing >
    if tag.ends_with('>') {
        return Some(start + tag_chars.len() - 1);
    }

    None
}

/// Find the end position of an opening tag (handles attributes)
fn find_opening_tag_end(chars: &[char], start: usize, opening_tag: &str) -> Option<usize> {
    let tag_chars: Vec<char> = opening_tag.chars().collect();

    if start + tag_chars.len() > chars.len() {
        return None;
    }

    // Check if the opening tag matches
    for (i, &ch) in tag_chars.iter().enumerate() {
        if !chars[start + i].eq_ignore_ascii_case(&ch) {
            return None;
        }
    }

    // CRITICAL FIX: Ensure the tag name is followed by a valid separator
    // This prevents "<think" from matching "<thinking>"
    let next_char_index = start + tag_chars.len();
    if next_char_index < chars.len() {
        let next_char = chars[next_char_index];
        // Valid separators after tag name: space, tab, newline, '>', '/'
        if !matches!(next_char, ' ' | '\t' | '\n' | '\r' | '>' | '/') {
            return None; // Not a complete tag match
        }
    }

    // Now find the closing > of this opening tag
    let mut i = start + tag_chars.len();
    while i < chars.len() {
        match chars[i] {
            '>' => return Some(i),
            // Handle quoted attributes
            '"' | '\'' => {
                let quote = chars[i];
                i += 1;
                while i < chars.len() && chars[i] != quote {
                    i += 1;
                }
                if i >= chars.len() {
                    return None; // Unclosed quote
                }
            }
            // Check for self-closing tag
            '/' => {
                if i + 1 < chars.len() && chars[i + 1] == '>' {
                    return None; // This is self-closing, not an opening tag
                }
            }
            _ => {}
        }
        i += 1;
    }

    None
}

/// Remove HTML/XML comments and backtick-quoted content from content to avoid false positives
fn remove_comments(content: &str) -> String {
    let mut result = String::new();
    let mut chars = content.chars().peekable();
    let mut in_backticks = false;

    while let Some(ch) = chars.next() {
        if !in_backticks && ch == '`' {
            // Start of backtick-quoted content (ignore tags inside)
            in_backticks = true;
            continue;
        } else if in_backticks && ch == '`' {
            // End of backtick-quoted content
            in_backticks = false;
            continue;
        } else if in_backticks {
            // Skip content inside backticks (don't add to result)
            continue;
        } else if ch == '<' {
            // Check for comment start
            if chars.peek() == Some(&'!') {
                chars.next(); // consume '!'
                if chars.peek() == Some(&'-') {
                    chars.next(); // consume first '-'
                    if chars.peek() == Some(&'-') {
                        chars.next(); // consume second '-'
                        // We're in a comment, skip until -->
                        let mut found_end = false;
                        while let Some(comment_ch) = chars.next() {
                            if comment_ch == '-' && chars.peek() == Some(&'-') {
                                chars.next(); // consume second '-'
                                if chars.peek() == Some(&'>') {
                                    chars.next(); // consume '>'
                                    found_end = true;
                                    break;
                                }
                            }
                        }
                        if !found_end {
                            // Unclosed comment, treat as incomplete content
                            return String::new();
                        }
                        continue;
                    } else {
                        // Not a comment, add back the consumed characters
                        result.push('<');
                        result.push('!');
                        result.push('-');
                        continue;
                    }
                } else {
                    // Not a comment, add back the consumed characters
                    result.push('<');
                    result.push('!');
                    continue;
                }
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_tags_closed_empty_config() {
        assert!(check_tags_closed("any content", ""));
        assert!(check_tags_closed("any content", "   "));
    }

    #[test]
    fn test_check_tags_closed_balanced() {
        assert!(check_tags_closed("<div>content</div>", "div"));
        assert!(check_tags_closed(
            "<div>content</div><span>test</span>",
            "div,span"
        ));
        // In the new top-level logic, only div is checked as top-level tag
        // span is nested inside div and not checked for balance
        assert!(check_tags_closed(
            "<div><span>nested</span></div>",
            "div,span"
        ));
    }

    #[test]
    fn test_check_tags_closed_self_closing() {
        assert!(check_tags_closed("<br/>", "br"));
        assert!(check_tags_closed("<img src='test'/>", "img"));
        assert!(check_tags_closed("<div>content</div><br/>", "div,br"));
    }

    #[test]
    fn test_check_tags_closed_unbalanced() {
        assert!(!check_tags_closed("<div>content", "div"));
        assert!(!check_tags_closed("<div>content</span>", "div"));
        // In the new logic, only div is checked as top-level. Since div is properly closed,
        // and span only appears as nested tag, this should pass now
        assert!(check_tags_closed("<div><span>nested</div>", "span"));
        // But if we're checking for div, it should fail because div is not closed properly
        assert!(!check_tags_closed("<div><span>nested</div>", "div"));
    }

    #[test]
    fn test_check_tags_closed_with_attributes() {
        assert!(check_tags_closed("<div class='test'>content</div>", "div"));
        assert!(check_tags_closed(
            "<span id=\"test\" class='highlight'>text</span>",
            "span"
        ));
    }

    #[test]
    fn test_check_tags_closed_no_tags_present() {
        assert!(check_tags_closed("just plain text", "div,span"));
        assert!(check_tags_closed("no tags here at all", "p,code"));
    }

    #[test]
    fn test_check_tags_closed_with_comments() {
        assert!(check_tags_closed(
            "<!-- comment --><div>content</div>",
            "div"
        ));
        // In comments, tags are ignored. Only div is at top level and is balanced
        assert!(check_tags_closed(
            "<div><!-- <span>inside comment</span> --></div>",
            "div,span"
        ));
        // Unclosed comment should return false (content is incomplete)
        assert!(!check_tags_closed(
            "<!-- unclosed comment <div>content</div>",
            "div"
        ));
    }

    #[test]
    fn test_check_tags_closed_case_insensitive() {
        assert!(check_tags_closed("<DIV>content</DIV>", "div"));
        assert!(check_tags_closed("<Div>content</Div>", "div"));
    }

    #[test]
    fn test_check_required_tags_exist_empty_config() {
        assert!(check_required_tags_exist("any content", ""));
        assert!(check_required_tags_exist("any content", "   "));
    }

    #[test]
    fn test_check_required_tags_exist_all_present_and_closed() {
        assert!(check_required_tags_exist("<answer>yes</answer>", "answer"));
        assert!(check_required_tags_exist(
            "<answer>yes</answer><thinking>process</thinking>",
            "answer,thinking"
        ));
        // In the new logic, answer must be at top level. Here div is top-level and answer is nested,
        // so this should fail
        assert!(!check_required_tags_exist(
            "<div><answer>nested</answer></div>",
            "answer"
        ));
        // But this should pass because div is top-level and answer is also at top level
        assert!(check_required_tags_exist(
            "<div></div><answer>nested</answer>",
            "div,answer"
        ));
    }

    #[test]
    fn test_check_required_tags_exist_missing_tags() {
        assert!(!check_required_tags_exist("no tags here", "answer"));
        assert!(!check_required_tags_exist("<answer>yes</answer>", "answer,thinking"));
        assert!(!check_required_tags_exist("<thinking>process</thinking>", "answer"));
    }

    #[test]
    fn test_check_required_tags_exist_unclosed_tags() {
        assert!(!check_required_tags_exist("<answer>incomplete", "answer"));
        assert!(!check_required_tags_exist(
            "<answer>yes</answer><thinking>incomplete",
            "answer,thinking"
        ));
    }

    #[test]
    fn test_check_required_tags_exist_with_self_closing() {
        assert!(check_required_tags_exist("<br/>", "br"));
        assert!(check_required_tags_exist("<answer>yes</answer><br/>", "answer,br"));
    }

    #[test]
    fn test_check_tags_closed_top_level_logic() {
        // Content tag contains answer tag - content is top level and balanced
        assert!(check_tags_closed("<content><answer></answer></content>", "content,answer"));
        
        // Answer tag contains content tag - answer is top level and balanced
        assert!(check_tags_closed("<answer><content></content></answer>", "content,answer"));
        
        // Content tag with unclosed answer inside - content is top level and balanced (answer is nested)
        assert!(check_tags_closed("<content><answer></content>", "content,answer"));
        
        // Both tags at top level and balanced
        assert!(check_tags_closed("<content></content><answer></answer>", "content,answer"));
        
        // Cross-tag closure - should fail because answer tries to close outside content
        assert!(!check_tags_closed("<content><answer></content></answer>", "content,answer"));
        
        // Unclosed top-level tag should fail
        assert!(!check_tags_closed("<content><answer></answer>", "content"));
    }

    #[test]
    fn test_check_required_tags_exist_top_level_logic() {
        // Required tag exists at top level - should pass
        assert!(check_required_tags_exist("<answer><content></content></answer>", "answer"));
        
        // Required tag exists but only nested inside another specified tag - should fail
        assert!(!check_required_tags_exist("<content><answer></answer></content>", "answer"));
        
        // Multiple required tags both at top level - should pass
        assert!(check_required_tags_exist("<content></content><answer></answer>", "content,answer"));
        
        // One required tag at top level, one nested - should fail
        assert!(!check_required_tags_exist("<content><answer></answer></content>", "content,answer"));
        
        // Required tag at top level but unclosed - should fail
        assert!(!check_required_tags_exist("<answer><content></content>", "answer"));
    }

    #[test]
    fn test_analyze_top_level_tags_function() {
        // Test the core analyze function directly
        let result = analyze_top_level_tags("<content><answer></answer></content>", &["content", "answer"]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "content");
        assert_eq!(result[0].1, true); // content is balanced
        
        // Test cross-tag closure
        let result = analyze_top_level_tags("<content><answer></content></answer>", &["content", "answer"]);
        assert_eq!(result.len(), 2);
        // Both should be marked as unbalanced due to improper nesting
        assert!(result.iter().any(|(name, balanced)| name == "content" && !balanced));
        assert!(result.iter().any(|(name, balanced)| name == "answer" && !balanced));
    }
}

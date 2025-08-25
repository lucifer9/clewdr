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

    for tag in tags {
        if !is_tag_balanced(content, tag) {
            info!("[TAG_CHECK] Tag '{}' is not balanced", tag);
            return false;
        }
    }

    info!("[TAG_CHECK] All tags are properly balanced");
    true
}

/// Check if a specific tag is balanced in the content
/// Handles self-closing tags and comments properly
fn is_tag_balanced(content: &str, tag: &str) -> bool {
    let opening_tag = format!("<{tag}");
    let closing_tag = format!("</{tag}>");
    let self_closing_tag = format!("<{tag}/>");

    // Check comment balance first
    if content.contains("<!--") {
        let comment_starts = content.matches("<!--").count();
        let comment_ends = content.matches("-->").count();
        if comment_starts != comment_ends {
            info!(
                "[TAG_CHECK] Unbalanced comments detected: {} starts, {} ends",
                comment_starts, comment_ends
            );
            return false;
        }
    }

    // Remove comments to avoid false positives
    let content_without_comments = remove_comments(content);

    // If remove_comments returned empty string due to malformed comments,
    // treat as incomplete content
    if content.contains("<!--") && content_without_comments.is_empty() {
        return false;
    }

    let mut stack = Vec::new();
    let mut i = 0;
    let chars: Vec<char> = content_without_comments.chars().collect();

    while i < chars.len() {
        if chars[i] == '<' {
            // Try to match self-closing tag first
            if let Some(end_pos) = find_tag_end(&chars, i, &self_closing_tag) {
                // Self-closing tag found, continue
                i = end_pos + 1;
                continue;
            }

            // Try to match closing tag
            if let Some(end_pos) = find_tag_end(&chars, i, &closing_tag) {
                if stack.is_empty() {
                    // Closing tag without opening tag
                    return false;
                }
                stack.pop();
                i = end_pos + 1;
                continue;
            }

            // Try to match opening tag
            if let Some(end_pos) = find_opening_tag_end(&chars, i, &opening_tag) {
                stack.push(tag);
                i = end_pos + 1;
                continue;
            }
        }
        i += 1;
    }

    // All tags should be closed
    stack.is_empty()
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
        assert!(!check_tags_closed("<div><span>nested</div>", "span"));
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
}

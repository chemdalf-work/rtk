//! Log deduplication engine — collapses repetitive output before terse scoring.
//!
//! Strips timestamps, detects block separators, collapses consecutive identical lines.
//! Error/critical/fatal lines are always preserved.

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref TIMESTAMP_RE: Regex =
        Regex::new(r"^\[?\d{4}[-/]\d{2}[-/]\d{2}[T ]\d{2}:\d{2}:\d{2}[^\]\s]*\]?\s*").unwrap();
}

fn is_block_separator(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    if t.len() >= 3 && t.chars().all(|c| c == '=' || c == '-') {
        return true;
    }
    if t.starts_with("===") || t.starts_with("---") {
        return true;
    }
    if t.starts_with("commit ")
        && t.len() >= 12
        && t[7..].starts_with(|c: char| c.is_ascii_hexdigit())
    {
        return true;
    }
    if t.starts_with("diff --git ") {
        return true;
    }
    if t.starts_with("##") || t.starts_with("Step ") || t.starts_with("STEP ") {
        return true;
    }
    false
}

fn is_error_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("error")
        || lower.contains("critical")
        || lower.contains("fatal")
        || lower.contains("panic")
}

struct Block {
    separator: Option<String>,
    entries: Vec<(String, u32)>,
}

/// Compress repetitive log output by deduplicating consecutive identical lines.
/// Returns `None` if input is too short or no dedup benefit.
pub fn compress(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 10 {
        return None;
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut current = Block {
        separator: None,
        entries: Vec::new(),
    };
    let mut error_lines: Vec<String> = Vec::new();
    let total_lines = lines.len();

    for line in &lines {
        let stripped = TIMESTAMP_RE.replace(line, "").trim().to_string();
        if stripped.is_empty() {
            continue;
        }

        if is_block_separator(&stripped) {
            if !current.entries.is_empty() || current.separator.is_some() {
                blocks.push(current);
            }
            current = Block {
                separator: Some(stripped.clone()),
                entries: Vec::new(),
            };
            continue;
        }

        if is_error_line(&stripped) {
            error_lines.push(stripped.clone());
        }

        if let Some(last) = current.entries.last_mut() {
            if last.0 == stripped {
                last.1 += 1;
                continue;
            }
        }
        current.entries.push((stripped, 1));
    }
    if !current.entries.is_empty() || current.separator.is_some() {
        blocks.push(current);
    }

    let total_unique: usize = blocks.iter().map(|b| b.entries.len()).sum();
    let has_multiple_blocks = blocks.len() > 1;

    let mut parts = Vec::new();
    parts.push(format!("{total_lines} lines → {total_unique} unique"));

    if !error_lines.is_empty() {
        parts.push(format!("{} errors:", error_lines.len()));
        for e in error_lines.iter().take(5) {
            parts.push(format!("  {e}"));
        }
        if error_lines.len() > 5 {
            parts.push(format!("  ... +{} more errors", error_lines.len() - 5));
        }
    }

    let mut formatted: Vec<String> = Vec::new();
    for block in &blocks {
        if let Some(sep) = &block.separator {
            formatted.push(sep.clone());
        }
        for (line, count) in &block.entries {
            if *count > 1 {
                formatted.push(format!("{line} (x{count})"));
            } else {
                formatted.push(line.clone());
            }
        }
    }

    if !has_multiple_blocks && formatted.len() > 30 {
        let tail = &formatted[formatted.len() - 15..];
        parts.push(format!("last 15 unique lines:\n{}", tail.join("\n")));
    } else if has_multiple_blocks && formatted.len() > 20 {
        for line in formatted.iter().take(5) {
            parts.push(line.clone());
        }
        let omitted = formatted.len() - 10;
        parts.push(format!("[{omitted} lines omitted]"));
        for line in formatted.iter().skip(formatted.len() - 5) {
            parts.push(line.clone());
        }
    } else {
        for line in &formatted {
            parts.push(line.clone());
        }
    }

    let result = parts.join("\n");
    if result.len() >= output.len() {
        return None;
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_returns_none() {
        let output = "line1\nline2\nline3";
        assert!(compress(output).is_none());
    }

    #[test]
    fn deduplicates_consecutive_lines() {
        let lines = vec!["INFO Processing request"; 15];
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(result.contains("(x15)"), "must show repeat count: {result}");
        assert!(
            result.contains("15 lines"),
            "must show total lines: {result}"
        );
    }

    #[test]
    fn respects_block_separators() {
        let mut lines = vec!["=== commit aaaa001 ==="];
        lines.extend(vec!["file_a.rs | 10 +++++"; 5]);
        lines.push("=== commit aaaa002 ===");
        lines.extend(vec!["file_b.rs | 20 ++++++++++"; 5]);
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(
            result.contains("=== commit aaaa001 ==="),
            "first block separator preserved: {result}"
        );
        assert!(
            result.contains("=== commit aaaa002 ==="),
            "second block separator preserved: {result}"
        );
    }

    #[test]
    fn error_lines_preserved() {
        let mut lines = vec!["ok line"; 12];
        lines.push("ERROR: something failed");
        lines.extend(vec!["ok line"; 5]);
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(result.contains("1 errors:"), "error count shown: {result}");
        assert!(
            result.contains("ERROR: something failed"),
            "error line preserved: {result}"
        );
    }

    #[test]
    fn strips_timestamps() {
        let mut lines = Vec::new();
        for i in 0..15 {
            lines.push(format!("[2024-01-15T10:30:{:02}Z] Processing item", i));
        }
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(
            result.contains("(x15)"),
            "identical after timestamp strip: {result}"
        );
    }

    #[test]
    fn does_not_merge_across_blocks() {
        let lines = vec![
            "=== block 1 ===",
            "same line",
            "same line",
            "same line",
            "=== block 2 ===",
            "same line",
            "same line",
            "=== block 3 ===",
            "same line",
            "same line",
            "different line here",
        ];
        let output = lines.join("\n");
        let result = compress(&output).unwrap();
        assert!(result.contains("=== block 1 ==="), "block 1: {result}");
        assert!(result.contains("=== block 2 ==="), "block 2: {result}");
        assert!(result.contains("=== block 3 ==="), "block 3: {result}");
    }

    #[test]
    fn token_savings_on_repetitive_log() {
        let mut lines = Vec::new();
        for i in 0..100 {
            lines.push(format!("[2024-01-15T10:30:00Z] Compiling crate_{}", i % 5));
        }
        let output = lines.join("\n");
        let result = compress(&output).unwrap();

        fn count_tokens(s: &str) -> usize {
            s.split_whitespace().count()
        }
        let savings = 100.0 - (count_tokens(&result) as f64 / count_tokens(&output) as f64 * 100.0);
        assert!(
            savings >= 60.0,
            "expected >=60% savings on repetitive log, got {:.1}%",
            savings
        );
    }
}

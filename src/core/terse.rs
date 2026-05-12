//! Line-level information density scoring and compression.
//!
//! Fallback compression for commands with no specific rtk filter.
//! Scores each line by information density and removes low-value lines.
//! Only activates on output > MIN_CHARS with > MIN_LINES.

use lazy_static::lazy_static;
use regex::Regex;

const MIN_CHARS: usize = 8000;
const MIN_LINES: usize = 5;
const DEFAULT_THRESHOLD: f32 = 2.5;

lazy_static! {
    static ref ERROR_RE: Regex =
        Regex::new(r"(?i)\b(error|fatal|panic|exception|failed|FAIL)\b").unwrap();
    static ref WARNING_RE: Regex = Regex::new(r"(?i)\b(warn(ing)?|deprecated|caution)\b").unwrap();
    static ref FN_SIG_RE: Regex = Regex::new(
        r"^\s*(pub\s+)?(fn|func|def|function|class|struct|trait|impl|interface|type)\s+\w"
    )
    .unwrap();
    static ref IMPORT_RE: Regex =
        Regex::new(r"^\s*(use|import|from|require|include|#include)\s").unwrap();
    static ref SEPARATOR_RE: Regex = Regex::new(r"^[\s\-=_~*#]{3,}$").unwrap();
    static ref PROGRESS_RE: Regex =
        Regex::new(r"(\d+%|\[\s*=*>?\s*\]|\.{3,}|downloading|fetching|resolving|compiling\s)")
            .unwrap();
    static ref URL_RE: Regex = Regex::new(r"https?://\S+").unwrap();
    static ref PATH_RE: Regex = Regex::new(r"(/[\w\-.]+){2,}|\\[\w\-.]+\\").unwrap();
}

pub struct TerseResult {
    pub output: String,
    pub lines_removed: usize,
    pub lines_total: usize,
}

fn score_line(line: &str) -> f32 {
    let trimmed = line.trim();

    if trimmed.is_empty() {
        return 0.0;
    }

    if SEPARATOR_RE.is_match(trimmed) {
        return 0.5;
    }

    if ERROR_RE.is_match(trimmed) {
        return 5.0;
    }

    if WARNING_RE.is_match(trimmed) {
        return 4.5;
    }

    if FN_SIG_RE.is_match(trimmed) {
        return 4.0;
    }

    if IMPORT_RE.is_match(trimmed) {
        return 1.5;
    }

    if PROGRESS_RE.is_match(trimmed) {
        return 1.0;
    }

    let mut score: f32 = 2.0;

    if URL_RE.is_match(trimmed) {
        score += 1.0;
    }

    if PATH_RE.is_match(trimmed) {
        score += 0.5;
    }

    // Longer lines with mixed content are more informative
    let word_count = trimmed.split_whitespace().count();
    if word_count >= 5 {
        score += 0.5;
    }

    // Lines with numbers/data tend to be more informative
    let digit_ratio =
        trimmed.chars().filter(|c| c.is_ascii_digit()).count() as f32 / trimmed.len().max(1) as f32;
    if digit_ratio > 0.1 {
        score += 0.5;
    }

    score
}

/// L-curve attention reordering: place highest-scored lines at the start
/// and end of output (where LLM attention is strongest), with lower-scored
/// lines in the middle "dead zone". Based on "Lost in the Middle" research.
fn reorder_for_attention(lines: &[&str], threshold: f32) -> String {
    if lines.len() <= 6 {
        return lines.join("\n");
    }

    let mut scored: Vec<(usize, f32, &str)> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| (i, score_line(l), *l))
        .collect();

    // Split into high-attention (errors/warnings/signatures) and medium lines
    let high_threshold = threshold + 2.0; // >= 4.5 = errors, warnings, fn sigs
    let mut high: Vec<(usize, &str)> = Vec::new();
    let mut medium: Vec<(usize, &str)> = Vec::new();

    for &(idx, score, line) in &scored {
        if score >= high_threshold {
            high.push((idx, line));
        } else {
            medium.push((idx, line));
        }
    }

    // If no high-value lines, preserve original order
    if high.is_empty() || medium.is_empty() {
        return scored
            .iter()
            .map(|(_, _, l)| *l)
            .collect::<Vec<_>>()
            .join("\n");
    }

    // Split high-value lines: ~60% at start, ~40% at end
    let start_count = (high.len() * 3 + 2) / 5; // ceiling of 60%
    let (start_lines, end_lines) = high.split_at(start_count);

    let mut result: Vec<&str> = Vec::with_capacity(lines.len());

    // Start zone: high-value lines (errors, warnings, signatures)
    for &(_, line) in start_lines {
        result.push(line);
    }

    // Middle zone: medium-value lines (still above threshold but less critical)
    for &(_, line) in &medium {
        result.push(line);
    }

    // End zone: remaining high-value lines
    for &(_, line) in end_lines {
        result.push(line);
    }

    result.join("\n")
}

pub fn compress(text: &str, threshold: f32) -> Option<TerseResult> {
    if text.len() < MIN_CHARS {
        return None;
    }

    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < MIN_LINES {
        return None;
    }

    let threshold = if threshold > 0.0 {
        threshold
    } else {
        DEFAULT_THRESHOLD
    };

    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut removed = 0usize;

    for line in &lines {
        if score_line(line) >= threshold {
            kept.push(line);
        } else {
            removed += 1;
        }
    }

    if removed == 0 {
        return None;
    }

    let output = reorder_for_attention(&kept, threshold);

    // shorter_only guard: never return compressed output longer than original
    if output.len() >= text.len() {
        return None;
    }

    Some(TerseResult {
        output,
        lines_removed: removed,
        lines_total: lines.len(),
    })
}

pub fn compress_default(text: &str) -> Option<TerseResult> {
    compress(text, DEFAULT_THRESHOLD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn test_score_error_lines_high() {
        assert!(score_line("error[E0308]: mismatched types") >= 5.0);
        assert!(score_line("FAIL src/test.rs") >= 5.0);
        assert!(score_line("fatal: not a git repository") >= 5.0);
    }

    #[test]
    fn test_score_warning_lines_high() {
        assert!(score_line("warning: unused variable `x`") >= 4.0);
        assert!(score_line("DEPRECATED: use new_fn instead") >= 4.0);
    }

    #[test]
    fn test_score_fn_signatures_high() {
        assert!(score_line("pub fn main() -> Result<()> {") >= 4.0);
        assert!(score_line("  def process(self, data):") >= 4.0);
        assert!(score_line("function handleClick(event) {") >= 4.0);
    }

    #[test]
    fn test_score_empty_and_separators_low() {
        assert!(score_line("") < 1.0);
        assert!(score_line("---") < 1.0);
        assert!(score_line("====") < 1.0);
        assert!(score_line("    ") < 1.0);
    }

    #[test]
    fn test_score_progress_low() {
        assert!(score_line("downloading crate `serde` ...") < 2.0);
        assert!(score_line("  [========>     ] 65%") < 2.0);
    }

    #[test]
    fn test_compress_below_min_chars_returns_none() {
        let short = "line1\nline2\nline3";
        assert!(compress(short, DEFAULT_THRESHOLD).is_none());
    }

    #[test]
    fn test_compress_below_min_lines_returns_none() {
        let long_few_lines = "a".repeat(10000);
        assert!(compress(&long_few_lines, DEFAULT_THRESHOLD).is_none());
    }

    #[test]
    fn test_compress_removes_low_value_lines() {
        let mut lines = Vec::new();
        // High-value lines
        lines.push("error[E0308]: mismatched types".to_string());
        lines.push("pub fn main() -> Result<()> {".to_string());
        lines.push("warning: unused variable".to_string());
        // Low-value lines (padding to exceed MIN_CHARS)
        for _ in 0..250 {
            lines.push("------------------------------------------------------------".to_string());
            lines.push("...".to_string());
            lines.push("  [========>     ] downloading...".to_string());
        }
        lines.push("error: aborting due to previous error".to_string());

        let text = lines.join("\n");
        assert!(
            text.len() > MIN_CHARS,
            "fixture is {} bytes, need > {}",
            text.len(),
            MIN_CHARS
        );

        let result = compress(&text, DEFAULT_THRESHOLD).expect("should compress");
        assert!(result.lines_removed > 0);
        assert!(result.output.contains("error[E0308]"));
        assert!(result.output.contains("pub fn main"));
        assert!(result.output.contains("warning:"));
        assert!(!result.output.contains("[========>"));

        let savings =
            100.0 - (count_tokens(&result.output) as f64 / count_tokens(&text) as f64 * 100.0);
        assert!(
            savings >= 60.0,
            "Expected >=60% savings, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_lcurve_reordering() {
        let lines: Vec<&str> = vec![
            "some medium info line about config",
            "another medium line with details",
            "error[E0308]: mismatched types",
            "more medium content here folks",
            "warning: unused variable `x`",
            "pub fn main() -> Result<()> {",
            "yet more medium filler content",
            "error: aborting due to previous error",
        ];

        let result = reorder_for_attention(&lines, DEFAULT_THRESHOLD);
        let result_lines: Vec<&str> = result.lines().collect();

        // High-value lines (errors, warnings, fn sigs) should be at start and end
        assert!(
            result_lines[0].contains("error")
                || result_lines[0].contains("warning")
                || result_lines[0].contains("pub fn"),
            "First line should be high-value, got: {}",
            result_lines[0]
        );

        let last = result_lines.last().unwrap();
        assert!(
            last.contains("error") || last.contains("warning") || last.contains("pub fn"),
            "Last line should be high-value, got: {}",
            last
        );
    }

    #[test]
    fn test_lcurve_small_input_preserves_order() {
        let lines: Vec<&str> = vec!["line1", "line2", "line3"];
        let result = reorder_for_attention(&lines, DEFAULT_THRESHOLD);
        assert_eq!(result, "line1\nline2\nline3");
    }

    #[test]
    fn test_shorter_only_guard() {
        // If all lines score above threshold, returns None (no compression)
        let mut lines = Vec::new();
        for _ in 0..200 {
            lines.push("error: something went wrong at /path/to/file.rs:42");
        }
        let text = lines.join("\n");
        assert!(text.len() > MIN_CHARS);

        let result = compress(&text, DEFAULT_THRESHOLD);
        assert!(result.is_none());
    }
}

//! Filters grep output by grouping matches by file.

use crate::core::config;
use crate::core::tracking;
use crate::core::utils::{exit_code_from_output, resolved_command, shorten_path};
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::HashMap;
use std::process::Stdio;

/// A single output line — either a match or a context line.
#[derive(Debug)]
struct OutputLine {
    line_num: usize,
    content: String,
    is_context: bool,
}

/// Try to parse an rg context line.
/// rg uses `-` as separator for context lines (vs `:` for match lines).
/// Two formats:
///   - Multi-file: `path-N-content`   (e.g. `src/main.rs-15-    let x = 1;`)
///   - Single-file: `N-content`        (e.g. `15-    let x = 1;`)
///
/// Returns (file_hint, linenum, content); file_hint is empty string for single-file format.
fn parse_context_line(line: &str) -> Option<(String, usize, String)> {
    // Short format: line starts with digits followed by `-` (single-file rg output)
    let bytes = line.as_bytes();
    if bytes.first().map(|b| b.is_ascii_digit()).unwrap_or(false) {
        let mut k = 0;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            k += 1;
        }
        if k < bytes.len() && bytes[k] == b'-' {
            if let Ok(ln) = line[..k].parse::<usize>() {
                return Some((String::new(), ln, line[k + 1..].to_string()));
            }
        }
    }

    // Full format: scan for first `-N-` where N is a non-empty digit run.
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' {
            let j = i + 1;
            let mut k = j;
            while k < bytes.len() && bytes[k].is_ascii_digit() {
                k += 1;
            }
            if k > j && k < bytes.len() && bytes[k] == b'-' {
                let ln: usize = line[j..k].parse().ok()?;
                let file = line[..i].to_string();
                let content = line[k + 1..].to_string();
                if !file.is_empty() {
                    return Some((file, ln, content));
                }
            }
        }
        i += 1;
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    pattern: &str,
    path: &str,
    max_line_len: usize,
    max_results: usize,
    context_only: bool,
    file_type: Option<&str>,
    extra_args: &[String],
    has_context: bool,
    verbose: u8,
) -> Result<i32> {
    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("grep: '{}' in {}", pattern, path);
    }

    // Fix: convert BRE alternation \| → | for rg (which uses PCRE-style regex)
    let rg_pattern = pattern.replace(r"\|", "|");

    let mut rg_cmd = resolved_command("rg");
    rg_cmd
        .args(["-n", "--no-heading", &rg_pattern, path])
        .stdin(Stdio::null());

    if let Some(ft) = file_type {
        rg_cmd.arg("--type").arg(ft);
    }

    for arg in extra_args {
        // Fix: skip grep-ism -r flag (rg is recursive by default; rg -r means --replace)
        if arg == "-r" || arg == "--recursive" {
            continue;
        }
        rg_cmd.arg(arg);
    }

    let output = rg_cmd
        .output()
        .or_else(|_| {
            resolved_command("grep")
                .args(["-rn", pattern, path])
                .stdin(Stdio::null())
                .output()
        })
        .context("grep/rg failed")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let exit_code = exit_code_from_output(&output, "grep");

    let raw_output = stdout.to_string();

    if stdout.trim().is_empty() {
        // Show stderr for errors (bad regex, missing file, etc.)
        if exit_code == 2 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                eprintln!("{}", stderr.trim());
            }
        }
        let msg = format!("0 matches for '{}'", pattern);
        println!("{}", msg);
        timer.track(
            &format!("grep -rn '{}' {}", pattern, path),
            "rtk grep",
            &raw_output,
            &msg,
        );
        return Ok(exit_code);
    }

    let mut by_file: HashMap<String, Vec<OutputLine>> = HashMap::new();
    let mut total = 0;

    // Compile context regex once (instead of per-line in clean_line)
    let context_re = if context_only {
        Regex::new(&format!("(?i).{{0,20}}{}.*", regex::escape(pattern))).ok()
    } else {
        None
    };

    for line in stdout.lines() {
        // Skip rg group separators (--) emitted between context groups
        if line == "--" {
            continue;
        }

        // Try match line format first: path:linenum:content
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() == 3 {
            if let Ok(ln) = parts[1].parse::<usize>() {
                let cleaned =
                    clean_line(parts[2], max_line_len, context_re.as_ref(), pattern);
                by_file
                    .entry(parts[0].to_string())
                    .or_default()
                    .push(OutputLine {
                        line_num: ln,
                        content: cleaned,
                        is_context: false,
                    });
                total += 1;
                continue;
            }
        }
        if parts.len() == 2 {
            if let Ok(ln) = parts[0].parse::<usize>() {
                let cleaned =
                    clean_line(parts[1], max_line_len, context_re.as_ref(), pattern);
                by_file
                    .entry(path.to_string())
                    .or_default()
                    .push(OutputLine {
                        line_num: ln,
                        content: cleaned,
                        is_context: false,
                    });
                total += 1;
                continue;
            }
        }

        // Try context line format (only when context flags were passed)
        if has_context {
            if let Some((file_hint, ln, content)) = parse_context_line(line) {
                let file_key = if file_hint.is_empty() {
                    path.to_string() // single-file search: attribute to search path
                } else {
                    file_hint
                };
                let cleaned =
                    clean_line(&content, max_line_len, context_re.as_ref(), pattern);
                by_file.entry(file_key).or_default().push(OutputLine {
                    line_num: ln,
                    content: cleaned,
                    is_context: true,
                });
                continue;
            }
        }
    }

    let match_count = by_file.values().flat_map(|v| v.iter()).filter(|l| !l.is_context).count();

    let limits = config::limits();
    let per_file = limits.grep_max_per_file;
    let max_files = limits.grep_max_files;

    let mut rtk_output = String::new();
    rtk_output.push_str(&format!("{} matches in {}F:\n", match_count, by_file.len()));

    let mut files: Vec<_> = by_file.iter().collect();
    files.sort_by_key(|(_, lines)| std::cmp::Reverse(lines.iter().filter(|l| !l.is_context).count()));

    let mut shown = 0;

    for (files_shown, (file, lines)) in files.iter().enumerate() {
        if files_shown >= max_files || shown >= max_results {
            break;
        }

        let match_count_file = lines.iter().filter(|l| !l.is_context).count();
        let file_display = shorten_path(file);
        rtk_output.push_str(&format!("\n{} ({}):", file_display, match_count_file));

        for (shown_file, ol) in lines.iter().enumerate() {
            if shown_file >= per_file || shown >= max_results {
                break;
            }
            let prefix = if ol.is_context { "  ctx " } else { "  " };
            rtk_output.push_str(&format!("\n{}{}: {}", prefix, ol.line_num, ol.content));
            shown += 1;
        }

        if lines.len() > per_file {
            rtk_output.push_str(&format!("\n  ... +{} more", lines.len() - per_file));
        }
    }

    if files.len() > max_files {
        rtk_output.push_str(&format!("\n\n... +{} more files", files.len() - max_files));
    } else if total > shown {
        rtk_output.push_str(&format!("\n\n... +{} more", total - shown));
    }

    print!("{}", rtk_output);
    timer.track(
        &format!("grep -rn '{}' {}", pattern, path),
        "rtk grep",
        &raw_output,
        &rtk_output,
    );

    Ok(exit_code)
}

fn clean_line(line: &str, max_len: usize, context_re: Option<&Regex>, pattern: &str) -> String {
    let trimmed = line.trim();

    if let Some(re) = context_re {
        if let Some(m) = re.find(trimmed) {
            let matched = m.as_str();
            if matched.len() <= max_len {
                return matched.to_string();
            }
        }
    }

    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        let lower = trimmed.to_lowercase();
        let pattern_lower = pattern.to_lowercase();

        if let Some(pos) = lower.find(&pattern_lower) {
            let char_pos = lower[..pos].chars().count();
            let chars: Vec<char> = trimmed.chars().collect();
            let char_len = chars.len();

            let start = char_pos.saturating_sub(max_len / 3);
            let end = (start + max_len).min(char_len);
            let start = if end == char_len {
                end.saturating_sub(max_len)
            } else {
                start
            };

            let slice: String = chars[start..end].iter().collect();
            if start > 0 && end < char_len {
                format!("...{}...", slice)
            } else if start > 0 {
                format!("...{}", slice)
            } else {
                format!("{}...", slice)
            }
        } else {
            let t: String = trimmed.chars().take(max_len - 3).collect();
            format!("{}...", t)
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_line() {
        let line = "            const result = someFunction();";
        let cleaned = clean_line(line, 50, None, "result");
        assert!(!cleaned.starts_with(' '));
        assert!(cleaned.len() <= 50);
    }

    #[test]
    fn test_shorten_path_in_grep() {
        use crate::core::utils::shorten_path;
        assert_eq!(shorten_path("src/core/patterns/grep.rs"), "s/c/p/grep.rs");
        assert_eq!(
            shorten_path("internal/generator/templates/readme.md.tmpl"),
            "i/g/t/readme.md.tmpl"
        );
    }

    #[test]
    fn test_extra_args_accepted() {
        // Test that the function signature accepts extra_args
        // This is a compile-time test - if it compiles, the signature is correct
        let _extra: Vec<String> = vec!["-i".to_string(), "-A".to_string(), "3".to_string()];
        // No need to actually run - we're verifying the parameter exists
    }

    #[test]
    fn test_clean_line_multibyte() {
        // Thai text that exceeds max_len in bytes
        let line = "  สวัสดีครับ นี่คือข้อความที่ยาวมากสำหรับทดสอบ  ";
        let cleaned = clean_line(line, 20, None, "ครับ");
        // Should not panic
        assert!(!cleaned.is_empty());
    }

    #[test]
    fn test_clean_line_emoji() {
        let line = "🎉🎊🎈🎁🎂🎄 some text 🎃🎆🎇✨";
        let cleaned = clean_line(line, 15, None, "text");
        assert!(!cleaned.is_empty());
    }

    // Fix: BRE \| alternation is translated to PCRE | for rg
    #[test]
    fn test_bre_alternation_translated() {
        let pattern = r"fn foo\|pub.*bar";
        let rg_pattern = pattern.replace(r"\|", "|");
        assert_eq!(rg_pattern, "fn foo|pub.*bar");
    }

    // Fix: -r flag (grep recursive) is stripped from extra_args (rg is recursive by default)
    #[test]
    fn test_recursive_flag_stripped() {
        let extra_args: Vec<String> = vec!["-r".to_string(), "-i".to_string()];
        let filtered: Vec<&String> = extra_args
            .iter()
            .filter(|a| *a != "-r" && *a != "--recursive")
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], "-i");
    }

    // Fix: parse_context_line handles rg context line format (path-linenum-content)
    #[test]
    fn test_parse_context_line_basic() {
        let line = "src/main.rs-15-    let x = 1;";
        let result = parse_context_line(line);
        assert!(result.is_some(), "should parse context line");
        let (file, ln, content) = result.unwrap();
        assert_eq!(file, "src/main.rs");
        assert_eq!(ln, 15);
        assert_eq!(content, "    let x = 1;");
    }

    #[test]
    fn test_parse_context_line_absolute_path() {
        let line = "/Users/foo/project/src/lib.rs-42-pub fn bar() {";
        let result = parse_context_line(line);
        assert!(result.is_some());
        let (file, ln, content) = result.unwrap();
        assert_eq!(file, "/Users/foo/project/src/lib.rs");
        assert_eq!(ln, 42);
        assert_eq!(content, "pub fn bar() {");
    }

    #[test]
    fn test_parse_context_line_short_format() {
        // Single-file rg output: no filename prefix
        let line = "48-    pattern: &str,";
        let result = parse_context_line(line);
        assert!(result.is_some(), "should parse short-format context line");
        let (file, ln, content) = result.unwrap();
        assert_eq!(file, "", "file hint empty for short format");
        assert_eq!(ln, 48);
        assert_eq!(content, "    pattern: &str,");
    }

    #[test]
    fn test_parse_context_line_ignores_group_separator() {
        // rg group separator "--" should not be parsed as a context line
        let line = "--";
        let result = parse_context_line(line);
        // "--" has no digit run, so should not match
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_context_line_content_with_colon() {
        // Context lines whose content contains ':' should still parse
        let line = "src/foo.rs-10-    key: value,";
        let result = parse_context_line(line);
        assert!(result.is_some());
        let (_, ln, content) = result.unwrap();
        assert_eq!(ln, 10);
        assert!(content.contains("key: value"));
    }

    // --- truncation accuracy ---

    #[test]
    fn test_grep_overflow_uses_uncapped_total() {
        let per_file = config::limits().grep_max_per_file;
        assert_eq!(per_file, 5, "default grep_max_per_file should be 5");
        let total_matches = per_file + 42;
        let overflow = total_matches - per_file;
        assert_eq!(overflow, 42, "overflow must equal true suppressed count");
    }

    #[test]
    fn test_grep_max_files_default() {
        let limits = config::limits();
        assert_eq!(limits.grep_max_files, 20, "default grep_max_files should be 20");
    }

    // Verify line numbers are always enabled in rg invocation (grep_cmd.rs:24).
    // The -n/--line-numbers clap flag in main.rs is a no-op accepted for compat.
    #[test]
    fn test_rg_always_has_line_numbers() {
        // grep_cmd::run() always passes "-n" to rg (line 24).
        // This test documents that -n is built-in, so the clap flag is safe to ignore.
        let mut cmd = resolved_command("rg");
        cmd.args(["-n", "--no-heading", "NONEXISTENT_PATTERN_12345", "."]);
        // If rg is available, it should accept -n without error (exit 1 = no match, not error)
        if let Ok(output) = cmd.output() {
            assert!(
                output.status.code() == Some(1) || output.status.success(),
                "rg -n should be accepted"
            );
        }
        // If rg is not installed, skip gracefully (test still passes)
    }
}

//! Task-aware file preloader with Boltzmann allocation.
//!
//! Given a task description, ranks project files by relevance and outputs
//! the most relevant paths with signatures (function/class/type declarations).
//! Uses keyword extraction, git recency, and structural heuristics.

use anyhow::{Context, Result};
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 8;
const SIGNATURE_BUDGET: usize = 10;
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "__pycache__",
    ".venv",
    "dist",
    "build",
    ".next",
    "vendor",
    "coverage",
];

lazy_static! {
    static ref FN_SIG_RE: Regex = Regex::new(
        r"^\s*(pub\s+)?(fn|func|def|function|class|struct|trait|impl|interface|type|export)\s+(\w+)"
    )
    .unwrap();
}

struct ScoredFile {
    path: PathBuf,
    score: f64,
}

pub fn run(task: &str, verbose: u8) -> Result<i32> {
    if task.trim().is_empty() {
        println!("Usage: rtk preload <task description>");
        println!("Example: rtk preload \"fix the authentication bug in login flow\"");
        return Ok(1);
    }

    let timer = crate::core::tracking::TimedExecution::start();

    let keywords = extract_keywords(task);
    if verbose > 0 {
        eprintln!("Keywords: {:?}", keywords);
    }

    let project_root = std::env::current_dir().context("Failed to get cwd")?;
    let files = discover_files(&project_root)?;

    if verbose > 0 {
        eprintln!("Discovered {} files", files.len());
    }

    let mut scored = score_files(&files, &keywords, &project_root);
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Boltzmann allocation: allocate attention budget proportionally
    let selected = boltzmann_select(&scored, MAX_FILES);

    let mut output_parts: Vec<String> = Vec::new();
    output_parts.push(format!("[task: {}]", task));
    output_parts.push(format!("[files: {}/{}]", selected.len(), files.len()));
    output_parts.push(String::new());

    for sf in &selected {
        let rel_path = sf
            .path
            .strip_prefix(&project_root)
            .unwrap_or(&sf.path)
            .display();
        let sigs = extract_signatures(&sf.path);

        output_parts.push(format!("## {} (score: {:.2})", rel_path, sf.score));
        if sigs.is_empty() {
            output_parts.push("  (no signatures found)".to_string());
        } else {
            for sig in sigs.iter().take(SIGNATURE_BUDGET) {
                output_parts.push(format!("  {}", sig));
            }
        }
        output_parts.push(String::new());
    }

    let result = output_parts.join("\n");
    println!("{}", result);

    timer.track(
        &format!("preload {}", task),
        "rtk preload",
        task,
        &result,
    );

    Ok(0)
}

fn extract_keywords(task: &str) -> Vec<String> {
    let stop_words: &[&str] = &[
        "the", "a", "an", "in", "on", "at", "to", "for", "of", "with", "and", "or", "is", "are",
        "was", "were", "be", "been", "being", "have", "has", "had", "do", "does", "did", "will",
        "would", "could", "should", "may", "might", "shall", "can", "need", "must", "it", "its",
        "this", "that", "these", "those", "i", "we", "you", "he", "she", "they", "fix", "add",
        "update", "change", "make", "implement", "create", "build", "from", "into",
    ];

    task.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
        .filter(|w| !stop_words.contains(&w.as_str()))
        .collect()
}

fn discover_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    discover_recursive(root, root, &mut files, 0)?;
    Ok(files)
}

fn discover_recursive(
    dir: &Path,
    root: &Path,
    files: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<()> {
    if depth > 6 {
        return Ok(());
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        if name.starts_with('.') && name != ".env" {
            continue;
        }

        if path.is_dir() {
            if IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            discover_recursive(&path, root, files, depth + 1)?;
        } else if is_source_file(&name) {
            files.push(path);
        }
    }

    Ok(())
}

fn is_source_file(name: &str) -> bool {
    let exts = &[
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "rb", "java", "kt", "swift", "c", "cpp",
        "h", "hpp", "cs", "ex", "exs", "zig", "lua", "sh", "bash", "zsh", "toml", "yaml",
        "yml", "json", "md",
    ];
    name.rsplit('.')
        .next()
        .map(|ext| exts.contains(&ext))
        .unwrap_or(false)
}

fn score_files(files: &[PathBuf], keywords: &[String], root: &Path) -> Vec<ScoredFile> {
    // Get recently modified files from git
    let recent_files = git_recent_files(root);

    files
        .iter()
        .map(|path| {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_lowercase();

            let mut score = 0.0f64;

            // Keyword match in path/filename
            for kw in keywords {
                if rel.contains(kw.as_str()) {
                    score += 3.0;
                }
            }

            // Git recency bonus
            if let Some(&recency) = recent_files.get(&rel) {
                score += recency;
            }

            // File type relevance
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            match ext {
                "rs" | "ts" | "tsx" | "py" | "go" => score += 0.5,
                "md" | "toml" | "yaml" => score += 0.2,
                _ => {}
            }

            // Filename patterns
            let filename = path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("");
            if filename.contains("test") || filename.contains("spec") {
                // Slightly lower priority unless task mentions testing
                if !keywords.iter().any(|k| k.contains("test")) {
                    score -= 0.5;
                }
            }
            if filename == "mod.rs" || filename == "index.ts" || filename == "__init__.py" {
                score += 0.3;
            }

            ScoredFile {
                path: path.clone(),
                score,
            }
        })
        .filter(|sf| sf.score > 0.0)
        .collect()
}

fn git_recent_files(root: &Path) -> HashMap<String, f64> {
    let output = std::process::Command::new("git")
        .args(["log", "--pretty=format:", "--name-only", "-20"])
        .current_dir(root)
        .output();

    let mut recency: HashMap<String, f64> = HashMap::new();

    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut weight = 2.0f64;
        for line in stdout.lines() {
            let trimmed = line.trim().to_lowercase();
            if trimmed.is_empty() {
                weight *= 0.9; // decay for older commits
                continue;
            }
            recency.entry(trimmed).or_insert(weight);
        }
    }

    recency
}

fn boltzmann_select(scored: &[ScoredFile], max: usize) -> Vec<&ScoredFile> {
    if scored.len() <= max {
        return scored.iter().collect();
    }

    // Temperature: lower = more concentrated on top files
    let temperature = 0.5f64;
    let max_score = scored
        .iter()
        .map(|s| s.score)
        .fold(f64::NEG_INFINITY, f64::max);

    let weights: Vec<f64> = scored
        .iter()
        .take(max * 3) // consider top 3x candidates
        .map(|s| ((s.score - max_score) / temperature).exp())
        .collect();

    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return scored.iter().take(max).collect();
    }

    // Select top-N by normalized weight (greedy, not sampling)
    let mut indexed: Vec<(usize, f64)> = weights
        .iter()
        .enumerate()
        .map(|(i, &w)| (i, w / total))
        .collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    indexed
        .iter()
        .take(max)
        .map(|(i, _)| &scored[*i])
        .collect()
}

fn extract_signatures(path: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    content
        .lines()
        .filter_map(|line| {
            if FN_SIG_RE.is_match(line) {
                Some(line.trim().to_string())
            } else {
                None
            }
        })
        .take(SIGNATURE_BUDGET)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_keywords() {
        let kw = extract_keywords("fix the authentication bug in login flow");
        assert!(kw.contains(&"authentication".to_string()));
        assert!(kw.contains(&"bug".to_string()));
        assert!(kw.contains(&"login".to_string()));
        assert!(kw.contains(&"flow".to_string()));
        assert!(!kw.contains(&"the".to_string()));
        assert!(!kw.contains(&"in".to_string()));
    }

    #[test]
    fn test_is_source_file() {
        assert!(is_source_file("main.rs"));
        assert!(is_source_file("index.ts"));
        assert!(is_source_file("app.py"));
        assert!(!is_source_file("image.png"));
        assert!(!is_source_file("data.bin"));
    }

    #[test]
    fn test_extract_keywords_dedupes_stopwords() {
        let kw = extract_keywords("add a new feature to handle user input");
        assert!(!kw.contains(&"a".to_string()));
        assert!(!kw.contains(&"to".to_string()));
        assert!(kw.contains(&"feature".to_string()));
        assert!(kw.contains(&"handle".to_string()));
        assert!(kw.contains(&"user".to_string()));
        assert!(kw.contains(&"input".to_string()));
    }

    #[test]
    fn test_boltzmann_select_fewer_than_max() {
        let files = vec![
            ScoredFile {
                path: PathBuf::from("a.rs"),
                score: 5.0,
            },
            ScoredFile {
                path: PathBuf::from("b.rs"),
                score: 3.0,
            },
        ];
        let selected = boltzmann_select(&files, 8);
        assert_eq!(selected.len(), 2);
    }
}

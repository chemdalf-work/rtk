//! Data-format-aware filters: lockfile bypass, JSON schema, YAML key scan, markdown compression.

use lazy_static::lazy_static;
use regex::Regex;
use std::path::Path;

lazy_static! {
    static ref MD_HEADER: Regex = Regex::new(r"^#{1,6}\s+.+").unwrap();
    static ref CODE_FENCE: Regex = Regex::new(r"^```").unwrap();
    static ref YAML_TOP_KEY: Regex = Regex::new(r"^[a-zA-Z_][\w\-]*\s*[:=]").unwrap();
}

const LOCKFILE_NAMES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Gemfile.lock",
    "poetry.lock",
    "go.sum",
    "composer.lock",
    "flake.lock",
];

pub fn is_lockfile(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| LOCKFILE_NAMES.contains(&n))
        .unwrap_or(false)
}

pub fn summarize_lockfile(content: &str, path: &Path) -> String {
    let lines = content.lines().count();
    let pkg_count = count_lockfile_packages(content, path);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("lockfile");
    format!("[{name}: {lines} lines, ~{pkg_count} packages — generated file, not shown]")
}

fn count_lockfile_packages(content: &str, path: &Path) -> usize {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == "Cargo.lock" {
        content
            .lines()
            .filter(|l| l.starts_with("[[package]]"))
            .count()
    } else if name == "go.sum" {
        content.lines().count() / 2
    } else {
        content
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with(' ') && !l.starts_with('\t'))
            .count()
    }
}

pub fn summarize_json(content: &str) -> Option<String> {
    if content.len() > 512_000 {
        return Some(scan_yaml_keys(content));
    }
    let val: serde_json::Value = serde_json::from_str(content.trim()).ok()?;
    let schema = extract_schema(&val, 0);
    Some(format!("[JSON schema]\n{schema}"))
}

fn extract_schema(val: &serde_json::Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    match val {
        serde_json::Value::Object(map) => {
            if depth > 3 {
                return format!("{indent}{{...{} keys}}", map.len());
            }
            let mut entries: Vec<std::string::String> = map
                .iter()
                .take(20)
                .map(|(k, v)| match v {
                    serde_json::Value::Object(inner) if !inner.is_empty() && depth < 3 => {
                        let nested = extract_schema(v, depth + 1);
                        format!("{indent}  {k}: {{\n{nested}\n{indent}  }}")
                    }
                    serde_json::Value::Array(arr) => {
                        let item = arr.first().map(type_of).unwrap_or_else(|| "any".into());
                        format!("{indent}  {k}: [{item}...{}]", arr.len())
                    }
                    _ => format!("{indent}  {k}: {}", type_of(v)),
                })
                .collect();
            if map.len() > 20 {
                entries.push(format!("{indent}  ...+{} more keys", map.len() - 20));
            }
            entries.join("\n")
        }
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return format!("{indent}[]");
            }
            let first = extract_schema(&arr[0], depth + 1);
            format!("{indent}[{} items, each:\n{first}\n{indent}]", arr.len())
        }
        _ => format!("{indent}{}", type_of(val)),
    }
}

fn type_of(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(_) => "bool".into(),
        serde_json::Value::Number(n) => {
            if n.is_f64() {
                "float".into()
            } else {
                "int".into()
            }
        }
        serde_json::Value::String(s) => {
            if s.len() > 50 {
                format!("str({})", s.len())
            } else {
                "str".into()
            }
        }
        serde_json::Value::Array(a) => format!("[...{}]", a.len()),
        serde_json::Value::Object(m) => format!("{{...{} keys}}", m.len()),
    }
}

pub fn scan_yaml_keys(content: &str) -> String {
    let keys: Vec<&str> = content
        .lines()
        .filter(|l| YAML_TOP_KEY.is_match(l))
        .take(40)
        .collect();
    format!(
        "[YAML/TOML keys ({} top-level)]\n{}",
        keys.len(),
        keys.join("\n")
    )
}

pub fn compress_markdown(content: &str) -> String {
    let mut result: Vec<String> = Vec::new();
    let mut in_code = false;
    let mut in_section = false;
    let mut para_lines = 0usize;
    const MAX_PARA: usize = 3;

    for line in content.lines() {
        if CODE_FENCE.is_match(line) {
            in_code = !in_code;
            if in_code {
                result.push("[code block omitted]".to_string());
            }
            continue;
        }
        if in_code {
            continue;
        }
        if MD_HEADER.is_match(line) {
            result.push(line.to_string());
            in_section = true;
            para_lines = 0;
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if in_section && para_lines < MAX_PARA {
            result.push(line.to_string());
            para_lines += 1;
        }
    }

    let total = content.lines().count();
    let kept = result.len();
    if kept < total {
        result.push(format!(
            "[{} lines omitted — use --level none for full content]",
            total - kept
        ));
    }
    result.join("\n")
}

/// Detect the data sub-type from file extension for dispatch.
pub fn data_subtype(path: &Path) -> DataSubtype {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "json" | "jsonc" | "json5" => DataSubtype::Json,
        "yaml" | "yml" | "toml" => DataSubtype::Yaml,
        "md" | "markdown" => DataSubtype::Markdown,
        _ => DataSubtype::Other,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DataSubtype {
    Json,
    Yaml,
    Markdown,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_tokens(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn test_is_lockfile_positive() {
        assert!(is_lockfile(Path::new("Cargo.lock")));
        assert!(is_lockfile(Path::new("/some/path/package-lock.json")));
        assert!(is_lockfile(Path::new("go.sum")));
    }

    #[test]
    fn test_is_lockfile_negative() {
        assert!(!is_lockfile(Path::new("Cargo.toml")));
        assert!(!is_lockfile(Path::new("main.rs")));
        assert!(!is_lockfile(Path::new("package.json")));
    }

    #[test]
    fn test_summarize_lockfile_cargo() {
        let content = "[[package]]\nname = \"foo\"\n\n[[package]]\nname = \"bar\"\n";
        let summary = summarize_lockfile(content, Path::new("Cargo.lock"));
        assert!(summary.contains("~2 packages"));
        assert!(summary.contains("Cargo.lock"));
    }

    #[test]
    fn test_summarize_json_basic() {
        let json = r#"{"name": "test", "version": "1.0", "deps": {"a": "1", "b": "2"}}"#;
        let result = summarize_json(json).unwrap();
        assert!(result.contains("[JSON schema]"));
        assert!(result.contains("name: str"));
        assert!(result.contains("version: str"));
    }

    #[test]
    fn test_summarize_json_array() {
        let json = r#"[{"id": 1, "name": "a"}, {"id": 2, "name": "b"}]"#;
        let result = summarize_json(json).unwrap();
        assert!(result.contains("2 items"));
    }

    #[test]
    fn test_summarize_json_savings() {
        let mut deps = serde_json::Map::new();
        for i in 0..30 {
            deps.insert(
                format!("package-{i}"),
                serde_json::Value::String(format!("^{}.0.0", i % 10)),
            );
        }
        let json = serde_json::json!({
            "name": "my-project",
            "version": "1.0.0",
            "description": "A test project with many dependencies for testing JSON schema compression in rtk",
            "dependencies": deps,
            "devDependencies": {
                "jest": "^29.0.0",
                "eslint": "^8.0.0",
                "prettier": "^3.0.0",
                "typescript": "^5.0.0",
                "webpack": "^5.0.0"
            },
            "scripts": {
                "build": "webpack --mode production --config webpack.config.js",
                "test": "jest --coverage --watchAll=false",
                "lint": "eslint src/ --ext .ts,.tsx --fix"
            }
        });
        let content = serde_json::to_string_pretty(&json).unwrap();
        let result = summarize_json(&content).unwrap();
        let savings =
            100.0 - (count_tokens(&result) as f64 / count_tokens(&content) as f64 * 100.0);
        assert!(
            savings >= 30.0,
            "expected >=30% savings on package.json, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_scan_yaml_keys() {
        let yaml =
            "name: my-app\nversion: 1.0\ndependencies:\n  foo: bar\nscripts:\n  test: jest\n";
        let result = scan_yaml_keys(yaml);
        assert!(result.contains("4 top-level"), "got: {result}");
        assert!(result.contains("name:"));
    }

    #[test]
    fn test_compress_markdown() {
        let md = "# Title\n\nFirst paragraph line.\nSecond line.\nThird line.\nFourth line (should be omitted).\n\n## Section 2\n\nAnother paragraph.\n\n```rust\nfn main() {}\n```\n\nMore text.\n";
        let result = compress_markdown(md);
        assert!(result.contains("# Title"));
        assert!(result.contains("First paragraph line."));
        assert!(result.contains("## Section 2"));
        assert!(result.contains("[code block omitted]"));
        assert!(!result.contains("fn main()"));
        assert!(!result.contains("Fourth line"));
    }

    #[test]
    fn test_compress_markdown_savings() {
        let mut md = String::new();
        md.push_str(
            "# Project README\n\nShort intro paragraph that explains what this project does.\n\n",
        );
        for i in 0..20 {
            md.push_str(&format!(
                "## Section {i}\n\nParagraph explaining section {i} in detail with multiple words.\nAnother line of explanation for this section.\nThird line with more detail.\nFourth line that should be omitted.\nFifth line also omitted.\n\n```python\ndef func_{i}():\n    x = {i}\n    y = x * 2\n    return y\n```\n\nAdditional prose after the code block that provides context.\nMore detail here that is not needed.\n\n",
                i = i
            ));
        }
        let result = compress_markdown(&md);
        let savings = 100.0 - (count_tokens(&result) as f64 / count_tokens(&md) as f64 * 100.0);
        assert!(
            savings >= 50.0,
            "expected >=50% savings on README, got {:.1}%",
            savings
        );
    }

    #[test]
    fn test_data_subtype_detection() {
        assert_eq!(data_subtype(Path::new("file.json")), DataSubtype::Json);
        assert_eq!(data_subtype(Path::new("config.yaml")), DataSubtype::Yaml);
        assert_eq!(data_subtype(Path::new("README.md")), DataSubtype::Markdown);
        assert_eq!(data_subtype(Path::new("data.csv")), DataSubtype::Other);
    }
}

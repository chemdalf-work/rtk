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
    let lines: Vec<&str> = content.lines().collect();

    // Detect TOML sections: [section] or [[array]]
    let has_toml_sections = lines.iter().any(|l| {
        let t = l.trim();
        t.starts_with('[') && t.ends_with(']') && !t.contains('=')
    });

    if has_toml_sections {
        return scan_toml_sections(&lines);
    }

    // YAML: top-level keys (no leading whitespace)
    let keys: Vec<&str> = lines
        .iter()
        .filter(|l| YAML_TOP_KEY.is_match(l))
        .take(40)
        .copied()
        .collect();
    format!(
        "[YAML keys ({} top-level)]\n{}",
        keys.len(),
        keys.join("\n")
    )
}

fn scan_toml_sections(lines: &[&str]) -> String {
    let mut sections: Vec<(String, usize)> = Vec::new();
    let mut current_section = String::from("[top-level]");
    let mut current_keys = 0usize;
    let total_lines = lines.len();

    for line in lines {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') && !t.contains('=') {
            if current_keys > 0 || current_section != "[top-level]" {
                sections.push((current_section.clone(), current_keys));
            }
            current_section = t.to_string();
            current_keys = 0;
        } else if YAML_TOP_KEY.is_match(t) {
            current_keys += 1;
        }
    }
    if current_keys > 0 || current_section != "[top-level]" {
        sections.push((current_section, current_keys));
    }

    let mut result = format!(
        "[TOML: {} sections, {} lines]\n",
        sections.len(),
        total_lines
    );
    for (section, keys) in &sections {
        result.push_str(&format!("  {} ({} keys)\n", section, keys));
    }
    result.trim_end().to_string()
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

/// Mask values in .env file content, preserving key names.
pub fn mask_env_values(content: &str) -> String {
    let mut result = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq_pos) = trimmed.find('=') {
            let key = &trimmed[..eq_pos];
            result.push(format!("{key}=***"));
        } else {
            result.push(trimmed.to_string());
        }
    }
    let total = content.lines().count();
    let keys = result.len();
    format!(
        "[.env: {} keys, values masked]\n{}",
        keys,
        result.join("\n")
    )
}

/// Detect the data sub-type from file extension and filename for dispatch.
pub fn data_subtype(path: &Path) -> DataSubtype {
    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

    // .env, .env.local, .env.production, etc
    if filename == ".env" || filename.starts_with(".env.") || filename.ends_with(".env") {
        return DataSubtype::Env;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "json" | "jsonc" | "json5" => DataSubtype::Json,
        "yaml" | "yml" | "toml" => DataSubtype::Yaml,
        "md" | "markdown" => DataSubtype::Markdown,
        "env" => DataSubtype::Env,
        _ => DataSubtype::Other,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DataSubtype {
    Json,
    Yaml,
    Markdown,
    Env,
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
    fn test_scan_toml_sections() {
        let toml = "[package]\nname = \"rtk\"\nversion = \"1.0\"\n\n[dependencies]\nclap = \"4\"\nanyhow = \"1\"\nregex = \"1\"\n\n[dev-dependencies]\ntempfile = \"3\"\n";
        let result = scan_yaml_keys(toml);
        assert!(result.contains("TOML:"), "should detect TOML: {result}");
        assert!(result.contains("[package]"), "got: {result}");
        assert!(result.contains("[dependencies]"), "got: {result}");
        assert!(
            result.contains("3 keys"),
            "deps should have 3 keys: {result}"
        );
    }

    #[test]
    fn test_mask_env_values() {
        let env = "# Database config\nDB_HOST=localhost\nDB_PASSWORD=super_secret_123\nAPI_KEY=sk-1234567890abcdef\n\nDEBUG=true\n";
        let result = mask_env_values(env);
        assert!(result.contains("4 keys"), "got: {result}");
        assert!(result.contains("DB_HOST=***"), "got: {result}");
        assert!(result.contains("API_KEY=***"), "got: {result}");
        assert!(
            !result.contains("super_secret"),
            "must not leak values: {result}"
        );
        assert!(
            !result.contains("sk-1234"),
            "must not leak values: {result}"
        );
    }

    #[test]
    fn test_data_subtype_env_detection() {
        assert_eq!(data_subtype(Path::new(".env")), DataSubtype::Env);
        assert_eq!(data_subtype(Path::new(".env.local")), DataSubtype::Env);
        assert_eq!(data_subtype(Path::new(".env.production")), DataSubtype::Env);
        assert_eq!(data_subtype(Path::new("app.env")), DataSubtype::Env);
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

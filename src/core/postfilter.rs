//! Post-filter pipeline: CDC reorder + terse fallback + budget tracking.
//!
//! Two entry points:
//! - `postprocess_filtered`: called after a specific filter or TOML filter ran
//! - `postprocess_fallback`: called for unhandled commands (no filter match)

use super::budget;
use super::cdc;
use super::chunk_cache;
use super::config::PostFilterConfig;
use super::dedup;
use super::terse;

/// Apply CDC reorder + budget tracking to already-filtered output.
pub fn postprocess_filtered(filtered: &str, cmd_key: &str, config: &PostFilterConfig) -> String {
    let mut output = filtered.to_string();

    if config.cdc_enabled && filtered.len() >= cdc::CDC_MIN_BYTES {
        output = apply_cdc(&output, cmd_key);
    }

    track_budget(&output);

    if let Some(warning) = budget::warning_suffix() {
        output.push('\n');
        output.push_str(&warning);
    }

    output
}

/// Apply dedup + terse scoring + CDC reorder + budget tracking to unfiltered fallback output.
pub fn postprocess_fallback(raw: &str, cmd_key: &str, config: &PostFilterConfig) -> Option<String> {
    let deduped = if config.dedup_enabled {
        dedup::compress(raw)
    } else {
        None
    };
    let input_for_terse = deduped.as_deref().unwrap_or(raw);

    let terse_result = if config.terse_enabled {
        terse::compress(input_for_terse, config.terse_threshold)
    } else {
        None
    };

    let compressed = match (&terse_result, &deduped) {
        (Some(r), _) => r.output.clone(),
        (None, Some(d)) => d.clone(),
        (None, None) => return apply_budget_only(raw),
    };

    let mut output = compressed;

    if config.cdc_enabled && output.len() >= cdc::CDC_MIN_BYTES {
        output = apply_cdc(&output, cmd_key);
    }

    track_budget(&output);

    if let Some(warning) = budget::warning_suffix() {
        output.push('\n');
        output.push_str(&warning);
    }

    Some(output)
}

fn apply_cdc(text: &str, cmd_key: &str) -> String {
    let new_chunks = cdc::chunk(text);
    let old_hashes = chunk_cache::load(cmd_key);

    let result = if !old_hashes.is_empty() {
        cdc::stable_reorder(text, &new_chunks, &old_hashes)
    } else {
        text.to_string()
    };

    // Store new hashes for next invocation
    let new_hashes = cdc::hashes(&new_chunks);
    if let Err(e) = chunk_cache::store(cmd_key, &new_hashes) {
        eprintln!("[rtk: chunk cache write failed: {}]", e);
    }

    result
}

fn track_budget(output: &str) {
    let tokens = budget::estimate_tokens(output);
    budget::BudgetTracker::global().record_output(tokens);
}

fn apply_budget_only(text: &str) -> Option<String> {
    track_budget(text);
    budget::warning_suffix().map(|warning| format!("{}\n{}", text, warning))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> PostFilterConfig {
        PostFilterConfig::default()
    }

    #[test]
    fn test_postprocess_filtered_passthrough_small() {
        let config = PostFilterConfig {
            cdc_enabled: false,
            ..default_config()
        };
        let result = postprocess_filtered("small output", "test", &config);
        assert_eq!(result, "small output");
    }

    #[test]
    fn test_postprocess_fallback_below_threshold() {
        let config = default_config();
        let result = postprocess_fallback("short", "test", &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_postprocess_fallback_compresses_large() {
        let mut lines = Vec::new();
        // Pad with low-value content to exceed 8000 chars
        for i in 0..300 {
            lines.push(format!("---------- separator line {} ----------", i));
            lines.push(format!(
                "  [========>     ] {}% downloading package...",
                i % 100
            ));
        }
        lines.push("error: build failed due to compilation errors".to_string());
        lines.push("error: aborting due to 3 previous errors".to_string());
        let large = lines.join("\n");
        assert!(large.len() > 8000, "fixture must exceed MIN_CHARS");

        let config = default_config();
        let result = postprocess_fallback(&large, "test_cmd", &config);
        assert!(result.is_some());

        let output = result.unwrap();
        assert!(output.contains("error:"));
        assert!(output.len() < large.len());
    }
}

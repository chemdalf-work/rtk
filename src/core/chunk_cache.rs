//! SQLite-backed chunk fingerprint cache with TTL.
//!
//! Stores CDC chunk hashes per (command_key, cwd) so subsequent invocations
//! can reorder unchanged chunks to the front for prompt-cache alignment.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::path::PathBuf;

use super::constants::RTK_DATA_DIR;

const CACHE_DB: &str = "chunk_cache.db";
const TTL_SECONDS: u64 = 300; // 5 minutes — matches Claude's prompt cache TTL

fn cache_db_path() -> Result<PathBuf> {
    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(RTK_DATA_DIR);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("Failed to create data dir: {}", data_dir.display()))?;
    Ok(data_dir.join(CACHE_DB))
}

fn open_db() -> Result<Connection> {
    let path = cache_db_path()?;
    let conn = Connection::open(&path)
        .with_context(|| format!("Failed to open chunk cache: {}", path.display()))?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chunk_hashes (
            cmd_key TEXT NOT NULL,
            hash TEXT NOT NULL,
            stored_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            PRIMARY KEY (cmd_key, hash)
        );
        CREATE INDEX IF NOT EXISTS idx_chunk_cmd ON chunk_hashes(cmd_key);
        CREATE INDEX IF NOT EXISTS idx_chunk_time ON chunk_hashes(stored_at);",
    )
    .context("Failed to initialize chunk cache schema")?;

    Ok(conn)
}

/// Build a cache key from the command and working directory.
pub fn cache_key(command: &str) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{}|{}", cwd, command)
}

/// Store chunk hashes for a command, replacing any existing entry.
pub fn store(cmd_key: &str, hashes: &HashSet<String>) -> Result<()> {
    let conn = open_db()?;

    conn.execute(
        "DELETE FROM chunk_hashes WHERE cmd_key = ?1",
        params![cmd_key],
    )
    .context("Failed to clear old hashes")?;

    let mut stmt = conn
        .prepare("INSERT INTO chunk_hashes (cmd_key, hash) VALUES (?1, ?2)")
        .context("Failed to prepare insert")?;

    for hash in hashes {
        stmt.execute(params![cmd_key, hash])?;
    }

    // Cleanup expired entries
    conn.execute(
        "DELETE FROM chunk_hashes WHERE stored_at < strftime('%s', 'now') - ?1",
        params![TTL_SECONDS],
    )
    .context("Failed to cleanup expired entries")?;

    Ok(())
}

/// Load chunk hashes for a command (returns empty set if expired or missing).
pub fn load(cmd_key: &str) -> HashSet<String> {
    let conn = match open_db() {
        Ok(c) => c,
        Err(_) => return HashSet::new(),
    };

    let mut stmt = match conn.prepare(
        "SELECT hash FROM chunk_hashes WHERE cmd_key = ?1 AND stored_at >= strftime('%s', 'now') - ?2",
    ) {
        Ok(s) => s,
        Err(_) => return HashSet::new(),
    };

    let rows = match stmt.query_map(params![cmd_key, TTL_SECONDS], |row| row.get::<_, String>(0)) {
        Ok(r) => r,
        Err(_) => return HashSet::new(),
    };

    rows.filter_map(|r| r.ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn test_key() -> String {
        format!("test_cmd_{}", std::process::id())
    }

    #[test]
    fn test_store_and_load() {
        let key = test_key();
        let mut hashes = HashSet::new();
        hashes.insert("abc123".to_string());
        hashes.insert("def456".to_string());

        store(&key, &hashes).expect("store should succeed");
        let loaded = load(&key);

        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains("abc123"));
        assert!(loaded.contains("def456"));

        // Cleanup
        let conn = open_db().unwrap();
        conn.execute("DELETE FROM chunk_hashes WHERE cmd_key = ?1", params![key])
            .unwrap();
    }

    #[test]
    fn test_load_missing_returns_empty() {
        let loaded = load("nonexistent_command_key_xyz");
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_store_replaces_old_hashes() {
        let key = test_key();

        let mut h1 = HashSet::new();
        h1.insert("old_hash".to_string());
        store(&key, &h1).expect("first store");

        let mut h2 = HashSet::new();
        h2.insert("new_hash".to_string());
        store(&key, &h2).expect("second store");

        let loaded = load(&key);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains("new_hash"));
        assert!(!loaded.contains("old_hash"));

        // Cleanup
        let conn = open_db().unwrap();
        conn.execute("DELETE FROM chunk_hashes WHERE cmd_key = ?1", params![key])
            .unwrap();
    }

    #[test]
    fn test_cache_key_includes_cwd() {
        let key = cache_key("git status");
        let cwd = env::current_dir().unwrap().to_string_lossy().to_string();
        assert!(key.contains(&cwd));
        assert!(key.contains("git status"));
    }
}

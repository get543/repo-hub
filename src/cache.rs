//! Persistent cache of discovered repo paths so startup is instant.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRepo {
    pub path: String,
    pub drive: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheFile {
    /// drive root paths, in the same order as `drive` indices above
    pub drives: Vec<String>,
    pub repos: Vec<CachedRepo>,
}

fn cache_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("repo-hub").join("cache.json"))
}

impl CacheFile {
    pub fn load() -> Option<Self> {
        let p = cache_path()?;
        let data = std::fs::read(&p).ok()?;
        serde_json::from_slice(&data).ok()
    }

    pub fn save(&self) {
        if let Some(p) = cache_path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_string_pretty(self) {
                // write-then-rename keeps the cache safe from partial writes
                let tmp = p.with_extension("json.tmp");
                if std::fs::write(&tmp, json).is_ok() {
                    let _ = std::fs::rename(&tmp, &p);
                }
            }
        }
    }

    pub fn clear() {
        if let Some(p) = cache_path() {
            let _ = std::fs::remove_file(p);
        }
    }
}

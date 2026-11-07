//! The help layer: the docs folder, searchable inside the app.
//!
//! It reads the Markdown that ships with the repository, so help cannot drift from the product.
//! No network, no separate content store.

use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Doc {
    pub title: String,
    pub path: String,
    pub body: String,
}

fn docs_dir() -> PathBuf {
    // The app is usually run from the repository during development and from an installed folder
    // otherwise; try both, then the vault's own docs folder.
    let mut candidates = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("docs"));
        candidates.push(cwd.join("..").join("docs"));
    }
    candidates.push(crate::settings::home().join("agentos-studio").join("docs"));
    candidates
        .into_iter()
        .find(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("docs"))
}

pub fn all() -> Result<Vec<Doc>> {
    let dir = docs_dir();
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out);
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&p) else {
            continue;
        };
        let title = body
            .lines()
            .find(|l| l.starts_with("# "))
            .map(|l| l.trim_start_matches("# ").trim().to_string())
            .unwrap_or_else(|| {
                p.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push(Doc {
            title,
            path: name,
            body: body.chars().take(40_000).collect(),
        });
    }
    out.sort_by_key(|d| d.title.to_lowercase());
    Ok(out)
}

/// Filter by title or body, case insensitive.
pub fn search(docs: &[Doc], query: &str) -> Vec<Doc> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return docs.to_vec();
    }
    docs.iter()
        .filter(|d| d.title.to_lowercase().contains(&q) || d.body.to_lowercase().contains(&q))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(title: &str, body: &str) -> Doc {
        Doc {
            title: title.into(),
            path: "x.md".into(),
            body: body.into(),
        }
    }

    #[test]
    fn search_matches_title_and_body() {
        let docs = vec![
            doc("Updater", "signing keys"),
            doc("Missions", "review gate"),
        ];
        assert_eq!(search(&docs, "updat").len(), 1);
        assert_eq!(search(&docs, "gate").len(), 1);
        assert_eq!(search(&docs, "").len(), 2);
        assert_eq!(search(&docs, "nothing").len(), 0);
    }
}

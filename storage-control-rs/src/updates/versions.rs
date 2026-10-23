//! Version discovery: current installed version, GitHub releases, and local tags.
//!
//! Ported from `storage-control/app/updates/versions.py`. The same caching
//! (5 minutes), same release repositories, same migration-step extraction
//! keywords. The `stalwart` GitHub release fetch works; local tags are empty
//! because Stalwart is not installed from source.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use semver::Version;

use crate::config::Config;
use crate::exec;

const CACHE_TTL: Duration = Duration::from_secs(300);

/// Keywords that indicate a changelog line is a migration step (operator must
/// be aware of it before upgrading).
const MIGRATION_KEYWORDS: &[&str] = &[
    "migration", "migrate", "database", "db ", "occ ", "upgrade",
    "backup", "breaking", "important", "env", "config",
    "maintenance", "index", "rebuild", "npm", "pnpm", "php",
];

/// Normalised version string with the leading `v` stripped.
fn normalise(v: &str) -> String {
    v.trim_start_matches('v').to_string()
}

/// Compare two version strings by their leading numeric components.
/// `v3.3.3+1` and `v3.3.3-rc1` both compare equal to `v3.3.3`.
fn version_key(v: &str) -> Vec<u64> {
    let v = normalise(v);
    let mut nums = Vec::new();
    for part in v.split('.') {
        let num: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(n) = num.parse::<u64>() {
            nums.push(n);
        } else {
            break;
        }
    }
    nums
}

fn is_newer(latest: &str, current: &str) -> bool {
    let l = version_key(latest);
    let c = version_key(current);
    l > c
}

/// Cached result of a release check.
#[derive(Clone, Debug)]
struct CachedRelease {
    fetched_at: Instant,
    value: ReleaseInfo,
}

#[derive(Clone, Debug, Default)]
pub struct ReleaseInfo {
    pub current: String,
    pub latest: String,
    pub available: bool,
    pub error: Option<String>,
    pub body: String,
    pub migration_steps: Vec<String>,
    pub url: Option<String>,
}

/// Simple in-memory cache with TTL.
struct ReleaseCache {
    inner: Mutex<HashMap<(String, String), CachedRelease>>,
    client: Client,
}

impl ReleaseCache {
    fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent("storage-control")
                .build()
                .expect("reqwest client"),
        }
    }

    fn get(&self, target: &str, current: &str) -> Option<ReleaseInfo> {
        let mut guard = self.inner.lock().unwrap();
        let now = Instant::now();
        if let Some(cached) = guard.get(&(target.to_string(), current.to_string())) {
            if now.duration_since(cached.fetched_at) < CACHE_TTL {
                return Some(cached.value.clone());
            }
        }
        None
    }

    fn insert(&self, target: &str, current: &str, value: ReleaseInfo) {
        let mut guard = self.inner.lock().unwrap();
        guard.insert(
            (target.to_string(), current.to_string()),
            CachedRelease { fetched_at: Instant::now(), value },
        );
    }
}

lazy_static::lazy_static! {
    static ref RELEASE_CACHE: ReleaseCache = ReleaseCache::new();
    static ref RELEASE_REPOS: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        m.insert("immich", "immich-app/immich");
        m.insert("nextcloud", "nextcloud/server");
        m.insert("stalwart", "stalwartlabs/stalwart");
        m
    };
}

/// Fetch the latest release from GitHub and extract migration steps.
fn fetch_release(target: &str, current: &str) -> ReleaseInfo {
    if let Some(cached) = RELEASE_CACHE.get(target, current) {
        return cached;
    }

    let mut info = ReleaseInfo {
        current: current.to_string(),
        latest: current.to_string(),
        available: false,
        error: None,
        body: String::new(),
        migration_steps: Vec::new(),
        url: None,
    };

    let Some(repo) = RELEASE_REPOS.get(target) else {
        info.error = Some("unknown target".into());
        return info;
    };

    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let request = RELEASE_CACHE.client.get(&url)
        .header("Accept", "application/vnd.github+json");

    match request.send() {
        Ok(response) if response.status().is_success() => {
            match response.json::<serde_json::Value>() {
                Ok(data) => {
                    let latest = data.get("tag_name")
                        .and_then(|v| v.as_str())
                        .map(normalise)
                        .unwrap_or_else(|| current.to_string());
                    let body = data.get("body")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let html_url = data.get("html_url")
                        .and_then(|v| v.as_str())
                        .map(String::from);

                    info.latest = latest.clone();
                    info.body = body.clone();
                    info.url = html_url;
                    info.migration_steps = extract_migration_steps(&body);
                    info.available = !latest.is_empty() && latest != current && is_newer(&latest, current);
                }
                Err(e) => info.error = Some(format!("parse json: {e}")),
            }
        }
        Ok(response) => {
            info.error = Some(format!("HTTP {}", response.status()));
        }
        Err(e) => info.error = Some(e.to_string()),
    }

    RELEASE_CACHE.insert(target, current, info.clone());
    info
}

fn extract_migration_steps(body: &str) -> Vec<String> {
    let mut steps = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in body.lines() {
        let line = line.trim().trim_start_matches('#').trim_start_matches('-').trim_start_matches('*').trim_start_matches('•').trim();
        if line.is_empty() || line.len() < 8 {
            continue;
        }
        let lowered = line.to_lowercase();
        if MIGRATION_KEYWORDS.iter().any(|kw| lowered.contains(kw)) {
            if seen.insert(line.to_string()) {
                steps.push(line.to_string());
                if steps.len() >= 15 {
                    break;
                }
            }
        }
    }
    steps
}

/// Current installed version of Immich from the local git checkout.
pub fn immich_current(cfg: &Config) -> String {
    git_version(&cfg.immich_src)
}

/// Current installed version of Nextcloud from the local git checkout.
pub fn nextcloud_current(cfg: &Config) -> String {
    git_version(&cfg.nextcloud_root)
}

/// Current installed version of Stalwart from the binary.
pub fn stalwart_current(cfg: &Config) -> String {
    match exec::run_simple(&[&cfg.stalwart_bin.to_string_lossy(), "--version"]) {
        out if out.ok() => {
            let v = out.trimmed();
            if !v.starts_with('v') { format!("v{v}") } else { v }
        }
        _ => "unknown".into(),
    }
}

/// `git describe` logic: exact tag if HEAD is on one, else nearest tag
/// plus ahead count, else short commit hash.
pub fn git_version(path: &std::path::Path) -> String {
    let exact = exec::run(&["git", "describe", "--tags", "--exact-match"], Some(path), None, Duration::from_secs(30));
    if exact.ok() && !exact.stdout.trim().is_empty() {
        return exact.stdout.trim().to_string();
    }

    let nearest = exec::run(&["git", "describe", "--tags", "--abbrev=0"], Some(path), None, Duration::from_secs(30));
    if nearest.ok() && !nearest.stdout.trim().is_empty() {
        let tag = nearest.stdout.trim();
        let ahead = exec::run(&["git", "rev-list", "--count", &format!("{tag}..HEAD")], Some(path), None, Duration::from_secs(30));
        if ahead.ok() {
            let count = ahead.stdout.trim();
            if let Ok(n) = count.parse::<usize>() {
                if n > 0 {
                    return format!("{tag}+{n}");
                }
            }
        }
        return tag.to_string();
    }

    let hash = exec::run(&["git", "rev-parse", "--short", "HEAD"], Some(path), None, Duration::from_secs(30));
    if hash.ok() && !hash.stdout.trim().is_empty() {
        return hash.stdout.trim().to_string();
    }
    "unknown".into()
}

/// Available tags for Immich (local repo, sorted by creation date).
pub fn immich_tags(cfg: &Config, limit: usize) -> Vec<String> {
    git_tags(&cfg.immich_src, limit, |t| t.starts_with('v') && !t[1..].contains('-'))
}

/// Available tags for Nextcloud (local repo, only stable tags of same/newer major).
pub fn nextcloud_tags(cfg: &Config, limit: usize) -> Vec<String> {
    let current = nextcloud_current(cfg);
    let major = current.trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    git_tags(&cfg.nextcloud_root, limit, move |t| {
        let t = t.trim_start_matches('v');
        if let Ok(tag_major) = t.split('.').next().unwrap_or("0").parse::<u64>() {
            return tag_major >= major && t.chars().all(|c| c.is_ascii_digit() || c == '.');
        }
        false
    })
}

/// Available tags for Stalwart (empty — not a local git checkout).
pub fn stalwart_tags(_cfg: &Config, _limit: usize) -> Vec<String> {
    Vec::new()
}

/// Generic tag listing with a filter predicate.
fn git_tags(path: &std::path::Path, limit: usize, filter: impl Fn(&str) -> bool) -> Vec<String> {
    let out = exec::run(&["git", "tag", "--sort=-creatordate"], Some(path), None, Duration::from_secs(30));
    if !out.ok() {
        return Vec::new();
    }
    out.stdout.lines()
        .map(str::trim)
        .filter(|t| filter(t))
        .take(limit)
        .map(String::from)
        .collect()
}

/// `immich_release_info` with the current version resolved from the repo.
pub fn immich_release_info(cfg: &Config) -> ReleaseInfo {
    let current = immich_current(cfg);
    fetch_release("immich", &current)
}

/// `nextcloud_release_info` with the current version resolved from the repo.
pub fn nextcloud_release_info(cfg: &Config) -> ReleaseInfo {
    let current = nextcloud_current(cfg);
    fetch_release("nextcloud", &current)
}

/// `stalwart_release_info` with the current version resolved from the binary.
pub fn stalwart_release_info(cfg: &Config) -> ReleaseInfo {
    let current = stalwart_current(cfg);
    fetch_release("stalwart", &current)
}

/// Ensure the latest release is present in the tag list (for the UI dropdown).
pub fn add_candidate(tags: Vec<String>, latest: String) -> Vec<String> {
    let latest = normalise(&latest);
    if tags.iter().any(|t| normalise(t) == latest) {
        tags
    } else {
        let mut out = vec![latest];
        out.extend(tags);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_key_strips_suffixes() {
        assert_eq!(version_key("v3.3.3"), vec![3, 3, 3]);
        assert_eq!(version_key("v3.3.3+1"), vec![3, 3, 3]);
        assert_eq!(version_key("v3.3.3-rc1"), vec![3, 3, 3]);
        assert_eq!(version_key("v35.0.1"), vec![35, 0, 1]);
    }

    #[test]
    fn is_newer_works() {
        assert!(is_newer("v3.3.4", "v3.3.3"));
        assert!(is_newer("v3.4.0", "v3.3.9"));
        assert!(is_newer("v4.0.0", "v3.9.9"));
        assert!(!is_newer("v3.3.3", "v3.3.3"));
        assert!(!is_newer("v3.3.3", "v3.3.4"));
        assert!(!is_newer("v3.3.3+1", "v3.3.3")); // same numeric version
    }

    #[test]
    fn normalise_strips_v() {
        assert_eq!(normalise("v3.3.3"), "3.3.3");
        assert_eq!(normalise("3.3.3"), "3.3.3");
    }

    #[test]
    fn migration_steps_extracted() {
        let body = r#"
## What's Changed
- Migration: database schema updated
- Breaking change: config format changed
- Updated dependencies
- Migration: run `occ db:add-missing-indices`
- Feature: new dashboard
"#;
        let steps = extract_migration_steps(body);
        assert!(steps.iter().any(|s| s.contains("Migration")));
        assert!(steps.iter().any(|s| s.contains("Breaking change")));
        assert!(steps.iter().any(|s| s.contains("occ db:add-missing-indices")));
        assert!(!steps.iter().any(|s| s.contains("Updated dependencies")));
    }
}
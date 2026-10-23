//! Configuration, loaded from the environment plus an optional env file.
//!
//! Behavioural contract with the old Flask app (see doc/rust-port.md §2.1):
//! the same variable names, the same filesystem layout, the same defaults.

use std::path::{Path, PathBuf};

/// Trees under the backup root that are machine-managed and must never be
/// walked file-by-file. `home-storage` is the rsync mirror of $STORAGE_HOME
/// (hundreds of thousands of files), `preserves` has its own UI.
pub const BACKUP_SKIP_DIRS: [&str; 2] = ["home-storage", "preserves"];

#[derive(Debug, Clone)]
pub struct Config {
    pub secret_key: String,
    pub admin_username: String,
    pub admin_password: Option<String>,
    pub enable_scheduler: bool,

    pub storage_home: PathBuf,
    pub data_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub db_path: PathBuf,
    pub backup_root: PathBuf,
    pub repo_root: PathBuf,

    pub immich_src: PathBuf,
    pub immich_env: PathBuf,
    pub immich_patches_dir: PathBuf,
    pub nextcloud_root: PathBuf,
    pub stalwart_home: PathBuf,
    pub stalwart_bin: PathBuf,
    pub stalwart_data: PathBuf,
}

impl Config {
    /// Load configuration. If `env_file` is given it is parsed first and the
    /// values it defines are used only for variables that are not already set
    /// in the real environment (same precedence as `load_dotenv(override=False)`).
    pub fn load(env_file: Option<&Path>) -> Result<Self, String> {
        let from_flag = env_file.map(Path::to_path_buf);
        let from_env = std::env::var_os("STORAGE_CONTROL_ENV_FILE").map(PathBuf::from);
        if let Some(path) = from_flag.or(from_env) {
            load_env_file(&path)?;
        }
        Self::from_env()
    }

    pub fn from_env() -> Result<Self, String> {
        let storage_home = path_var("STORAGE_HOME", "/home/storage");
        let data_dir = std::env::var_os("STORAGE_CONTROL_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| storage_home.join(".local/share/storage-control"));

        // The repo root is only used to find tools/ and README.md; it can be
        // overridden for a relocated deployment.
        let repo_root = std::env::var_os("STORAGE_CONTROL_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .expect("crate lives in the repo root")
                    .to_path_buf()
            });

        let enable_scheduler = match std::env::var("ENABLE_SCHEDULER") {
            Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no" | ""),
            Err(_) => true,
        };

        Ok(Self {
            secret_key: std::env::var("SECRET_KEY").unwrap_or_else(|_| "dev-insecure-change-me".into()),
            admin_username: std::env::var("ADMIN_USERNAME").unwrap_or_else(|_| "admin".into()),
            admin_password: std::env::var("ADMIN_PASSWORD").ok().filter(|v| !v.is_empty()),
            enable_scheduler,

            runs_dir: data_dir.join("runs"),
            db_path: data_dir.join("storage-control.db"),
            backup_root: PathBuf::from("/mnt/backup/backups"),
            repo_root: repo_root.clone(),

            immich_src: storage_home.join("immich/src"),
            immich_env: storage_home.join("immich/.env"),
            immich_patches_dir: repo_root.join("tools/immich/patches"),
            nextcloud_root: storage_home.join("nextcloud"),
            stalwart_home: storage_home.join("stalwart"),
            stalwart_bin: storage_home.join("stalwart/bin/stalwart"),
            stalwart_data: PathBuf::from("/mnt/data/stalwart"),

            storage_home,
            data_dir,
        })
    }
}

fn path_var(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

/// Minimal `.env` parser: `KEY=VALUE`, `#` comments, optional surrounding
/// quotes, values never override the real environment.
fn load_env_file(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read env file {}: {e}", path.display()))?;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("{}:{}: expected KEY=VALUE", path.display(), lineno + 1));
        };
        let key = key.trim();
        if key.is_empty() || std::env::var_os(key).is_some() {
            continue;
        }
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        std::env::set_var(key, value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_file_does_not_override_existing_vars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.env");
        std::fs::write(&path, "SC_TEST_KEEP=from-file\nSC_TEST_NEW=hello\n# comment\n").unwrap();
        std::env::set_var("SC_TEST_KEEP", "from-process");
        load_env_file(&path).unwrap();
        assert_eq!(std::env::var("SC_TEST_KEEP").unwrap(), "from-process");
        assert_eq!(std::env::var("SC_TEST_NEW").unwrap(), "hello");
    }
}

//! Immich update runner — ported from `runners/update_immich.py`.
//!
//! Operational invariants (must not change):
//!   * pre-update DB dump (`pg_dump -Fc`) and config tar to `/mnt/backup/backups/immich/`
//!   * `git fetch origin --tags` (no `--no-recurse-submodules` — immich has no submodules)
//!   * tracked files that were edited locally are stashed by prepare; runner resets
//!     patched files before checkout and re-applies local patches from this repo
//!   * pnpm/node live only in `~/immich/node/bin` (corepack shims), NOT on PATH
//!   * `COREPACK_ENABLE_DOWNLOAD_PROMPT=0` and `NPM_CONFIG_UPDATE_NOTIFIER=false`
//!   * pnpm version pinned by upstream `packageManager` — never "upgrade" it
//!   * build: `pnpm install --frozen-lockfile`, `pnpm run build` in server/ and web/
//!   * core plugin: `pnpm --filter @immich/sdk --filter @immich/plugin-sdk --filter @immich/plugin-core build`
//!     then copy to `~/immich/build/plugins/immich-plugin-core/{manifest.json,dist}`
//!   * migrations: `pnpm run migrations:run` in server/
//!   * restart: `systemctl --user restart immich-ml.service immich-server.service`
//!   * smoke test: poll `/api/server/ping` (and fallback `/api/health`) until it answers
//!   * transient work in `/tmp`, never on data/backup disks
//!   * on failure: turn maintenance off where possible (immich has no maintenance mode)

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::Config;
use crate::db::repo::audit;
use crate::db::repo::update as repo;
use crate::db::Db;
use crate::error::AppError;
use crate::exec;
use crate::updates::steps::{Step, StepTracker};

const PATH_SEP: &str = if cfg!(windows) { ";" } else { ":" };

const SMOKE_URLS: &[&str] = &[
    "http://127.0.0.1:2283/api/server/ping",
    "http://127.0.0.1:2283/api/health",
];
const SMOKE_DEADLINE: Duration = Duration::from_secs(180);
const SMOKE_INTERVAL: Duration = Duration::from_secs(5);

/// Environment for any step that needs node/pnpm.
/// `pnpm` exists only in immich's private node install (`~/immich/node/bin`),
/// which is not on the storage-control service PATH.
fn tool_env(cfg: &Config) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    let node_bin = cfg.immich_src.parent().unwrap().join("node/bin");
    if node_bin.is_dir() {
        let new_path = format!(
            "{}{}{}",
            node_bin.display(),
            PATH_SEP,
            env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str()).unwrap_or("")
        );
        env.retain(|(k, _)| k != "PATH");
        env.push(("PATH".into(), new_path));
    }
    env.push(("COREPACK_ENABLE_DOWNLOAD_PROMPT".into(), "0".into()));
    env.push(("NPM_CONFIG_UPDATE_NOTIFIER".into(), "false".into()));
    env
}

/// `packageManager` value from `package.json` (e.g. `pnpm@11.27.1`).
fn pnpm_pin(cfg: &Config) -> String {
    let pkg_path = cfg.immich_src.join("package.json");
    let content = match std::fs::read_to_string(&pkg_path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let value: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let pin = value.get("packageManager").and_then(|v| v.as_str());
    pin.map(String::from).unwrap_or_default()
}

/// Parse `pnpm@11.27.1` / `pnpm@11.27.1+sha512.<hash>` → `11.27.1`.
fn pnpm_pin_version(pin: &str) -> String {
    pin.split('@').nth(1).unwrap_or("").split('+').next().unwrap_or("").to_string()
}

/// Log the pnpm version in use and compare to the pinned version.
fn check_pnpm_version(cfg: &Config, log_path: &Path, node_env: &[(String, String)]) -> exec::CmdOutput {
    let out = exec::run(&["pnpm", "--version"], Some(&cfg.immich_src), Some(node_env), Duration::from_secs(120));
    let effective = out.trimmed();
    let pin = pnpm_pin(cfg);
    exec::log_write(log_path, &format!("pnpm in use: {}\n", if effective.is_empty() { "?" } else { &effective }));
    exec::log_write(log_path, &format!("packageManager pin: {}\n", if pin.is_empty() { "(none)" } else { &pin }));
    if !pin.is_empty() && !effective.is_empty() && effective != pnpm_pin_version(&pin) {
        exec::log_write(log_path,
            &format!("WARN: pnpm {} does not match the pinned {}; the pin comes from upstream and must not be changed locally\n",
                effective, pin));
    }
    out
}

/// Environment for the core plugin build (extism-js, wasm-merge).
fn plugin_env(cfg: &Config) -> Vec<(String, String)> {
    let mut env = tool_env(cfg);
    let tools = cfg.storage_home.join("immich/tools");
    let mut extra = vec![tools.join("extism-js")];
    if let Ok(entries) = std::fs::read_dir(tools.join("binaryen")) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("binaryen-") {
                extra.push(entry.path().join("bin"));
            }
        }
    }
    let found: Vec<String> = extra.into_iter().filter(|p| p.is_dir()).map(|p| p.to_string_lossy().to_string()).collect();
    if !found.is_empty() {
        let new_path = format!("{}{}{}", found.join(PATH_SEP), PATH_SEP, env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str()).unwrap_or(""));
        env.retain(|(k, _)| k != "PATH");
        env.push(("PATH".into(), new_path));
    }
    env
}

/// Build the core workflow plugin and install it where immich loads it.
/// immich reads it from `<build>/plugins/immich-plugin-core/{manifest.json,dist}`.
fn build_core_plugin(cfg: &Config, log_path: &Path, plugin_env: &[(String, String)]) -> exec::CmdOutput {
    let out = exec::run(
        &[
            "pnpm", "--filter", "@immich/sdk", "--filter", "@immich/plugin-sdk",
            "--filter", "@immich/plugin-core", "build"
        ],
        Some(&cfg.immich_src), Some(plugin_env), Duration::from_secs(1800),
    );
    if !out.ok() {
        return out;
    }

    let src = cfg.immich_src.join("packages/plugin-core");
    let dst = cfg.immich_src.parent().unwrap().join("build/plugins/immich-plugin-core");
    let _ = std::fs::create_dir_all(&dst.join("dist"));
    let _ = std::fs::copy(src.join("manifest.json"), dst.join("manifest.json"));
    let _ = std::fs::remove_dir_all(&dst.join("dist"));
    let _ = copy_dir_all(&src.join("dist"), &dst.join("dist"));
    exec::log_write(log_path, &format!("installed core plugin to {}\n", dst.display()));
    exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// Re-apply local patches kept in this repo's `tools/immich/patches/`.
/// Non-fatal: a release may have fixed what the patch works around.
fn apply_local_patches(cfg: &Config, log_path: &Path) -> exec::CmdOutput {
    let patches_dir = &cfg.immich_patches_dir;
    let patches: Vec<PathBuf> = std::fs::read_dir(patches_dir)
        .map(|entries| entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map(|e| e == "patch").unwrap_or(false)).collect())
        .unwrap_or_default();
    if patches.is_empty() {
        exec::log_write(log_path, &format!("no local patches in {}\n", patches_dir.display()));
        return exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() };
    }

    // Check for tracked modifications that would block checkout
    let status = exec::run(&["git", "status", "--porcelain"], Some(&cfg.immich_src), None, Duration::from_secs(30));
    let dirty: Vec<&str> = status.stdout.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with("??"))
        .collect();
    if !status.ok() || !dirty.is_empty() {
        exec::log_write(log_path, "skipping local patches: tracked files are modified\n");
        return exec::CmdOutput { rc: 1, stdout: String::new(), stderr: "tracked files modified".into() };
    }

    let mut failed = Vec::new();
    let patch_count = patches.len();
    for patch in patches {
        let name = patch.file_name().unwrap().to_string_lossy();
        // Already applied?
        let reverse_check = exec::run(&["git", "apply", "--reverse", "--check", &patch.to_string_lossy()], Some(&cfg.immich_src), None, Duration::from_secs(30));
        if reverse_check.ok() {
            exec::log_write(log_path, &format!("{} already applied\n", name));
            continue;
        }
        let check = exec::run(&["git", "apply", "--check", &patch.to_string_lossy()], Some(&cfg.immich_src), None, Duration::from_secs(30));
        if check.ok() {
            let apply = exec::run(&["git", "apply", &patch.to_string_lossy()], Some(&cfg.immich_src), None, Duration::from_secs(30));
            if !apply.ok() {
                failed.push(name.to_string());
            }
            continue;
        }
        // Try 3-way merge
        let merge = exec::run(&["git", "apply", "--3way", &patch.to_string_lossy()], Some(&cfg.immich_src), None, Duration::from_secs(30));
        if merge.ok() {
            exec::log_write(log_path, &format!("{} applied with --3way\n", name));
            continue;
        }
        // Conflicted — restore and report
        let _ = exec::run(&["git", "reset", "--hard", "HEAD"], Some(&cfg.immich_src), None, Duration::from_secs(30));
        exec::log_write(log_path, &format!("WARN: {} no longer applies; the tree was restored\n", name));
        failed.push(name.to_string());
    }

    if failed.is_empty() {
        exec::log_write(log_path, &format!("local patches: {} applied\n", patch_count));
        exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() }
    } else {
        exec::CmdOutput { rc: 1, stdout: String::new(), stderr: format!("patches failed: {}", failed.join(", ")) }
    }
}

/// Files touched by local patches (repo-relative) — used to reset them before checkout.
fn patched_files(cfg: &Config) -> Vec<String> {
    let patches_dir = &cfg.immich_patches_dir;
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(patches_dir) {
        for entry in entries.flatten() {
            let patch = entry.path();
            if patch.extension().map(|e| e == "patch").unwrap_or(false) {
                let out = exec::run(&["git", "apply", "--numstat", &patch.to_string_lossy()], Some(&cfg.immich_src), None, Duration::from_secs(30));
                if !out.ok() { continue; }
                for line in out.stdout.lines() {
                    let parts: Vec<&str> = line.split('\t').collect();
                    if parts.len() < 3 { continue; }
                    let mut path = parts[2].trim().to_string();
                    if path.contains("=>") {
                        // rename: keep destination
                        path = path.split("=>").last().unwrap().trim().trim_matches(|c| c == '{' || c == '}').to_string();
                    }
                    if !path.is_empty() && !files.contains(&path) {
                        files.push(path);
                    }
                }
            }
        }
    }
    files
}

/// Reset patched files to HEAD (so `git checkout <tag>` won't abort).
fn reset_patched_files(cfg: &Config, log_path: &Path) -> exec::CmdOutput {
    let files = patched_files(cfg);
    if files.is_empty() {
        return exec::CmdOutput { rc: 0, stdout: String::new(), stderr: String::new() };
    }
    let mut argv: Vec<String> = vec!["git".into(), "checkout".into(), "--".into()];
    argv.extend(files.clone());
    let borrowed: Vec<&str> = argv.iter().map(String::as_str).collect();
    let out = exec::run(&borrowed, Some(&cfg.immich_src), None, Duration::from_secs(30));
    if !out.ok() {
        exec::log_write(log_path, &format!("WARN: could not reset patched files: {}\n", out.trimmed()));
    } else {
        exec::log_write(log_path, &format!("reset patched files before checkout: {}\n", files.join(", ")));
    }
    out
}

/// Snapshot patched files to a temp dir (for potential restore on failed checkout).
fn snapshot_patched(cfg: &Config, snap_dir: &Path) -> Vec<(String, PathBuf)> {
    let files = patched_files(cfg);
    let mut saved = Vec::new();
    for rel in files {
        let src = cfg.immich_src.join(&rel);
        if !src.is_file() { continue; }
        let dst = snap_dir.join(rel.replace('/', "__"));
        if let Some(parent) = dst.parent() { let _ = std::fs::create_dir_all(parent); }
        let _ = std::fs::copy(&src, &dst);
        saved.push((rel, dst));
    }
    saved
}

/// Restore snapshotted files (checkout failed, tree intact).
fn restore_snapshot(saved: &[(String, PathBuf)], cfg: &Config) {
    for (rel, dst) in saved {
        let _ = std::fs::copy(dst, cfg.immich_src.join(rel));
    }
}

/// Poll the Immich API until it answers or the deadline passes.
/// `systemctl restart` returns before the API binds :2283.
fn wait_healthy(log_path: &Path) -> exec::CmdOutput {
    let deadline = std::time::Instant::now() + SMOKE_DEADLINE;
    let mut attempt = 0;
    loop {
        attempt += 1;
        for url in SMOKE_URLS {
            let out = exec::run(
                &["curl", "-fsS", url],
                None, None, Duration::from_secs(30),
            );
            if out.ok() {
                if attempt > 1 {
                    exec::log_write(log_path, &format!("healthy after {} attempts: {}\n", attempt, url));
                }
                return out;
            }
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(SMOKE_INTERVAL);
    }
    exec::log_write(log_path, &format!("immich API did not answer within {:?}\n", SMOKE_DEADLINE));
    exec::CmdOutput { rc: 1, stdout: String::new(), stderr: "smoke test timeout".into() }
}

fn local_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H%M").to_string()
}

pub fn run_immich_update(cfg: &Config, run_id: i64) -> Result<(), AppError> {
    let db = Db::open(&cfg.db_path)?;
    let log_path = cfg.runs_dir.join(format!("update_immich_{run_id}.log"));
    let _ = std::fs::create_dir_all(&log_path.parent().unwrap());

    let mut tracker = StepTracker::new(db.clone(), run_id);
    let node_env = tool_env(cfg);
    let mig_env = {
        let mut env = node_env.clone();
        // Source IMMICH_ENV for migrations
        if let Ok(text) = std::fs::read_to_string(&cfg.immich_env) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') || !line.contains('=') { continue; }
                let (k, v) = line.split_once('=').unwrap();
                env.push((k.trim().to_string(), v.trim().trim_matches('"').trim_matches('\'').to_string()));
            }
        }
        env
    };
    let plugin_env = plugin_env(cfg);

    // Pre-update backups
    let backup_root = &cfg.backup_root;
    let _ = std::fs::create_dir_all(&backup_root.join("immich"));

    tracker.step("backup_db", || {
        let out = backup_root.join("immich").join(format!("immich_db_preupdate_{}.dump", local_stamp()));
        Ok(exec::run(
            &["pg_dump", &format!("--dbname=postgresql://storage@localhost:5432/immich"), "-Fc", "-f", &out.to_string_lossy()],
            None, None, Duration::from_secs(3600),
        ))
    })?;

    tracker.step("backup_config", || {
        let out = backup_root.join("immich").join(format!("immich_config_preupdate_{}.tar.gz", local_stamp()));
        Ok(exec::run(
            &["tar", "czf", &out.to_string_lossy(), "-C", &cfg.storage_home.to_string_lossy(), "immich/.env"],
            None, None, Duration::from_secs(300),
        ))
    })?;

    tracker.step("git_fetch", || {
        Ok(exec::run(&["git", "fetch", "origin", "--tags"], Some(&cfg.immich_src), None, Duration::from_secs(300)))
    })?;

    // Snapshot patched files and reset them before checkout
    let snap_dir = std::env::temp_dir().join(format!("immich-update-{run_id}"));
    let _ = std::fs::create_dir_all(&snap_dir);
    let saved = snapshot_patched(cfg, &snap_dir);

    tracker.step("reset_patched_files", || {
        Ok(reset_patched_files(cfg, &log_path))
    })?;

    // We need the target tag - read it from the DB
    let tag = {
        let run = repo::get_run(&db, run_id)?.ok_or(AppError::NotFound)?;
        run.to_version.unwrap_or_default()
    };
    if tag.is_empty() {
        return Err(AppError::BadRequest("no target tag".into()));
    }

    let checkout_rc = tracker.step("git_checkout", || {
        Ok(exec::run(&["git", "checkout", &tag], Some(&cfg.immich_src), None, Duration::from_secs(300)))
    })?;

    if checkout_rc.rc != 0 {
        restore_snapshot(&saved, cfg);
        return Err(AppError::BadRequest("git checkout failed".into()));
    }

    tracker.step("apply_local_patches", || {
        Ok(apply_local_patches(cfg, &log_path))
    })?;

    tracker.step("pnpm_version", || {
        Ok(check_pnpm_version(cfg, &log_path, &node_env))
    })?;

    tracker.step("pnpm_install", || {
        Ok(exec::run(&["pnpm", "install", "--frozen-lockfile"], Some(&cfg.immich_src), Some(&node_env), Duration::from_secs(1800)))
    })?;

    tracker.step("build_server", || {
        Ok(exec::run(&["pnpm", "run", "build"], Some(&cfg.immich_src.join("server")), Some(&node_env), Duration::from_secs(1800)))
    })?;

    tracker.step("build_web", || {
        Ok(exec::run(&["pnpm", "run", "build"], Some(&cfg.immich_src.join("web")), Some(&node_env), Duration::from_secs(1800)))
    })?;

    let plugin_rc = tracker.step("build_plugin_core", || {
        Ok(build_core_plugin(cfg, &log_path, &plugin_env))
    })?;
    if plugin_rc.rc != 0 {
        exec::log_write(&log_path, "WARN: core plugin build failed; continuing\n");
    }

    tracker.step("migrations", || {
        Ok(exec::run(&["pnpm", "run", "migrations:run"], Some(&cfg.immich_src.join("server")), Some(&mig_env), Duration::from_secs(600)))
    })?;

    tracker.step("restart_services", || {
        Ok(exec::run(&["systemctl", "--user", "restart", "immich-ml.service", "immich-server.service"], None, None, Duration::from_secs(120)))
    })?;

    let smoke_rc = tracker.step("smoke_test", || {
        Ok(wait_healthy(&log_path))
    })?;

    // Clean up snapshot dir
    let _ = std::fs::remove_dir_all(&snap_dir);

    let steps = tracker.into_steps();
    let status = if smoke_rc.rc == 0 { "success" } else { "failed" };
    repo::finish_run(&db, run_id, status, Some(&tag), &steps)?;
    audit::record(&db, "admin", "update.immich", Some(&tag), Some(status));
    Ok(())
}
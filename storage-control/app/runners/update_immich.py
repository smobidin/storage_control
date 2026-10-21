from pathlib import Path
from datetime import datetime
import os, json, time, shutil, tempfile

from ..extensions import db
from ..models import UpdateRun, AuditLog
from ..config import (IMMICH_SRC, IMMICH_ENV, IMMICH_PATCHES_DIR, STORAGE_HOME,
                      BACKUP_ROOT, RUNS_DIR)
from .common import run_cmd, log_write


def _env_file(path):
    out = {}
    try:
        for line in Path(path).read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip().strip('"').strip("'")
    except Exception:
        pass
    return out


def _tool_env():
    """Environment for steps that need node/pnpm.

    `pnpm` exists only in immich's private node install
    (`~/immich/node/bin`, a corepack shim), which is not on the
    storage-control service PATH, so it must be prepended explicitly.
    """
    env = dict(os.environ)
    node_bin = IMMICH_SRC.parent / "node" / "bin"
    if node_bin.is_dir():
        env["PATH"] = f"{node_bin}{os.pathsep}{env.get('PATH', '')}"
    # never let a corepack "continue? [Y/n]" prompt hang a background job
    env.setdefault("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0")
    # pnpm's update notifier prints an "Update available! x -> y" banner on
    # every install. It is advisory only, but it has repeatedly suggested
    # `corepack use pnpm@<newer>` -- a command that rewrites the upstream
    # `packageManager` pin in package.json and breaks the frozen lockfile.
    env.setdefault("NPM_CONFIG_UPDATE_NOTIFIER", "false")
    return env


def _pnpm_pin():
    """The pnpm version upstream pins in package.json (`packageManager`)."""
    try:
        pkg = json.loads((IMMICH_SRC / "package.json").read_text())
    except Exception:
        return ""
    return str(pkg.get("packageManager") or "").strip()


def _pnpm_pin_version(pin):
    """`pnpm@11.27.1` / `pnpm@11.27.1+sha512.<hash>` -> `11.27.1`."""
    _, _, spec = pin.partition("@")
    return spec.split("+", 1)[0].strip()


def _check_pnpm_version(node_env, log_path):
    """Log the pnpm version the build will actually use and compare it to
    the version pinned by upstream.

    The pin is deliberate: the lockfile is validated against that pnpm
    major. A mismatch means somebody ran `corepack use pnpm@...` inside
    ~/immich/src, which rewrites package.json and makes the next
    `git checkout <tag>` fail. Report it, but do not abort the update --
    the build steps themselves fail loudly if the newer pnpm misbehaves.
    """
    rc, out = run_cmd(["pnpm", "--version"], cwd=IMMICH_SRC, env=node_env,
                      log_path=log_path, timeout=120)
    effective = (out or "").strip().splitlines()[-1].strip() if out else ""
    pin = _pnpm_pin()
    log_write(log_path, "pnpm in use: %s\n" % (effective or "?"))
    log_write(log_path, "packageManager pin: %s\n" % (pin or "(none)"))
    if pin and effective and effective != _pnpm_pin_version(pin):
        log_write(log_path,
                  "WARN: pnpm %s does not match the pinned %s; the pin comes "
                  "from upstream and must not be changed locally\n"
                  % (effective, pin))
    return 0, effective


# `/api/health` was dropped upstream (it now falls through to the SPA and
# returns 200 HTML); `/api/server/ping` is the long-lived liveness route.
# Both are tried so a future rename does not silently break the smoke test.
_SMOKE_URLS = (
    "http://127.0.0.1:2283/api/server/ping",
    "http://127.0.0.1:2283/api/health",
)


def _wait_healthy(log_path, urls=_SMOKE_URLS, deadline=180):
    """Poll the immich API until it answers or the deadline passes.

    `systemctl restart` returns as soon as the units are started, while the
    server needs a few seconds to bind :2283 -- a single immediate curl
    reported a false failure right after a successful upgrade.
    """
    end = time.time() + deadline
    attempt = 0
    rc, out = 1, "not attempted"
    while True:
        attempt += 1
        for url in urls:
            rc, out = run_cmd(["curl", "-fsS", url],
                              log_path=log_path if attempt == 1 else None,
                              timeout=30)
            if rc == 0:
                if attempt > 1:
                    log_write(log_path, "healthy after %d attempts: %s\n" % (attempt, url))
                return rc, out
        if time.time() >= end:
            break
        time.sleep(5)
    log_write(log_path, "immich API did not answer within %ss\n" % deadline)
    return rc, out


def _plugin_env():
    """`_tool_env()` plus the extism-js / wasm-merge tool dirs.

    Both are hand-installed under ~/immich/tools and are not on any service
    PATH. binaryen's dir carries its version in the name, so it is discovered
    instead of hardcoded.
    """
    env = _tool_env()
    tools = STORAGE_HOME / "immich" / "tools"
    dirs = [tools / "extism-js"]
    dirs += sorted(p / "bin" for p in (tools / "binaryen").glob("binaryen-*"))
    found = [d for d in dirs if d.is_dir()]
    if found:
        env["PATH"] = os.pathsep.join([str(d) for d in found] + [env["PATH"]])
    return env


def _build_core_plugin(node_env, log_path):
    """Build the core workflow plugin and install it where immich loads it.

    immich reads it from <build>/plugins/immich-plugin-core/{manifest.json,dist}
    (the server/Dockerfile layout); without it the server logs
    "Failed to import plugin ..." on every boot and core templates are missing.
    """
    rc, out = run_cmd(
        ["pnpm", "--filter", "@immich/sdk", "--filter", "@immich/plugin-sdk",
         "--filter", "@immich/plugin-core", "build"],
        cwd=IMMICH_SRC, env=node_env, log_path=log_path, timeout=1800)
    if rc != 0:
        return rc, out

    src = IMMICH_SRC / "packages" / "plugin-core"
    dst = IMMICH_SRC.parent / "build" / "plugins" / "immich-plugin-core"
    (dst / "dist").mkdir(parents=True, exist_ok=True)
    shutil.copy2(src / "manifest.json", dst / "manifest.json")
    shutil.rmtree(dst / "dist", ignore_errors=True)
    shutil.copytree(src / "dist", dst / "dist")
    log_write(log_path, "installed core plugin to %s\n" % dst)
    return 0, ""


def _apply_local_patches(patches_dir, log_path):
    """Re-apply the local immich patches kept in this repo.

    They live outside the immich tree (branch `local/patches` in ~/immich/src
    and tools/immich/patches here), so every `git checkout <tag>` wipes them
    and the update has to put them back before the server is built.

    Best effort: a release may have fixed what the patch works around, so a
    patch that no longer applies is reported instead of failing the upgrade.
    """
    patches = sorted(Path(patches_dir).glob("*.patch"))
    if not patches:
        log_write(log_path, "no local patches in %s\n" % patches_dir)
        return 0, ""

    # Never touch a tree that already carries tracked edits: undoing a failed
    # 3-way merge would throw them away.
    rc, out = run_cmd(["git", "status", "--porcelain"], cwd=IMMICH_SRC, log_path=log_path)
    dirty = [line for line in (out or "").splitlines()
             if line.strip() and not line.startswith("??")]
    if rc != 0 or dirty:
        log_write(log_path, "skipping local patches: tracked files are modified\n")
        return 1, out

    failed = []
    for patch in patches:
        name = patch.name
        # already in the tree (e.g. someone sits on branch local/patches)
        rc, _ = run_cmd(["git", "apply", "--reverse", "--check", str(patch)],
                        cwd=IMMICH_SRC, log_path=log_path)
        if rc == 0:
            log_write(log_path, "%s already applied\n" % name)
            continue
        rc, _ = run_cmd(["git", "apply", "--check", str(patch)],
                        cwd=IMMICH_SRC, log_path=log_path)
        if rc == 0:
            rc, _ = run_cmd(["git", "apply", str(patch)], cwd=IMMICH_SRC, log_path=log_path)
            if rc != 0:
                failed.append(name)
            continue
        # context moved: try a 3-way merge, it is allowed to conflict
        rc, _ = run_cmd(["git", "apply", "--3way", str(patch)],
                        cwd=IMMICH_SRC, log_path=log_path)
        if rc == 0:
            log_write(log_path, "%s applied with --3way\n" % name)
            continue
        # a conflicted --3way leaves markers in the files; restore the tag
        run_cmd(["git", "reset", "--hard", "HEAD"], cwd=IMMICH_SRC, log_path=log_path)
        log_write(log_path, "WARN: %s no longer applies; the tree was restored\n" % name)
        failed.append(name)

    if failed:
        return 1, "patches failed: %s" % ", ".join(failed)
    log_write(log_path, "local patches: %d applied\n" % len(patches))
    return 0, ""


def _patched_files(patches_dir):
    """Repo-relative paths of the tracked files the local patches touch."""
    files = []
    for patch in sorted(Path(patches_dir).glob("*.patch")):
        rc, out = run_cmd(["git", "apply", "--numstat", str(patch)], cwd=IMMICH_SRC)
        if rc != 0:
            continue
        for line in (out or "").splitlines():
            parts = line.split("\t")
            if len(parts) < 3:
                continue
            path = parts[2].strip()
            if "=>" in path:  # rename: keep the destination path
                path = path.split("=>")[-1].strip().strip("{}")
            if path and path not in files:
                files.append(path)
    return files


def _reset_patched_files(patches_dir, log_path):
    """Restore the files the local patches touch to their HEAD state.

    The patch is kept in this repo and re-applied right after the checkout,
    so the working tree must not carry it: a modified tracked file makes
    `git checkout <tag>` fail ("local changes would be overwritten") as
    soon as upstream touches that file. Upstream did change the patched
    file in v3.3.1, so this is a real risk, not a theoretical one.
    """
    files = _patched_files(patches_dir)
    if not files:
        return 0, ""
    rc, out = run_cmd(["git", "checkout", "--", *files], cwd=IMMICH_SRC, log_path=log_path)
    if rc != 0:
        log_write(log_path, "WARN: could not reset patched files: %s\n"
                  % (out or "").strip())
        return 1, out
    log_write(log_path, "reset patched files before checkout: %s\n" % ", ".join(files))
    return 0, ""


def _snapshot(paths, backup_dir):
    """Copy the current content of the given repo-relative files."""
    saved = {}
    for rel in paths:
        src = IMMICH_SRC / rel
        if not src.is_file():
            continue
        dst = Path(backup_dir) / rel.replace("/", "__")
        shutil.copy2(src, dst)
        saved[rel] = dst
    return saved


def _restore_snapshot(saved):
    """Put the snapshotted file contents back (checkout failed, tree intact)."""
    for rel, dst in saved.items():
        try:
            shutil.copy2(dst, IMMICH_SRC / rel)
        except Exception:
            pass


def _push(run, steps):
    run.steps = json.dumps(steps)
    db.session.commit()


def run_immich_update(run_id, tag):
    from wsgi import app
    with app.app_context():
        run = UpdateRun.query.get(run_id)
        if not run:
            return
        log_path = Path(run.log_path)
        run.status = "running"
        db.session.commit()
        steps = []
        node_env = _tool_env()
        mig_env = dict(node_env)
        mig_env.update(_env_file(IMMICH_ENV))
        plugin_env = _plugin_env()

        def step(name, fn):
            steps.append({"name": name, "status": "running"})
            _push(run, steps)
            log_write(log_path, "\n=== %s ===\n" % name)
            rc, out = fn()
            steps[-1]["status"] = "ok" if rc == 0 else "failed"
            steps[-1]["rc"] = rc
            _push(run, steps)
            return rc, out

        try:
            ts = datetime.now().strftime("%Y-%m-%d_%H%M")
            (BACKUP_ROOT / "immich").mkdir(parents=True, exist_ok=True)
            # transient work belongs in /tmp, not on the data/backup disks
            snapshot_dir = tempfile.mkdtemp(prefix="immich-update-")

            rc, _ = step("backup_db", lambda: run_cmd(
                ["pg_dump", "--dbname=postgresql://storage@localhost:5432/immich", "-Fc",
                 "-f", str(BACKUP_ROOT / "immich" / ("immich_db_preupdate_%s.dump" % ts))],
                log_path=log_path, timeout=3600))
            if rc != 0:
                raise RuntimeError("db backup failed")

            rc, _ = step("backup_config", lambda: run_cmd(
                ["tar", "czf", str(BACKUP_ROOT / "immich" / ("immich_config_preupdate_%s.tar.gz" % ts)),
                 "-C", str(STORAGE_HOME), "immich/.env"], log_path=log_path))
            if rc != 0:
                raise RuntimeError("config backup failed")

            rc, _ = step("git_fetch", lambda: run_cmd(
                ["git", "fetch", "origin", "--tags"], cwd=IMMICH_SRC, log_path=log_path))
            if rc != 0:
                raise RuntimeError("git fetch failed")

            # Tracked files that were edited locally make `git checkout` fail
            # ("local changes would be overwritten"); log them up front so
            # the failed checkout is diagnosable.
            rc, out = run_cmd(["git", "status", "--porcelain"], cwd=IMMICH_SRC)
            dirty = [line for line in (out or "").splitlines()
                     if line.strip() and not line.startswith("??")]
            if dirty:
                log_write(log_path, "locally modified tracked files:\n%s\n"
                          % "\n".join(dirty))

            # The local patch must not be carried by the working tree across
            # the checkout: keep a copy, reset the file, and let
            # apply_local_patches put the patch back from this repo.
            patched = _patched_files(IMMICH_PATCHES_DIR)
            snapshot = _snapshot(patched, snapshot_dir)

            rc, _ = step("reset_patched_files", lambda: _reset_patched_files(
                IMMICH_PATCHES_DIR, log_path))

            rc, _ = step("git_checkout", lambda: run_cmd(
                ["git", "checkout", tag], cwd=IMMICH_SRC, log_path=log_path))
            if rc != 0:
                _restore_snapshot(snapshot)
                raise RuntimeError("git checkout failed")

            # Local patches are wiped by the checkout above. Non-fatal: a
            # release may have fixed what they work around.
            rc, _ = step("apply_local_patches", lambda: _apply_local_patches(
                IMMICH_PATCHES_DIR, log_path))
            if rc != 0:
                log_write(log_path, "WARN: local patches skipped; continuing\n")

            step("pnpm_version", lambda: _check_pnpm_version(node_env, log_path))

            rc, _ = step("pnpm_install", lambda: run_cmd(
                ["pnpm", "install", "--frozen-lockfile"], cwd=IMMICH_SRC,
                env=node_env, log_path=log_path, timeout=1800))
            if rc != 0:
                raise RuntimeError("pnpm install failed")

            rc, _ = step("build_server", lambda: run_cmd(
                ["pnpm", "run", "build"], cwd=IMMICH_SRC / "server",
                env=node_env, log_path=log_path, timeout=1800))
            if rc != 0:
                raise RuntimeError("server build failed")
            rc, _ = step("build_web", lambda: run_cmd(
                ["pnpm", "run", "build"], cwd=IMMICH_SRC / "web",
                env=node_env, log_path=log_path, timeout=1800))
            if rc != 0:
                raise RuntimeError("web build failed")

            # Not fatal: without it immich just logs "Failed to import plugin"
            # and core workflow templates are unavailable.
            rc, _ = step("build_plugin_core", lambda: _build_core_plugin(plugin_env, log_path))
            if rc != 0:
                log_write(log_path, "WARN: core plugin build failed; continuing\n")

            rc, _ = step("migrations", lambda: run_cmd(
                ["pnpm", "run", "migrations:run"], cwd=IMMICH_SRC / "server",
                env=mig_env, log_path=log_path, timeout=600))
            if rc != 0:
                raise RuntimeError("immich migrations failed")

            step("restart_services", lambda: run_cmd(
                ["systemctl", "--user", "restart", "immich-ml.service", "immich-server.service"],
                log_path=log_path))

            rc, _ = step("smoke_test", lambda: _wait_healthy(log_path))

            run.status = "success" if rc == 0 else "failed"
            run.to_version = tag
        except Exception as e:
            run.status = "failed"
            log_write(log_path, "\nFAILED: %s\n" % e)
        finally:
            shutil.rmtree(snapshot_dir, ignore_errors=True)
            run.finished_at = datetime.utcnow()
            run.steps = json.dumps(steps)
            db.session.commit()
            db.session.add(AuditLog(actor="admin", action="update.immich", obj=tag, payload=run.status))
            db.session.commit()

from pathlib import Path
from datetime import datetime
import os, json, time, shutil

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
    return env


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
    rc, _ = run_cmd(
        ["pnpm", "--filter", "@immich/sdk", "--filter", "@immich/plugin-sdk",
         "--filter", "@immich/plugin-core", "build"],
        cwd=IMMICH_SRC, env=node_env, log_path=log_path, timeout=1800)
    if rc != 0:
        return rc

    src = IMMICH_SRC / "packages" / "plugin-core"
    dst = IMMICH_SRC.parent / "build" / "plugins" / "immich-plugin-core"
    (dst / "dist").mkdir(parents=True, exist_ok=True)
    shutil.copy2(src / "manifest.json", dst / "manifest.json")
    shutil.rmtree(dst / "dist", ignore_errors=True)
    shutil.copytree(src / "dist", dst / "dist")
    log_write(log_path, "installed core plugin to %s\n" % dst)
    return 0


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
        return 0

    # Never touch a tree that already carries tracked edits: undoing a failed
    # 3-way merge would throw them away.
    rc, out = run_cmd(["git", "status", "--porcelain"], cwd=IMMICH_SRC, log_path=log_path)
    dirty = [line for line in (out or "").splitlines()
             if line.strip() and not line.startswith("??")]
    if rc != 0 or dirty:
        log_write(log_path, "skipping local patches: tracked files are modified\n")
        return 1

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
        return 1
    log_write(log_path, "local patches: %d applied\n" % len(patches))
    return 0


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
            rc, _ = step("git_checkout", lambda: run_cmd(
                ["git", "checkout", tag], cwd=IMMICH_SRC, log_path=log_path))
            if rc != 0:
                raise RuntimeError("git checkout failed")

            # Local patches are wiped by the checkout above. Non-fatal: a
            # release may have fixed what they work around.
            rc, _ = step("apply_local_patches", lambda: _apply_local_patches(
                IMMICH_PATCHES_DIR, log_path))
            if rc != 0:
                log_write(log_path, "WARN: local patches skipped; continuing\n")

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
            run.finished_at = datetime.utcnow()
            run.steps = json.dumps(steps)
            db.session.commit()
            db.session.add(AuditLog(actor="admin", action="update.immich", obj=tag, payload=run.status))
            db.session.commit()

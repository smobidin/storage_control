from pathlib import Path
from datetime import datetime
import os, json, re, shutil

from ..extensions import db
from ..models import UpdateRun, AuditLog
from ..config import NEXTCLOUD_ROOT, STORAGE_HOME, BACKUP_ROOT, RUNS_DIR
from .common import run_cmd, log_write


def _occ(args, log_path, timeout=600):
    return run_cmd(["php", "occ"] + args, cwd=NEXTCLOUD_ROOT, log_path=log_path, timeout=timeout)


def _push(run, steps):
    run.steps = json.dumps(steps)
    db.session.commit()


# Substrings that mean a step really failed even if the process exited 0.
# PHP prints "An unhandled exception has been thrown" before exit(1); some
# toolchains still return 0. Without this the recorded status could claim
# success over a crashed step.
_FATAL_MARKERS = (
    "An unhandled exception has been thrown",
    "Traceback (most recent call last)",
)


def _fatal_in(output):
    if not output:
        return False
    return any(m in output for m in _FATAL_MARKERS)


def _occ_status_problems(out, expect_version=None, check_maintenance=True):
    """Parse `occ status` output and return a list of problems ([] == healthy).

    `occ status` exits 0 even when the instance is broken, so the exit code is
    useless as a health signal -- we parse the printed fields instead.
    """
    info = {}
    for line in (out or "").splitlines():
        s = line.strip()
        if s.startswith("-"):
            s = s[1:].strip()
        if ":" not in s:
            continue
        k, v = s.split(":", 1)
        info[k.strip()] = v.strip()

    problems = []
    if info.get("installed") != "true":
        problems.append(f"installed={info.get('installed')!r}")
    if check_maintenance and info.get("maintenance") != "false":
        problems.append("still in maintenance mode")
    if info.get("needsDbUpgrade") != "false":
        problems.append("needsDbUpgrade=true")
    if expect_version and info.get("versionstring") != expect_version.lstrip("v"):
        problems.append(f"versionstring={info.get('versionstring')!r}, "
                        f"expected {expect_version.lstrip('v')}")
    return problems


def _viewer_max_major(info_path):
    """Read <nextcloud max-version=".."> from an app's info.xml, or None."""
    try:
        text = Path(info_path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    m = re.search(r"<nextcloud[^>]*\bmax-version=[\"'](\d+)[\"']", text)
    return int(m.group(1)) if m else None


def _sync_viewer(tag, log_path):
    """Make sure apps/viewer is present and supports the version we upgraded to.

    The server's .gitignore ignores /apps/*, so `git checkout <tag>` neither
    updates nor removes viewer -- and on a major version jump Nextcloud marks
    the old one incompatible and disables it. Without viewer the LoadViewer
    event never fires, richdocuments never registers its handler and clicking
    a document downloads the file instead of opening Collabora. The app store
    does not carry viewer either, so the release tarball is the only source.
    """
    version = str(tag).lstrip("v")
    try:
        major = int(version.split(".")[0])
    except ValueError:
        return 1, f"cannot parse major version out of tag {tag!r}\n"

    viewer_dir = NEXTCLOUD_ROOT / "apps" / "viewer"
    installed_major = _viewer_max_major(viewer_dir / "appinfo" / "info.xml")
    if installed_major is not None and installed_major >= major:
        msg = f"viewer max-version={installed_major} covers server {version}; nothing to do\n"
        log_write(log_path, msg)
        return 0, msg

    # No viewer, or one left over from the previous major version: pull it out
    # of the tarball that matches the release we just checked out.
    url = f"https://download.nextcloud.com/server/releases/nextcloud-{version}.tar.bz2"
    log_write(log_path, f"viewer missing or incompatible (max-version={installed_major}); "
                        f"refreshing from {url}\n")
    RUNS_DIR.mkdir(parents=True, exist_ok=True)
    archive = RUNS_DIR / f"nextcloud-{version}.tar.bz2"
    rc, out = run_cmd(["curl", "-sSfL", "--retry", "3", "--connect-timeout", "30",
                       "-o", str(archive), url], log_path=log_path, timeout=7200)
    if rc != 0:
        return rc, out + "failed to download release tarball for viewer refresh\n"

    # Unpack somewhere Nextcloud does not scan for apps, so a truncated
    # download can never end up as a half-written installed app.
    stage = RUNS_DIR / "viewer_stage"
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)
    rc, out = run_cmd(["tar", "-xjf", str(archive), "-C", str(stage),
                       "--strip-components=1", "nextcloud/apps/viewer"],
                      log_path=log_path, timeout=600)
    if rc != 0:
        return rc, out
    staged_major = _viewer_max_major(stage / "appinfo" / "info.xml")
    if staged_major is None or staged_major < major:
        return 1, (f"tarball viewer max-version={staged_major} does not cover "
                   f"server {version}\n")

    if viewer_dir.exists():
        shutil.rmtree(viewer_dir)
    shutil.move(str(stage), str(viewer_dir))

    rc, out = _occ(["app:enable", "viewer"], log_path, timeout=120)
    if rc != 0:
        return rc, out
    rc, out = _occ(["app:list", "--enabled"], log_path, timeout=120)
    if rc != 0:
        return rc, out
    if not re.search(r"^\s*-\s*viewer:", out, re.M):
        return 1, "viewer still not enabled after refresh\n"
    log_write(log_path, f"viewer refreshed to {version}\n")
    try:
        archive.unlink()  # ~250 MB per refresh
    except OSError:
        pass
    return 0, out


def run_nextcloud_update(run_id, tag):
    from wsgi import app
    with app.app_context():
        run = UpdateRun.query.get(run_id)
        if not run:
            return
        log_path = Path(run.log_path)
        run.status = "running"
        db.session.commit()
        steps = []

        def step(name, fn):
            steps.append({"name": name, "status": "running"})
            _push(run, steps)
            log_write(log_path, f"\n=== {name} ===\n")
            rc, out = fn()
            if rc == 0 and _fatal_in(out):
                log_write(log_path, "WARN: command exited 0 but printed a fatal "
                                     "error; marking step failed\n")
                rc = 1
            steps[-1]["status"] = "ok" if rc == 0 else "failed"
            steps[-1]["rc"] = rc
            _push(run, steps)
            return rc, out

        try:
            ts = datetime.now().strftime("%Y-%m-%d_%H%M")
            (BACKUP_ROOT / "nextcloud").mkdir(parents=True, exist_ok=True)

            rc, _ = step("backup_db", lambda: run_cmd(
                ["pg_dump", "--dbname=postgresql://nextcloud_db@localhost:5432/nextcloud_db", "-Fc",
                 "-f", str(BACKUP_ROOT / "nextcloud" / f"nextcloud_db_preupdate_{ts}.dump")],
                log_path=log_path, timeout=3600))
            if rc != 0:
                raise RuntimeError("db backup failed")

            rc, _ = step("backup_config", lambda: run_cmd(
                ["tar", "czf", str(BACKUP_ROOT / "nextcloud" / f"nextcloud_config_preupdate_{ts}.tar.gz"),
                 "-C", str(NEXTCLOUD_ROOT), "config/config.php"], log_path=log_path))
            if rc != 0:
                raise RuntimeError("config backup failed")

            rc, _ = step("maintenance_on", lambda: _occ(["maintenance:mode", "--on"], log_path))
            if rc != 0:
                raise RuntimeError("maintenance mode on failed")

            rc, _ = step("git_fetch", lambda: run_cmd(
                ["git", "fetch", "origin", "--tags", "--no-recurse-submodules"], cwd=NEXTCLOUD_ROOT, log_path=log_path))
            if rc != 0:
                raise RuntimeError("git fetch failed")
            rc, _ = step("git_checkout", lambda: run_cmd(
                ["git", "checkout", tag], cwd=NEXTCLOUD_ROOT, log_path=log_path))
            if rc != 0:
                raise RuntimeError("git checkout failed")

            # Bring submodules (3rdparty, ...) to the checked-out tag. Skipping
            # this leaves 3rdparty on the old release and `occ upgrade` fails
            # with missing classes / Symfony errors.
            rc, _ = step("git_submodules", lambda: run_cmd(
                ["git", "submodule", "update", "--init", "--recursive"],
                cwd=NEXTCLOUD_ROOT, log_path=log_path, timeout=1800))
            if rc != 0:
                raise RuntimeError("git submodule update failed")

            rc, _ = step("build_js", lambda: run_cmd(
                ["make", "build-js-production"], cwd=NEXTCLOUD_ROOT, log_path=log_path, timeout=1800))
            if rc != 0:
                log_write(log_path, "WARN: build-js-production failed; continuing\n")

            rc, _ = step("occ_upgrade", lambda: _occ(["upgrade"], log_path, timeout=1800))
            if rc != 0:
                raise RuntimeError("occ upgrade failed")

            # Fail fast if the upgrade did not actually apply. Maintenance is
            # still on at this point, so only version + db state are checked.
            rc, out = step("verify_upgrade", lambda: _occ(["status"], log_path, timeout=120))
            problems = _occ_status_problems(out, expect_version=tag, check_maintenance=False)
            if problems:
                raise RuntimeError("upgrade did not apply: " + "; ".join(problems))

            step("integrity_check", lambda: _occ(["integrity:check-core"], log_path))
            step("add_missing_indices", lambda: _occ(["db:add-missing-indices"], log_path))

            rc, _ = step("maintenance_off", lambda: _occ(["maintenance:mode", "--off"], log_path))
            if rc != 0:
                raise RuntimeError("maintenance mode off failed")

            step("restart_services", lambda: run_cmd(
                ["sudo", "systemctl", "restart", "php8.5-fpm", "nginx"], log_path=log_path))

            rc, out = step("smoke_test", lambda: run_cmd(
                ["php", "occ", "status"], cwd=NEXTCLOUD_ROOT, log_path=log_path, timeout=120))

            # `occ status` exits 0 even when the instance is broken, so decide
            # the final status from the parsed fields, not the bare exit code.
            problems = _occ_status_problems(out, expect_version=tag, check_maintenance=True)
            if rc == 0 and _fatal_in(out):
                problems.append("smoke test printed a fatal error")
            if problems:
                log_write(log_path, "SMOKE TEST FAILED: " + "; ".join(problems) + "\n")
                run.status = "failed"
            else:
                run.status = "success"
                run.to_version = tag
        except Exception as e:
            run.status = "failed"
            try:
                _occ(["maintenance:mode", "--off"], log_path)
            except Exception:
                pass
            log_write(log_path, f"\nFAILED: {e}\n")
        finally:
            run.finished_at = datetime.utcnow()
            run.steps = json.dumps(steps)
            db.session.commit()
            db.session.add(AuditLog(actor="admin", action="update.nextcloud", obj=tag, payload=run.status))
            db.session.commit()
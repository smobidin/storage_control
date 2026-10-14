from pathlib import Path
from datetime import datetime
import os, json

from ..extensions import db
from ..models import UpdateRun, AuditLog
from ..config import NEXTCLOUD_ROOT, STORAGE_HOME, BACKUP_ROOT, RUNS_DIR
from .common import run_cmd, log_write


def _occ(args, log_path, timeout=600):
    return run_cmd(["php", "occ"] + args, cwd=NEXTCLOUD_ROOT, log_path=log_path, timeout=timeout)


def _push(run, steps):
    run.steps = json.dumps(steps)
    db.session.commit()


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

            rc, _ = step("build_js", lambda: run_cmd(
                ["make", "build-js-production"], cwd=NEXTCLOUD_ROOT, log_path=log_path, timeout=1800))
            if rc != 0:
                log_write(log_path, "WARN: build-js-production failed; continuing\n")

            rc, _ = step("occ_upgrade", lambda: _occ(["upgrade"], log_path, timeout=1800))
            if rc != 0:
                raise RuntimeError("occ upgrade failed")

            step("integrity_check", lambda: _occ(["integrity:check-core"], log_path))
            step("add_missing_indices", lambda: _occ(["db:add-missing-indices"], log_path))

            rc, _ = step("maintenance_off", lambda: _occ(["maintenance:mode", "--off"], log_path))
            if rc != 0:
                raise RuntimeError("maintenance mode off failed")

            step("restart_services", lambda: run_cmd(
                ["sudo", "systemctl", "restart", "php8.5-fpm", "nginx"], log_path=log_path))

            rc, _ = step("smoke_test", lambda: run_cmd(
                ["php", "occ", "status"], cwd=NEXTCLOUD_ROOT, log_path=log_path, timeout=120))

            run.status = "success" if rc == 0 else "failed"
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
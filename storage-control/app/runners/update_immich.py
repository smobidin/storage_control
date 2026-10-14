from pathlib import Path
from datetime import datetime
import os, json

from ..extensions import db
from ..models import UpdateRun, AuditLog
from ..config import IMMICH_SRC, IMMICH_ENV, STORAGE_HOME, BACKUP_ROOT, RUNS_DIR
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

            rc, _ = step("pnpm_install", lambda: run_cmd(
                ["pnpm", "install", "--frozen-lockfile"], cwd=IMMICH_SRC, log_path=log_path, timeout=1800))
            if rc != 0:
                raise RuntimeError("pnpm install failed")

            rc, _ = step("build_server", lambda: run_cmd(
                ["pnpm", "run", "build"], cwd=IMMICH_SRC / "server", log_path=log_path, timeout=1800))
            if rc != 0:
                raise RuntimeError("server build failed")
            rc, _ = step("build_web", lambda: run_cmd(
                ["pnpm", "run", "build"], cwd=IMMICH_SRC / "web", log_path=log_path, timeout=1800))
            if rc != 0:
                raise RuntimeError("web build failed")

            env = dict(os.environ)
            env.update(_env_file(IMMICH_ENV))
            rc, _ = step("migrations", lambda: run_cmd(
                ["pnpm", "run", "migrations:run"], cwd=IMMICH_SRC / "server", env=env, log_path=log_path, timeout=600))
            if rc != 0:
                raise RuntimeError("immich migrations failed")

            step("restart_services", lambda: run_cmd(
                ["systemctl", "--user", "restart", "immich-ml.service", "immich-server.service"],
                log_path=log_path))

            rc, _ = step("smoke_test", lambda: run_cmd(
                ["curl", "-fsS", "http://127.0.0.1:2283/api/health"], log_path=log_path, timeout=60))

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

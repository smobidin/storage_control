import json, os, time
from pathlib import Path
from datetime import datetime

from ..config import BACKUP_ROOT, STORAGE_HOME
from ..extensions import db
from ..models import BackupTask, BackupRun
from .common import run_cmd, log_write


def _pg_dump(dbname, user, outpath, log_path):
    url = None
    # build URL
    url = f"postgresql://{user}@localhost:5432/{dbname}"
    return run_cmd(["pg_dump", f"--dbname={url}", "-Fc", "-f", str(outpath)], log_path=log_path, timeout=3600)


def execute_backup_task(task_id):
    from wsgi import app
    with app.app_context():
        task = BackupTask.query.get(task_id)
        if not task:
            return
        run = BackupRun(task_id=task.id, status="running")
        db.session.add(run)
        db.session.commit()
        rid = run.id
        # outdir
        ts = datetime.now().strftime("%Y-%m-%d_%H%M")
        outdir = BACKUP_ROOT
        Path(outdir).mkdir(parents=True, exist_ok=True)
        files = []
        log_path = (app.config["RUNS_DIR"] / f"backup_{rid}.log")
        rc_total = 0
        msg = ""

        kind = task.kind
        try:
            if kind == "immich_db":
                out = outdir / "immich" / f"immich_db_{ts}.dump"
                Path(out).parent.mkdir(parents=True, exist_ok=True)
                rc, _ = _pg_dump("immich", "storage", out, log_path)
                files.append(str(out)); rc_total = rc
            elif kind == "nextcloud_db":
                out = outdir / "nextcloud" / f"nextcloud_db_{ts}.dump"
                Path(out).parent.mkdir(parents=True, exist_ok=True)
                rc, _ = _pg_dump("nextcloud_db", "nextcloud_db", out, log_path)
                files.append(str(out)); rc_total = rc
            elif kind == "immich_config":
                out = outdir / "immich" / f"immich_config_{ts}.tar.gz"
                Path(out).parent.mkdir(parents=True, exist_ok=True)
                rc, _ = run_cmd(["tar", "czf", str(out), "-C", str(STORAGE_HOME), "immich/.env", ".config/systemd/user"], log_path=log_path)
                files.append(str(out)); rc_total = rc
            elif kind == "nextcloud_config":
                out = outdir / "nextcloud" / f"nextcloud_config_{ts}.tar.gz"
                Path(out).parent.mkdir(parents=True, exist_ok=True)
                rc, _ = run_cmd(["tar", "czf", str(out), "-C", str(STORAGE_HOME / "nextcloud"), "config/config.php"], log_path=log_path)
                files.append(str(out)); rc_total = rc
            elif kind == "all":
                script = STORAGE_HOME / "tools" / "backup_immich_nc.sh"
                rc, _ = run_cmd(["bash", str(script)], log_path=log_path, timeout=7200)
                rc_total = rc
            elif kind == "custom":
                if not task.command:
                    raise RuntimeError("no command")
                rc, _ = run_cmd(["bash", "-lc", task.command], log_path=log_path, timeout=3600)
                rc_total = rc
            else:
                raise RuntimeError(f"unknown kind {kind}")
        except Exception as e:
            log_write(log_path, f"\nEXCEPTION: {e}\n")
            rc_total = 1
            msg = str(e)

        run = BackupRun.query.get(rid)
        run.finished_at = datetime.utcnow()
        run.exit_code = rc_total
        run.status = "success" if rc_total == 0 else "failed"
        run.outdir = str(outdir)
        run.files = json.dumps(files)
        try:
            run.log = Path(log_path).read_text()[-4000:]
        except Exception:
            run.log = ""
        db.session.commit()

        task.last_run_at = datetime.utcnow()
        task.last_status = run.status
        db.session.commit()


def run_retention_cleanup():
    from wsgi import app
    with app.app_context():
        for task in BackupTask.query.all():
            if not task.retention_days:
                continue
            cutoff = datetime.now() - timedelta(days=task.retention_days)
            # scan backup root best-effort: delete files older than cutoff with known extensions
            for root, _, fs in os.walk(BACKUP_ROOT):
                for f in fs:
                    if not f.endswith((".dump", ".tar.gz", ".txt")):
                        continue
                    p = Path(root) / f
                    try:
                        if datetime.fromtimestamp(p.stat().st_mtime) < cutoff:
                            p.unlink(missing_ok=True)
                    except Exception:
                        pass

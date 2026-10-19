from pathlib import Path
from datetime import datetime
import json, os, shutil, tempfile, urllib.request, tarfile, time

from ..extensions import db
from ..models import UpdateRun, AuditLog
from ..config import (STALWART_HOME, STALWART_BIN, STALWART_DATA,
                      BACKUP_ROOT, RUNS_DIR, STORAGE_HOME)
from .common import run_cmd, log_write


def _smoke():
    for _ in range(30):
        rc, out = run_cmd(["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}",
                           "http://127.0.0.1:8082/admin/"], timeout=10)
        if rc == 0 and out.strip() in ("200", "301", "302", "303"):
            return True
        time.sleep(2)
    return False


def run_stalwart_update(run_id, tag):
    from wsgi import app
    with app.app_context():
        run = UpdateRun.query.get(run_id)
        if not run:
            return
        log_path = Path(run.log_path)
        log_path.parent.mkdir(parents=True, exist_ok=True)
        run.status = "running"
        db.session.commit()
        steps = []

        def record(name, rc):
            steps.append({"name": name, "rc": rc, "status": "ok" if rc == 0 else "failed"})
            run.steps = json.dumps(steps)
            db.session.commit()

        stage_root = None

        def cleanup_stage():
            if stage_root:
                shutil.rmtree(stage_root, ignore_errors=True)

        def fail(exc):
            run.status = "failed"
            run.error if hasattr(run, "error") else None
            run.finished_at = datetime.utcnow()
            db.session.commit()
            log_write(log_path, "\nFAILED: %s\n" % exc)
            cleanup_stage()
            # ensure service back up where possible
            run_cmd(["systemctl", "--user", "start", "stalwart.service"], log_path=str(log_path))

        try:
            log_write(log_path, "Updating stalwart to %s\n" % tag)
            # stop service
            rc, _ = run_cmd(["systemctl", "--user", "stop", "stalwart.service"], log_path=str(log_path))
            record("stop_service", rc)

            # backup current binary (a real pre-update backup artifact, lives in
            # BACKUP_ROOT like the immich/nextcloud pre-update dumps)
            base = BACKUP_ROOT / "stalwart" / str(run_id)
            base.mkdir(parents=True, exist_ok=True)
            if STALWART_BIN.exists():
                shutil.copy2(STALWART_BIN, base / "stalwart.bak")
            record("backup_binary", 0)

            # download into /tmp: the update staging area must not touch
            # /mnt/data (service data only) or BACKUP_ROOT (backups only)
            stage_root = Path(tempfile.mkdtemp(prefix="stalwart-update-%d_" % run_id))
            url = "https://github.com/stalwartlabs/stalwart/releases/download/%s/stalwart-x86_64-unknown-linux-gnu.tar.gz" % tag
            tar_path = stage_root / "stalwart.tar.gz"
            log_write(log_path, "Downloading %s\n" % url)
            try:
                urllib.request.urlretrieve(url, tar_path)
                record("download", 0)
            except Exception as e:
                record("download", 1)
                raise RuntimeError("download failed: %s" % e)

            # extract
            tmp = stage_root / "extract"
            with tarfile.open(tar_path, "r:gz") as tf:
                tf.extractall(tmp)
            # find binary
            candidates = list(tmp.rglob("stalwart"))
            if not candidates:
                record("extract", 1)
                raise RuntimeError("binary not found in archive")
            new_bin = candidates[0]
            record("extract", 0)

            # replace (cross-device safe: copy onto the target filesystem first,
            # os.replace cannot rename across filesystems)
            stage = STALWART_BIN.with_name(STALWART_BIN.name + ".new")
            shutil.copy2(new_bin, stage)
            try:
                os.chmod(stage, 0o755)
            except Exception:
                pass
            os.replace(stage, STALWART_BIN)
            try:
                os.chmod(STALWART_BIN, 0o755)
            except Exception:
                pass
            record("replace_binary", 0)

            # start service
            rc, _ = run_cmd(["systemctl", "--user", "start", "stalwart.service"], log_path=str(log_path), timeout=120)
            record("start_service", rc)
            if rc != 0:
                raise RuntimeError("failed to start stalwart")

            # smoke
            if _smoke():
                record("smoke", 0)
            else:
                record("smoke", 1)
                raise RuntimeError("stalwart smoke test failed")

            run.status = "success"
            run.finished_at = datetime.utcnow()
            db.session.commit()
            db.session.add(AuditLog(actor="admin", action="update.run", obj="stalwart", payload="%s -> %s" % (run.from_version, tag)))
            db.session.commit()
            cleanup_stage()
            log_write(log_path, "\nUPDATE OK\n")
        except Exception as exc:
            fail(exc)
            run.status = "failed"
            run.finished_at = datetime.utcnow()
            db.session.commit()

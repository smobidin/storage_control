from pathlib import Path
from datetime import datetime
import json, shutil

from ..extensions import db
from ..models import UpdatePreserve, AuditLog
from ..config import IMMICH_SRC, IMMICH_ENV, NEXTCLOUD_ROOT, BACKUP_ROOT, STORAGE_HOME
from .common import run_cmd, log_write


def _sh(path):
    return Path(path)


def _step(steps, run_obj, name, fn, log_path=None):
    if log_path is None:
        log_path = getattr(run_obj, "log_path", None) if run_obj is not None else None
    steps.append({"name": name, "status": "running"})
    if run_obj is not None:
        run_obj.steps = __import__("json").dumps(steps)
        db.session.commit()
    log_write(log_path, "\n=== %s ===\n" % name)
    rc, out = fn()
    steps[-1]["status"] = "ok" if rc == 0 else "failed"
    steps[-1]["rc"] = rc
    if run_obj is not None:
        run_obj.steps = json.dumps(steps)
        db.session.commit()
    return rc, out


def prepare_preserve(preserve_id):
    from wsgi import app
    with app.app_context():
        preserve = UpdatePreserve.query.get(preserve_id)
        if not preserve:
            return
        log_path = Path(preserve.log_path)
        preserve.status = "preparing"
        db.session.commit()
        steps = []

        def fail(exc):
            preserve.status = "failed"
            preserve.error = str(exc)
            preserve.finished_at = datetime.utcnow()
            db.session.commit()
            log_write(log_path, "\nFAILED: %s\n" % exc)

        try:
            base = Path(preserve.storage_dir)
            base.mkdir(parents=True, exist_ok=True)
            # Derive git repository and current version depending on target.
            if preserve.target == "immich":
                repo = IMMICH_SRC
                git_before = _run_git_before(repo, log_path)
                stash_hash = _stash(repo, preserve.id, log_path)
                rc, _ = _step(steps, preserve, "backup_db", lambda: run_cmd(
                    ["pg_dump", "--dbname=postgresql://storage@localhost:5432/immich", "-Fc",
                     "-f", str(base / "immich_db.dump")], log_path=log_path, timeout=3600))
                if rc != 0:
                    raise RuntimeError("immich database backup failed")
                rc, _ = _step(steps, preserve, "backup_config", lambda: run_cmd(
                    ["tar", "czf", str(base / "immich_config.tar.gz"),
                     "-C", str(STORAGE_HOME), "immich/.env"], log_path=log_path))
                if rc != 0:
                    raise RuntimeError("immich config backup failed")
                _copy_safe(IMMICH_ENV, base / ".env")

            elif preserve.target == "nextcloud":
                repo = NEXTCLOUD_ROOT
                git_before = _run_git_before(repo, log_path)
                stash_hash = _stash(repo, preserve.id, log_path)
                rc, _ = _step(steps, preserve, "backup_db", lambda: run_cmd(
                    ["pg_dump", "--dbname=postgresql://nextcloud_db@localhost:5432/nextcloud_db", "-Fc",
                     "-f", str(base / "nextcloud_db.dump")], log_path=log_path, timeout=3600))
                if rc != 0:
                    raise RuntimeError("nextcloud database backup failed")
                rc, _ = _step(steps, preserve, "backup_config", lambda: run_cmd(
                    ["tar", "czf", str(base / "nextcloud_config.tar.gz"),
                     "-C", str(NEXTCLOUD_ROOT), "config/config.php"], log_path=log_path))
                if rc != 0:
                    raise RuntimeError("nextcloud config backup failed")
                _copy_safe(NEXTCLOUD_ROOT / "config" / "config.php", base / "config.php")
                _copy_safe(NEXTCLOUD_ROOT / ".htaccess", base / ".htaccess")

            else:
                from ..updates.versions import stalwart_current
                git_before = stalwart_current()
                stash_hash = None
                rc, _ = _step(steps, preserve, "stop_service", lambda: run_cmd(
                    ["systemctl", "--user", "stop", "stalwart.service"], log_path=log_path))
                rc, _ = _step(steps, preserve, "backup_data", lambda: run_cmd(
                    ["tar", "czf", str(base / "stalwart_data.tar.gz"), "-C", "/mnt/data", "stalwart"],
                    log_path=log_path, timeout=3600))
                if rc != 0:
                    raise RuntimeError("stalwart data backup failed")
                rc, _ = _step(steps, preserve, "backup_config", lambda: run_cmd(
                    ["tar", "czf", str(base / "stalwart_config.tar.gz"),
                     "-C", str(STORAGE_HOME), ".config/systemd/user/stalwart.service", "stalwart/etc"],
                    log_path=log_path))
                if rc != 0:
                    raise RuntimeError("stalwart config backup failed")
                run_cmd(["systemctl", "--user", "start", "stalwart.service"], log_path=log_path)

            preserve.from_version = git_before
            preserve.git_before = git_before
            preserve.git_stash = stash_hash
            if preserve.target == "immich":
                preserve.db_dump = str(base / "immich_db.dump")
                preserve.config_path = str(base / ".env")
            elif preserve.target == "nextcloud":
                preserve.db_dump = str(base / "nextcloud_db.dump")
                preserve.config_path = str(base / "config.php")
            else:
                preserve.db_dump = str(base / "stalwart_data.tar.gz")
                preserve.config_path = str(base / "stalwart_config.tar.gz")
            preserve.extra_files = json.dumps({
                "config_archive": str(base / ("%s_config.tar.gz" % preserve.target)),
                "htaccess": str(base / ".htaccess") if preserve.target == "nextcloud" else None,
            })
            preserve.status = "ready"
            preserve.finished_at = datetime.utcnow()
            steps.append({"name": "prepare_done", "status": "ok"})
            preserve.steps = json.dumps(steps)
            db.session.commit()
            db.session.add(AuditLog(actor="admin", action="update.prepare", obj=preserve.target, payload=preserve.id))
            db.session.commit()
            log_write(log_path, "\nPREPARE OK\n")
        except Exception as exc:
            fail(exc)


def _copy_safe(source, destination):
    source = Path(source)
    destination = Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    if source.exists():
        shutil.copy2(source, destination)


def _run_git_before(repo, log_path):
    from ..updates.versions import _git_version

    version = _git_version(repo)
    log_write(log_path, "$ git version: %s\n" % version)
    return version


def _stash(repo, preserve_id, log_path):
    rc, out = run_cmd(["git", "status", "--porcelain"], cwd=repo, log_path=log_path)
    if rc != 0:
        raise RuntimeError("git status failed during prepare")
    if not (out or "").strip():
        return None
    rc, out = run_cmd(
        ["git", "stash", "push", "-u", "-m", "storage-control preserve %s" % preserve_id],
        cwd=repo, log_path=log_path, timeout=1800)
    if rc != 0:
        raise RuntimeError("git stash failed during prepare")
    rc, out = run_cmd(["git", "stash", "list", "--format=%H", "-n", "1"], cwd=repo, log_path=log_path)
    if rc != 0:
        return None
    stash_hash = (out or "").strip().splitlines()[0].strip() if out.strip() else None
    if stash_hash:
        run_cmd(["git", "update-ref", "refs/preserve/%s" % preserve_id, stash_hash], cwd=repo, log_path=log_path)
    return stash_hash


def restore_preserve(preserve_id):
    from wsgi import app
    with app.app_context():
        preserve = UpdatePreserve.query.get(preserve_id)
        if not preserve or preserve.status not in ("ready", "failed", "restored"):
            return
        log_path = Path(preserve.log_path)
        preserve.status = "restoring"
        db.session.commit()
        steps = []

        def finish_ok():
            preserve.status = "restored"
            preserve.restored_at = datetime.utcnow()
            preserve.steps = json.dumps(steps)
            db.session.commit()
            log_write(log_path, "\nRESTORE OK\n")

        def fail(exc):
            preserve.status = "failed"
            preserve.error = str(exc)
            preserve.steps = json.dumps(steps)
            db.session.commit()
            log_write(log_path, "\nFAILED: %s\n" % exc)

        try:
            base = Path(preserve.storage_dir)
            if preserve.target == "nextcloud":
                repo = NEXTCLOUD_ROOT
                rc, _ = _step(steps, preserve, "maintenance_on", lambda: run_cmd(
                    ["php", "occ", "maintenance:mode", "--on"], cwd=NEXTCLOUD_ROOT, log_path=log_path))
                if rc != 0:
                    raise RuntimeError("cannot enable maintenance mode")
                _copy_safe(base / "config.php", NEXTCLOUD_ROOT / "config" / "config.php")
                _copy_safe(base / ".htaccess", NEXTCLOUD_ROOT / ".htaccess")
                rc, _ = _step(steps, preserve, "git_checkout", lambda: run_cmd(
                    ["git", "checkout", preserve.git_before or "-"], cwd=repo, log_path=log_path))
                if rc != 0:
                    raise RuntimeError("git checkout failed")
                if preserve.git_stash:
                    _step(steps, preserve, "git_stash_apply", lambda: run_cmd(
                        ["git", "stash", "apply", preserve.git_stash], cwd=repo, log_path=log_path))
                rc, _ = _step(steps, preserve, "restore_db", lambda: run_cmd(
                    ["pg_restore", "--dbname=postgresql://nextcloud_db@localhost:5432/nextcloud_db",
                     "--clean", "--if-exists", str(base / "nextcloud_db.dump")], log_path=log_path, timeout=3600))
                if rc != 0:
                    raise RuntimeError("database restore failed")
                rc, _ = _step(steps, preserve, "maintenance_off", lambda: run_cmd(
                    ["php", "occ", "maintenance:mode", "--off"], cwd=NEXTCLOUD_ROOT, log_path=log_path))
                if rc != 0:
                    raise RuntimeError("cannot disable maintenance mode")
                _step(steps, preserve, "restart_services", lambda: run_cmd(
                    ["sudo", "systemctl", "restart", "php8.5-fpm", "nginx"], log_path=log_path))
            elif preserve.target == "stalwart":
                run_cmd(["systemctl", "--user", "stop", "stalwart.service"], log_path=log_path)
                rc, _ = _step(steps, preserve, "restore_data", lambda: run_cmd(
                    ["tar", "xzf", str(base / "stalwart_data.tar.gz"), "-C", "/mnt/data"],
                    log_path=log_path, timeout=3600))
                if rc != 0:
                    raise RuntimeError("stalwart data restore failed")
                run_cmd(["systemctl", "--user", "start", "stalwart.service"], log_path=log_path)
            else:
                repo = IMMICH_SRC
                run_cmd(["systemctl", "--user", "stop", "immich-ml.service", "immich-server.service"], log_path=log_path)
                rc, _ = _step(steps, preserve, "restore_db", lambda: run_cmd(
                    ["pg_restore", "--dbname=postgresql://storage@localhost:5432/immich",
                     "--clean", "--if-exists", str(base / "immich_db.dump")], log_path=log_path, timeout=3600))
                if rc != 0:
                    raise RuntimeError("database restore failed")
                _copy_safe(base / ".env", IMMICH_ENV)
                rc, _ = _step(steps, preserve, "git_checkout", lambda: run_cmd(
                    ["git", "checkout", preserve.git_before or "-"], cwd=repo, log_path=log_path))
                if rc != 0:
                    raise RuntimeError("git checkout failed")
                if preserve.git_stash:
                    _step(steps, preserve, "git_stash_apply", lambda: run_cmd(
                        ["git", "stash", "apply", preserve.git_stash], cwd=repo, log_path=log_path))
                run_cmd(["systemctl", "--user", "start", "immich-ml.service", "immich-server.service"], log_path=log_path)
            finish_ok()
        except Exception as exc:
            fail(exc)


def delete_preserve(preserve_id):
    from wsgi import app
    with app.app_context():
        preserve = UpdatePreserve.query.get(preserve_id)
        if not preserve:
            return
        try:
            shutil.rmtree(Path(preserve.storage_dir), ignore_errors=True)
        except Exception:
            pass
        db.session.delete(preserve)
        db.session.commit()


def list_preserves(target=None):
    from wsgi import app
    with app.app_context():
        q = UpdatePreserve.query
        if target:
            q = q.filter_by(target=target)
        return q.order_by(UpdatePreserve.created_at.desc()).all()

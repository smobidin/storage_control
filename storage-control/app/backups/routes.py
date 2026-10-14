import os, json
from pathlib import Path
from flask import Blueprint, render_template, request, flash, redirect, url_for, send_file, jsonify, current_app
from flask_login import login_required
from flask_wtf.csrf import validate_csrf
from wtforms.validators import ValidationError

from ..extensions import db
from ..models import BackupTask, BackupRun, AuditLog
from ..config import BACKUP_ROOT
from ..runners.backup_task import execute_backup_task
from ..runners.common import async_call
from ..scheduler import sync_backup_jobs

bp = Blueprint("backups", __name__, template_folder="../../templates", url_prefix="/backups")


def _files():
    out = []
    root = Path(BACKUP_ROOT)
    if not root.exists():
        return out
    for p in root.rglob("*"):
        if p.is_file():
            try:
                st = p.stat()
                out.append({"path": str(p), "rel": str(p.relative_to(root)), "name": p.name,
                            "size": st.st_size, "mtime": st.st_mtime})
            except Exception:
                pass
    out.sort(key=lambda x: x["mtime"], reverse=True)
    return out


def safe_path(rel):
    root = Path(BACKUP_ROOT).resolve()
    p = (root / rel).resolve()
    if not str(p).startswith(str(root)):
        return None
    return p


@bp.route("/")
@login_required
def index():
    tasks = BackupTask.query.order_by(BackupTask.id).all()
    runs = BackupRun.query.order_by(BackupRun.started_at.desc()).limit(50).all()
    return render_template("backups/index.html", tasks=tasks, runs=runs, files=_files())


@bp.route("/tasks", methods=["POST"])
@login_required
def create_task():
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("backups.index"))
    t = BackupTask(
        name=request.form["name"],
        kind=request.form["kind"],
        schedule_cron=request.form.get("schedule_cron") or None,
        command=request.form.get("command") or None,
        enabled=request.form.get("enabled") == "on",
        retention_days=int(request.form.get("retention_days") or 14),
    )
    db.session.add(t)
    db.session.commit()
    sync_backup_jobs(current_app)
    db.session.add(AuditLog(actor="admin", action="backup_task.create", obj=t.name))
    db.session.commit()
    flash("Task created", "success")
    return redirect(url_for("backups.index"))


@bp.route("/tasks/<int:tid>/update", methods=["POST"])
@login_required
def update_task(tid):
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("backups.index"))
    t = BackupTask.query.get_or_404(tid)
    t.name = request.form["name"]
    t.kind = request.form["kind"]
    t.schedule_cron = request.form.get("schedule_cron") or None
    t.command = request.form.get("command") or None
    t.enabled = request.form.get("enabled") == "on"
    t.retention_days = int(request.form.get("retention_days") or 14)
    db.session.commit()
    sync_backup_jobs(current_app)
    flash("Task updated", "success")
    return redirect(url_for("backups.index"))


@bp.route("/tasks/<int:tid>/delete", methods=["POST"])
@login_required
def delete_task(tid):
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("backups.index"))
    t = BackupTask.query.get_or_404(tid)
    BackupTask.query.filter_by(id=tid).delete()
    db.session.commit()
    sync_backup_jobs(current_app)
    flash("Task deleted", "success")
    return redirect(url_for("backups.index"))


@bp.route("/tasks/<int:tid>/run", methods=["POST"])
@login_required
def run_task(tid):
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("backups.index"))
    async_call(execute_backup_task, tid)
    flash("Task started", "info")
    return redirect(url_for("backups.index"))


@bp.route("/files/download")
@login_required
def download_file():
    rel = request.args.get("rel", "")
    p = safe_path(rel)
    if not p or not p.is_file():
        flash("File not found", "danger")
        return redirect(url_for("backups.index"))
    return send_file(str(p), as_attachment=True, download_name=p.name)


@bp.route("/files/delete", methods=["POST"])
@login_required
def delete_file():
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("backups.index"))
    rel = request.form.get("rel", "")
    p = safe_path(rel)
    if p and p.is_file():
        p.unlink()
        flash("Deleted", "success")
    else:
        flash("Not found", "danger")
    return redirect(url_for("backups.index"))


@bp.route("/files/verify", methods=["POST"])
@login_required
def verify_file():
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("backups.index"))
    rel = request.form.get("rel", "")
    p = safe_path(rel)
    if not p or not p.is_file():
        flash("Not found", "danger"); return redirect(url_for("backups.index"))
    if p.suffix == ".dump":
        from ..runners.common import run_cmd
        rc, out = run_cmd(["pg_restore", "--list", str(p)], timeout=300)
        flash("Verify OK" if rc == 0 else f"Verify failed: {out[-300:]}", "success" if rc == 0 else "danger")
    else:
        flash("Only .dump supported", "info")
    return redirect(url_for("backups.index"))

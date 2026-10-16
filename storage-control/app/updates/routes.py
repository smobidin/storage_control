from flask import Blueprint, render_template, request, flash, redirect, url_for, jsonify
from flask_login import login_required
from flask_wtf.csrf import validate_csrf
from wtforms.validators import ValidationError
from datetime import datetime

from ..extensions import db
from ..models import UpdateRun, AuditLog, UpdatePreserve
from ..config import RUNS_DIR, BACKUP_ROOT
from ..updates.versions import immich_current, immich_tags, nextcloud_current, nextcloud_tags, stalwart_current, stalwart_tags, add_candidate, immich_release_info, nextcloud_release_info, stalwart_release_info
from ..runners.update_immich import run_immich_update
from ..runners.update_nextcloud import run_nextcloud_update
from ..runners.update_stalwart import run_stalwart_update
from ..runners.preserve import prepare_preserve, restore_preserve, delete_preserve
from ..runners.common import async_call

bp = Blueprint("updates", __name__, template_folder="../../templates", url_prefix="/updates")


@bp.route("/")
@login_required
def index():
    runs = UpdateRun.query.order_by(UpdateRun.started_at.desc()).limit(30).all()
    preserves = UpdatePreserve.query.order_by(UpdatePreserve.created_at.desc()).limit(50).all()
    ic = immich_current()
    it = immich_tags()
    nc = nextcloud_current()
    nt = nextcloud_tags()
    sc = stalwart_current()
    st = stalwart_tags()
    immich_info = immich_release_info(ic)
    nextcloud_info = nextcloud_release_info(nc)
    stalwart_info = stalwart_release_info(sc)
    return render_template("updates/index.html",
                           immich_current=ic,
                           immich_tags=add_candidate(it, immich_info["latest"]),
                           immich_latest=immich_info["latest"],
                           immich_available=immich_info["available"],
                           immich_release_url=immich_info["url"],
                           immich_changelog=immich_info["body"],
                           immich_migration_steps=immich_info["migration_steps"],
                           immich_release_error=immich_info["error"],
                           nextcloud_current=nc,
                           nextcloud_tags=add_candidate(nt, nextcloud_info["latest"]),
                           nextcloud_latest=nextcloud_info["latest"],
                           nextcloud_available=nextcloud_info["available"],
                           nextcloud_release_url=nextcloud_info["url"],
                           nextcloud_changelog=nextcloud_info["body"],
                           nextcloud_migration_steps=nextcloud_info["migration_steps"],
                           nextcloud_release_error=nextcloud_info["error"],
                           stalwart_current=sc,
                           stalwart_tags=add_candidate(st, stalwart_info["latest"]),
                           stalwart_latest=stalwart_info["latest"],
                           stalwart_available=stalwart_info["available"],
                           stalwart_release_url=stalwart_info["url"],
                           stalwart_changelog=stalwart_info["body"],
                           stalwart_migration_steps=stalwart_info["migration_steps"],
                           stalwart_release_error=stalwart_info["error"],
                           preserves=preserves,
                           runs=runs)


@bp.route("/immich/run", methods=["POST"])
@login_required
def run_immich():
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("updates.index"))
    tag = request.form.get("tag") or ""
    run = UpdateRun(target="immich", to_version=tag, log_path=str(RUNS_DIR / f"update_immich_{int(datetime.utcnow().timestamp())}.log"))
    run.from_version = immich_current()
    db.session.add(run); db.session.commit()
    async_call(run_immich_update, run.id, tag)
    flash(f"Immich update started: {tag}", "info")
    return redirect(url_for("updates.index"))


@bp.route("/nextcloud/run", methods=["POST"])
@login_required
def run_nextcloud():
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("updates.index"))
    tag = request.form.get("tag") or ""
    run = UpdateRun(target="nextcloud", to_version=tag, log_path=str(RUNS_DIR / f"update_nextcloud_{int(datetime.utcnow().timestamp())}.log"))
    run.from_version = nextcloud_current()
    db.session.add(run); db.session.commit()
    async_call(run_nextcloud_update, run.id, tag)
    flash(f"Nextcloud update started: {tag}", "info")
    return redirect(url_for("updates.index"))


@bp.route("/stalwart/run", methods=["POST"])
@login_required
def run_stalwart():
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("updates.index"))
    tag = request.form.get("tag") or ""
    if not tag:
        flash("tag required", "danger"); return redirect(url_for("updates.index"))
    run = UpdateRun(target="stalwart", to_version=tag, log_path=str(RUNS_DIR / f"update_stalwart_{int(datetime.utcnow().timestamp())}.log"))
    run.from_version = stalwart_current()
    db.session.add(run); db.session.commit()
    async_call(run_stalwart_update, run.id, tag)
    flash(f"Stalwart update started: {tag}", "info")
    return redirect(url_for("updates.index"))


@bp.route("/api/runs/<int:rid>")
@login_required
def run_status(rid):
    run = UpdateRun.query.get_or_404(rid)
    try:
        log_tail = PathText(run.log_path)
    except Exception:
        log_tail = ""
    import json
    try:
        steps = json.loads(run.steps) if run.steps else []
    except Exception:
        steps = []
    return jsonify({"id": run.id, "status": run.status, "steps": steps, "log_tail": log_tail, "finished_at": str(run.finished_at) if run.finished_at else None})



@bp.route("/preserve/prepare", methods=["POST"])
@login_required
def prepare_preserve_route():
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("updates.index"))
    target = (request.form.get("target") or "").strip()
    if target not in ("immich", "nextcloud", "stalwart"):
        flash("invalid target", "danger"); return redirect(url_for("updates.index"))
    from datetime import datetime
    preserve = UpdatePreserve(target=target, status="pending",
                              log_path=str(RUNS_DIR / ("prepare_%s_%d_%d.log" % (target, int(datetime.utcnow().timestamp()), 0))))
    db.session.add(preserve); db.session.commit()
    preserve.storage_dir = str(BACKUP_ROOT / "preserves" / target / str(preserve.id))
    preserve.log_path = str(RUNS_DIR / ("prepare_%s_%d.log" % (target, preserve.id)))
    db.session.commit()
    async_call(prepare_preserve, preserve.id)
    flash("Prepare started: %s" % target, "info")
    return redirect(url_for("updates.index"))


@bp.route("/preserve/<int:pid>/restore", methods=["POST"])
@login_required
def restore_preserve_route(pid):
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("updates.index"))
    async_call(restore_preserve, pid)
    flash("Restore started for preserve %d" % pid, "info")
    return redirect(url_for("updates.index"))


@bp.route("/preserve/<int:pid>/delete", methods=["POST"])
@login_required
def delete_preserve_route(pid):
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("updates.index"))
    delete_preserve(pid)
    flash("Preserve %d deleted" % pid, "success")
    return redirect(url_for("updates.index"))


@bp.route("/preserve/<int:pid>/log")
@login_required
def preserve_log(pid):
    preserve = UpdatePreserve.query.get_or_404(pid)
    try:
        text = PathText(preserve.log_path)
    except Exception:
        text = ""
    from flask import Response
    return Response(text, mimetype="text/plain")


@bp.route("/preserve/<int:pid>/api")
@login_required
def preserve_status(pid):
    preserve = UpdatePreserve.query.get_or_404(pid)
    try:
        log_tail = PathText(preserve.log_path)
    except Exception:
        log_tail = ""
    import json
    try:
        steps = json.loads(preserve.steps) if preserve.steps else []
    except Exception:
        steps = []
    return jsonify({
        "id": preserve.id,
        "status": preserve.status,
        "steps": steps,
        "log_tail": log_tail,
        "target": preserve.target,
        "finished_at": str(preserve.finished_at) if preserve.finished_at else None,
    })

@bp.route("/api/availability")
@login_required
def api_availability():
    ic = immich_current()
    nc = nextcloud_current()
    sc = stalwart_current()
    i = immich_release_info(ic)
    n = nextcloud_release_info(nc)
    s = stalwart_release_info(sc)
    return jsonify({
        "immich": {
            "current": i.get("current"),
            "latest": i.get("latest"),
            "available": bool(i.get("available")),
            "error": i.get("error"),
            "url": i.get("url"),
        },
        "nextcloud": {
            "current": n.get("current"),
            "latest": n.get("latest"),
            "available": bool(n.get("available")),
            "error": n.get("error"),
            "url": n.get("url"),
        },
        "stalwart": {
            "current": s.get("current"),
            "latest": s.get("latest"),
            "available": bool(s.get("available")),
            "error": s.get("error"),
            "url": s.get("url"),
        },
    })

def PathText(p):
    from pathlib import Path
    return Path(p).read_text(errors="replace")[-4000:] if Path(p).exists() else ""

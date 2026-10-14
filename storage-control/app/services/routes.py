from flask import Blueprint, render_template, request, jsonify, flash, redirect, url_for, Response
from flask_login import login_required
from flask_wtf.csrf import validate_csrf
from wtforms.validators import ValidationError
from ..collectors import services as svc
from ..collectors.services import validate_unit as _validate_unit
from ..models import AuditLog
from ..extensions import db

bp = Blueprint("services", __name__, template_folder="../../templates", url_prefix="/services")


@bp.route("/")
@login_required
def index():
    return render_template("services/index.html", services=svc.all_status())


@bp.route("/manage", methods=["POST"])
@login_required
def manage():
    unit = request.form.get("unit", "")
    scope = request.form.get("scope") or None
    action = request.form.get("action", "")
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        return jsonify({"ok": False, "msg": "csrf"}), 400
    try:
        ok, msg = svc.manage_one(unit, scope, action)
    except ValueError as e:
        return jsonify({"ok": False, "msg": str(e)}), 400
    db.session.add(AuditLog(actor="admin", action=f"service.{action}", obj=unit, payload=msg))
    db.session.commit()
    if request.headers.get("X-Requested-With") == "XMLHttpRequest":
        return jsonify({"ok": ok, "msg": msg})
    flash(f"{unit}: {msg}", "success" if ok else "danger")
    return redirect(url_for("services.index"))


@bp.route("/logs")
@login_required
def logs():
    unit = request.args.get("unit", "")
    scope = request.args.get("scope", "system")
    try:
        _validate_unit(unit)
    except ValueError:
        return Response("invalid unit", status=400, mimetype="text/plain")
    if scope == "user":
        cmd = ["journalctl", "--user", "-u", unit, "-n", "300", "--no-pager"]
    else:
        cmd = ["sudo", "journalctl", "-u", unit, "-n", "300", "--no-pager"]
    import subprocess
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    return Response(r.stdout + r.stderr, mimetype="text/plain")


@bp.route("/group", methods=["POST"])
@login_required
def group():
    group = request.form.get("group", "")
    action = request.form.get("action", "")
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        return jsonify({"ok": False, "msg": "csrf"}), 400
    results = svc.group_action(group, action)
    db.session.add(AuditLog(actor="admin", action=f"group.{action}", obj=group, payload=str(results)))
    db.session.commit()
    if request.headers.get("X-Requested-With") == "XMLHttpRequest":
        return jsonify({"ok": all(r["ok"] for r in results), "results": results})
    flash(str(results), "info")
    return redirect(url_for("services.index"))

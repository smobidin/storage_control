from flask import Blueprint, render_template, jsonify, request
from flask_login import login_required
from ..collectors.system import system_summary
from ..collectors.raid import raid_summary
from ..collectors import services as svc
from ..collectors.smart import smart_summary

bp = Blueprint("dashboard", __name__, template_folder="../../templates", url_prefix="")


from pathlib import Path
import markdown


@bp.route("/")
@login_required
def index():
    return render_template("dashboard/index.html")

@bp.route("/help")
@login_required
def help_page():
    readme_path = Path(__file__).resolve().parents[2] / "README.md"
    text = readme_path.read_text(encoding="utf-8")
    html = markdown.markdown(text, extensions=["tables", "fenced_code", "toc"])
    return render_template("help.html", readme_html=html)


@bp.route("/api/health/summary")
@login_required
def api_summary():
    data = system_summary()
    data["raid"] = raid_summary()
    return jsonify(data)


@bp.route("/api/health/smart")
@login_required
def api_smart():
    return jsonify(smart_summary())

@bp.route("/api/health/services")
@login_required
def api_services():
    return jsonify(svc.all_status())


@bp.route("/api/health/smart/status")
@login_required
def api_smart_status():
    from ..models import SmartDiskState, SmartAlert
    from ..extensions import db
    states = SmartDiskState.query.order_by(SmartDiskState.device).all()
    alerts = SmartAlert.query.filter_by(active=True).order_by(SmartAlert.created_at.desc()).all()
    return jsonify({
        "states": [{
            "device": s.device,
            "model": s.model,
            "serial": s.serial,
            "health": s.health,
            "reallocated": s.reallocated,
            "pending": s.pending,
            "offline_uncorrectable": s.offline_uncorrectable,
            "ata_error_count": s.ata_error_count,
            "temperature": s.temperature,
            "power_on_hours": s.power_on_hours,
            "first_seen": s.first_seen_at.isoformat() if s.first_seen_at else None,
            "last_checked": s.last_checked_at.isoformat() if s.last_checked_at else None,
        } for s in states],
        "alerts": [{
            "device": a.device,
            "severity": a.severity,
            "message": a.message,
            "created_at": a.created_at.isoformat() if a.created_at else None,
        } for a in alerts],
    })

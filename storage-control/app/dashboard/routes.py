from flask import Blueprint, render_template, jsonify, request
from flask_login import login_required
from ..collectors.system import system_summary
from ..collectors.raid import raid_summary
from ..collectors import services as svc
from ..collectors.smart import smart_summary
from ..extensions import csrf
import json

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
    import json
    states = SmartDiskState.query.order_by(SmartDiskState.device).all()
    alerts = SmartAlert.query.filter_by(active=True).order_by(SmartAlert.created_at.desc()).all()

    def parse_baseline(state):
        if state and state.resolved_counters:
            try:
                return json.loads(state.resolved_counters)
            except (json.JSONDecodeError, TypeError):
                return None
        return None

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
            "baseline": parse_baseline(s),
        } for s in states],
        "alerts": [{
            "id": a.id,
            "device": a.device,
            "severity": a.severity,
            "message": a.message,
            "created_at": a.created_at.isoformat() if a.created_at else None,
        } for a in alerts],
    })


@bp.route("/api/health/smart/alert/<int:alert_id>/resolve", methods=["POST"])
@login_required
@csrf.exempt
def api_smart_alert_resolve(alert_id):
    from ..models import SmartAlert, SmartDiskState
    from ..extensions import db
    from datetime import datetime, timezone

    alert = SmartAlert.query.get_or_404(alert_id)
    alert.active = False
    alert.resolved_at = datetime.now(timezone.utc)

    # Save current counters as baseline for this device
    state = SmartDiskState.query.filter_by(device=alert.device).first()
    if state:
        _save_resolved_counters(state, state.reallocated, state.pending,
                                state.offline_uncorrectable, state.ata_error_count)

    db.session.commit()
    return jsonify({"ok": True, "resolved_at": alert.resolved_at.isoformat()})


def _save_resolved_counters(state, reallocated, pending, offline, ata_errors):
    """Save current counters as resolved baseline."""
    state.resolved_counters = json.dumps({
        "reallocated": reallocated,
        "pending": pending,
        "offline": offline,
        "ata_errors": ata_errors,
    })


@bp.route("/api/health/smart/refresh", methods=["POST"])
@login_required
@csrf.exempt
def api_smart_refresh():
    """Manual SMART refresh - runs same check logic as nightly scan."""
    from ..runners.smart_monitor import run_manual_smart_check
    results = run_manual_smart_check()
    return jsonify({"ok": True, "results": results})


@bp.route("/api/health/smart/history/<device>")
@login_required
def api_smart_history(device):
    """Get historical counter data for a device."""
    from ..models import SmartDiskCounterHistory
    history = SmartDiskCounterHistory.query.filter_by(device=device)\
        .order_by(SmartDiskCounterHistory.checked_at.desc()).limit(100).all()
    return jsonify([{
        "checked_at": h.checked_at.isoformat() if h.checked_at else None,
        "reallocated": h.reallocated,
        "pending": h.pending,
        "offline_uncorrectable": h.offline_uncorrectable,
        "ata_error_count": h.ata_error_count,
        "temperature": h.temperature,
        "health": h.health,
        "smartctl_rc": h.smartctl_rc,
        "source": h.source,
    } for h in history])
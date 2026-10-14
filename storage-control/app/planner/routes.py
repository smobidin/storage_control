import subprocess, re
from pathlib import Path
from flask import Blueprint, render_template, request, flash, redirect, url_for
from flask_login import login_required
from flask_wtf.csrf import validate_csrf
from wtforms.validators import ValidationError

from ..extensions import db
from ..models import ScheduledJob, AuditLog
from ..collectors import services as svcs
from ..scheduler import get_scheduler
from ..runners.common import async_call

bp = Blueprint("scheduler", __name__, template_folder="../../templates", url_prefix="/scheduler")


def parse_system_timers():
    out = []
    # user timers
    for scope in ("user", "system"):
        cmd = (["systemctl", "--user", "list-timers", "--all", "--no-pager"] if scope == "user"
               else ["sudo", "systemctl", "list-timers", "--all", "--no-pager"])
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=20)
            text = r.stdout
        except Exception as e:
            text = str(e)
        # Parse lines: NEXT LEFT LAST PASSED UNIT ACTIVATES
        for line in text.splitlines():
            m = re.match(r"^\s*\S+\s+\S+\s+\S+\s+\S+\s+(\S+)\s+(\S+)\s*$", line)
            # fallback simpler: lines containing .timer
            if ".timer" in line:
                parts = line.split()
                unit = next((p for p in parts if p.endswith(".timer")), None)
                if unit:
                    out.append({"scope": scope, "unit": unit, "raw": line.strip()})
    return out


@bp.route("/")
@login_required
def index():
    jobs = ScheduledJob.query.order_by(ScheduledJob.id).all()
    timers = parse_system_timers()
    sched = get_scheduler()
    aps_jobs = []
    if sched:
        for j in sched.get_jobs():
            aps_jobs.append({"id": j.id, "next": str(j.next_run_time), "trigger": str(j.trigger)})
    return render_template("scheduler/index.html", jobs=jobs, timers=timers, aps_jobs=aps_jobs)


@bp.route("/jobs", methods=["POST"])
@login_required
def create_job():
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("scheduler.index"))
    j = ScheduledJob(
        name=request.form["name"],
        job_type="custom_shell",
        target=None,
        schedule_cron=request.form.get("schedule_cron") or None,
        enabled=request.form.get("enabled") == "on",
        notes=request.form.get("notes"),
    )
    cmd = request.form.get("command") or ""
    db.session.add(j)
    db.session.commit()

    if j.schedule_cron and j.enabled:
        from apscheduler.triggers.cron import CronTrigger
        sched = get_scheduler()
        if sched:
            def runner(jid=j.id, command=cmd):
                from wsgi import app
                with app.app_context():
                    import subprocess
                    from ..models import ScheduledJob
                    from .common import run_cmd
                    from pathlib import Path
                    log_path = app.config["RUNS_DIR"] / f"sched_{jid}.log"
                    rc, _ = run_cmd(["bash", "-lc", command], log_path=log_path, timeout=3600)
                    sj = ScheduledJob.query.get(jid)
                    if sj:
                        sj.last_run_at = __import__("datetime").datetime.utcnow()
                        sj.last_status = "success" if rc == 0 else "failed"
                        db.session.commit()
            sched.add_job(runner, CronTrigger.from_crontab(j.schedule_cron), id=f"sched_job_{j.id}", replace_existing=True)

    db.session.add(AuditLog(actor="admin", action="scheduled_job.create", obj=j.name))
    db.session.commit()
    flash("Job created", "success")
    return redirect(url_for("scheduler.index"))


@bp.route("/jobs/<int:jid>/run", methods=["POST"])
@login_required
def run_job(jid):
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("scheduler.index"))
    j = ScheduledJob.query.get_or_404(jid)
    flash("Use custom command form to run ad-hoc; scheduled shell runner logged", "info")
    return redirect(url_for("scheduler.index"))


@bp.route("/jobs/<int:jid>/delete", methods=["POST"])
@login_required
def delete_job(jid):
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("scheduler.index"))
    j = ScheduledJob.query.get_or_404(jid)
    sched = get_scheduler()
    if sched:
        try: sched.remove_job(f"sched_job_{jid}")
        except Exception: pass
    ScheduledJob.query.filter_by(id=jid).delete()
    db.session.commit()
    flash("Deleted", "success")
    return redirect(url_for("scheduler.index"))


@bp.route("/timer/toggle", methods=["POST"])
@login_required
def toggle_timer():
    try:
        validate_csrf(request.form.get("csrf_token"))
    except ValidationError:
        flash("csrf", "danger"); return redirect(url_for("scheduler.index"))
    unit = request.form["unit"]
    scope = request.form["scope"]
    action = request.form["action"]  # enable/disable/start/stop
    ok, msg = svcs.manage_one(unit, scope, action)
    flash(f"{unit}: {msg}", "success" if ok else "danger")
    return redirect(url_for("scheduler.index"))

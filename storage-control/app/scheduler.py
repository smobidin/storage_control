from apscheduler.schedulers.background import BackgroundScheduler
from apscheduler.jobstores.sqlalchemy import SQLAlchemyJobStore
from apscheduler.triggers.cron import CronTrigger
from flask import current_app

_SCHED = {"sched": None}


def get_scheduler():
    return _SCHED["sched"]


def init_scheduler(app):
    if app.config.get("ENABLE_SCHEDULER", True) is False:
        return None
    jobstores = {"default": SQLAlchemyJobStore(url=app.config["SQLALCHEMY_DATABASE_URI"])}
    sched = BackgroundScheduler(jobstores=jobstores, timezone="UTC")
    sched.start()
    _SCHED["sched"] = sched

    # internal retention cleanup daily 02:30
    from .runners.backup_task import run_retention_cleanup
    sched.add_job(run_retention_cleanup, CronTrigger(hour=2, minute=30), id="retention_cleanup", replace_existing=True)

    from .runners.smart_monitor import run_smart_check
    sched.add_job(run_smart_check, CronTrigger(hour=1, minute=0), id="smart_nightly_check", replace_existing=True)

    sync_backup_jobs(app, sched)
    app.extensions = getattr(app, "extensions", {})
    app.extensions["scheduler"] = sched
    return sched


def sync_backup_jobs(app, sched=None):
    sched = sched or get_scheduler()
    if not sched:
        return
    from .extensions import db
    from .models import BackupTask
    with app.app_context():
        for task in BackupTask.query.filter(BackupTask.enabled.is_(True)).all():
            job_id = f"backup_task_{task.id}"
            if task.schedule_cron:
                try:
                    trigger = CronTrigger.from_crontab(task.schedule_cron)
                except Exception:
                    continue
                sched.add_job(lambda tid=task.id: __import__("app.runners.backup_task", fromlist=["execute_backup_task"]).execute_backup_task(tid),
                              trigger, id=job_id, replace_existing=True)
            else:
                # remove if exists
                try:
                    sched.remove_job(job_id)
                except Exception:
                    pass
        # custom scheduled jobs handled separately in scheduler_setup maybe; keep simple

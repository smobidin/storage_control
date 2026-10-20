from datetime import datetime, timezone
from flask_login import UserMixin
from .extensions import db


class User(UserMixin, db.Model):
    id = db.Column(db.Integer, primary_key=True)
    username = db.Column(db.String(80), unique=True, nullable=False)
    password_hash = db.Column(db.String(255), nullable=False)
    created_at = db.Column(db.DateTime, default=datetime.utcnow)


class AuditLog(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    actor = db.Column(db.String(80))
    action = db.Column(db.String(120), nullable=False)
    obj = db.Column(db.String(255))
    payload = db.Column(db.Text)
    ip = db.Column(db.String(64))
    ts = db.Column(db.DateTime, default=datetime.utcnow, index=True)


class UpdateRun(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    target = db.Column(db.String(32), nullable=False)  # immich|nextcloud
    from_version = db.Column(db.String(64))
    to_version = db.Column(db.String(64))
    status = db.Column(db.String(16), default="pending")  # pending/running/success/failed
    started_at = db.Column(db.DateTime, default=datetime.utcnow)
    finished_at = db.Column(db.DateTime)
    log_path = db.Column(db.String(512))
    backup_file = db.Column(db.String(512))
    steps = db.Column(db.Text)  # JSON list


class BackupTask(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    name = db.Column(db.String(120), nullable=False)
    kind = db.Column(db.String(48), nullable=False)
    schedule_cron = db.Column(db.String(120))  # null => manual
    command = db.Column(db.Text)
    enabled = db.Column(db.Boolean, default=True)
    retention_days = db.Column(db.Integer, default=14)
    last_run_at = db.Column(db.DateTime)
    last_status = db.Column(db.String(16))
    created_at = db.Column(db.DateTime, default=datetime.utcnow)
    updated_at = db.Column(db.DateTime, default=datetime.utcnow, onupdate=datetime.utcnow)


class BackupRun(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    task_id = db.Column(db.Integer, db.ForeignKey("backup_task.id"))
    status = db.Column(db.String(16), default="running")
    started_at = db.Column(db.DateTime, default=datetime.utcnow)
    finished_at = db.Column(db.DateTime)
    outdir = db.Column(db.String(512))
    files = db.Column(db.Text)  # JSON
    log = db.Column(db.Text)
    exit_code = db.Column(db.Integer)


class ScheduledJob(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    name = db.Column(db.String(120), nullable=False)
    job_type = db.Column(db.String(32), nullable=False)  # systemd_timer|systemd_service|custom_shell|internal
    target = db.Column(db.String(255))
    schedule_cron = db.Column(db.String(120))
    enabled = db.Column(db.Boolean, default=True)
    notes = db.Column(db.Text)
    last_run_at = db.Column(db.DateTime)
    last_status = db.Column(db.String(16))
    created_at = db.Column(db.DateTime, default=datetime.utcnow)

class UpdatePreserve(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    target = db.Column(db.String(32), nullable=False)
    from_version = db.Column(db.String(64))
    status = db.Column(db.String(24), default="pending")
    storage_dir = db.Column(db.String(512))
    db_dump = db.Column(db.String(512))
    config_path = db.Column(db.String(512))
    extra_files = db.Column(db.Text)
    git_before = db.Column(db.String(128))
    git_stash = db.Column(db.String(64))
    log_path = db.Column(db.String(512))
    error = db.Column(db.Text)
    steps = db.Column(db.Text)
    created_at = db.Column(db.DateTime, default=datetime.utcnow)
    finished_at = db.Column(db.DateTime)
    restored_at = db.Column(db.DateTime)
    note = db.Column(db.Text)


class SmartDiskState(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    device = db.Column(db.String(32), unique=True, nullable=False, index=True)
    model = db.Column(db.String(160))
    serial = db.Column(db.String(80))
    health = db.Column(db.String(32))
    reallocated = db.Column(db.Integer, default=0)
    pending = db.Column(db.Integer, default=0)
    offline_uncorrectable = db.Column(db.Integer, default=0)
    ata_error_count = db.Column(db.Integer, default=0)
    temperature = db.Column(db.Integer)
    power_on_hours = db.Column(db.Integer)
    smartctl_rc = db.Column(db.Integer)
    first_seen_at = db.Column(db.DateTime, default=lambda: datetime.now(timezone.utc))
    last_checked_at = db.Column(db.DateTime, default=lambda: datetime.now(timezone.utc))
    raw_summary = db.Column(db.Text)
    # Baseline counters at last user resolution (JSON: reallocated, pending, offline, ata_errors)
    resolved_counters = db.Column(db.Text)  # JSON string


class SmartDiskCounterHistory(db.Model):
    """Historical record of SMART counters for trend analysis."""
    id = db.Column(db.Integer, primary_key=True)
    device = db.Column(db.String(32), nullable=False, index=True)
    reallocated = db.Column(db.Integer, default=0)
    pending = db.Column(db.Integer, default=0)
    offline_uncorrectable = db.Column(db.Integer, default=0)
    ata_error_count = db.Column(db.Integer, default=0)
    temperature = db.Column(db.Integer)
    health = db.Column(db.String(32))
    smartctl_rc = db.Column(db.Integer)
    checked_at = db.Column(db.DateTime, default=lambda: datetime.now(timezone.utc), index=True)
    source = db.Column(db.String(16), default="auto")  # auto|manual


class SmartAlert(db.Model):
    id = db.Column(db.Integer, primary_key=True)
    device = db.Column(db.String(32), nullable=False, index=True)
    severity = db.Column(db.String(16), default="warning")  # warning|critical
    message = db.Column(db.Text, nullable=False)
    active = db.Column(db.Boolean, default=True, index=True)
    created_at = db.Column(db.DateTime, default=lambda: datetime.now(timezone.utc))
    resolved_at = db.Column(db.DateTime)
    # Counters at the time this alert was created (for comparison)
    alert_counters = db.Column(db.Text)  # JSON string

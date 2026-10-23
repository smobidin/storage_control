-- New schema for the Rust rewrite (doc/rust-port.md §6).
--
-- Differences from the SQLAlchemy schema it replaces:
--   * all timestamps are INTEGER unix milliseconds (UTC), never naive TEXT;
--   * statuses are CHECKed, booleans are INTEGER with a CHECK;
--   * backup_run.task_id has a real foreign key;
--   * alembic_version / apscheduler_jobs are gone (no Alembic, scheduler is in-process).
--
-- JSON-ish columns (steps, files, extra_files, resolved_counters,
-- alert_counters) stay TEXT: they are opaque blobs for the UI, validated on read.

CREATE TABLE IF NOT EXISTS schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS audit (
    id      INTEGER PRIMARY KEY,
    actor   TEXT,
    action  TEXT NOT NULL,
    object  TEXT,
    payload TEXT,
    ip      TEXT,
    ts      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit(ts);

CREATE TABLE IF NOT EXISTS update_run (
    id           INTEGER PRIMARY KEY,
    target       TEXT NOT NULL CHECK (target IN ('immich', 'nextcloud', 'stalwart')),
    from_version TEXT,
    to_version   TEXT,
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'running', 'success', 'failed')),
    started_at   INTEGER NOT NULL,
    finished_at  INTEGER,
    log_path     TEXT,
    backup_file  TEXT,
    steps        TEXT
);
CREATE INDEX IF NOT EXISTS idx_update_run_target ON update_run(target, status);

CREATE TABLE IF NOT EXISTS backup_task (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN (
                       'immich_db', 'nextcloud_db', 'immich_config', 'nextcloud_config',
                       'stalwart_db', 'stalwart_config', 'all', 'custom')),
    cron           TEXT,
    command        TEXT,
    enabled        INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    retention_days INTEGER NOT NULL DEFAULT 14,
    last_run_at    INTEGER,
    last_status    TEXT,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS backup_run (
    id          INTEGER PRIMARY KEY,
    task_id     INTEGER REFERENCES backup_task(id) ON DELETE SET NULL,
    status      TEXT NOT NULL DEFAULT 'running'
                CHECK (status IN ('running', 'success', 'failed')),
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    outdir      TEXT,
    files       TEXT,
    log         TEXT,
    exit_code   INTEGER
);
CREATE INDEX IF NOT EXISTS idx_backup_run_started ON backup_run(started_at);

CREATE TABLE IF NOT EXISTS scheduled_job (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    job_type    TEXT NOT NULL,
    target      TEXT,
    cron        TEXT,
    command     TEXT,
    enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    notes       TEXT,
    last_run_at INTEGER,
    last_status TEXT,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS preserve (
    id           INTEGER PRIMARY KEY,
    target       TEXT NOT NULL CHECK (target IN ('immich', 'nextcloud', 'stalwart')),
    from_version TEXT,
    status       TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
                     'pending', 'preparing', 'ready', 'restoring', 'restored', 'failed')),
    storage_dir  TEXT,
    db_dump      TEXT,
    config_path  TEXT,
    extra_files  TEXT,
    git_before   TEXT,
    git_stash    TEXT,
    log_path     TEXT,
    error        TEXT,
    steps        TEXT,
    created_at   INTEGER NOT NULL,
    finished_at  INTEGER,
    restored_at  INTEGER,
    note         TEXT
);

CREATE TABLE IF NOT EXISTS smart_disk (
    id                  INTEGER PRIMARY KEY,
    device              TEXT NOT NULL UNIQUE,
    model               TEXT,
    serial              TEXT,
    health              TEXT,
    reallocated         INTEGER NOT NULL DEFAULT 0,
    pending             INTEGER NOT NULL DEFAULT 0,
    offline_uncorrectable INTEGER NOT NULL DEFAULT 0,
    ata_error_count     INTEGER NOT NULL DEFAULT 0,
    temperature         INTEGER,
    power_on_hours      INTEGER,
    smartctl_rc         INTEGER,
    resolved_counters   TEXT,
    raw_summary         TEXT,
    first_seen_at       INTEGER NOT NULL,
    last_checked_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS smart_sample (
    id                  INTEGER PRIMARY KEY,
    device              TEXT NOT NULL,
    reallocated         INTEGER NOT NULL DEFAULT 0,
    pending             INTEGER NOT NULL DEFAULT 0,
    offline_uncorrectable INTEGER NOT NULL DEFAULT 0,
    ata_error_count     INTEGER NOT NULL DEFAULT 0,
    temperature         INTEGER,
    health              TEXT,
    smartctl_rc         INTEGER,
    checked_at          INTEGER NOT NULL,
    source              TEXT NOT NULL DEFAULT 'auto' CHECK (source IN ('auto', 'manual'))
);
CREATE INDEX IF NOT EXISTS idx_smart_sample_device ON smart_sample(device, checked_at);

CREATE TABLE IF NOT EXISTS smart_alert (
    id             INTEGER PRIMARY KEY,
    device         TEXT NOT NULL,
    severity       TEXT NOT NULL DEFAULT 'warning' CHECK (severity IN ('warning', 'critical')),
    message        TEXT NOT NULL,
    active         INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at     INTEGER NOT NULL,
    resolved_at    INTEGER,
    alert_counters TEXT
);
CREATE INDEX IF NOT EXISTS idx_smart_alert_active ON smart_alert(active, device);

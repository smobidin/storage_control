import re
import subprocess
from ..config import Config


UNIT_RE = re.compile(r"^[A-Za-z0-9_.@-]+\.(service|timer|socket|target)$")

SERVICE_CATALOG = [
    # Immich stack (user)
    {"name": "immich-ml.service", "scope": "user", "group": "immich", "order": 10, "critical": False, "desc": "ML inference"},
    {"name": "immich-server.service", "scope": "user", "group": "immich", "order": 20, "critical": False, "desc": "API + web (port 2283)"},
    {"name": "immich-nightfix.service", "scope": "user", "group": "immich", "order": 30, "critical": False, "desc": "Night thumbnail fix (manual run)"},
    {"name": "immich-nightfix.timer", "scope": "user", "group": "immich", "order": 31, "critical": False, "desc": "Timer 03:00 daily"},
    {"name": "backup-immich-nc.timer", "scope": "user", "group": "backup", "order": 10, "critical": False, "desc": "DB/config backup 04:00"},
    # Infra (system)
    {"name": "postgresql.service", "scope": "system", "group": "infra", "order": 10, "critical": True, "desc": "PostgreSQL 18"},
    {"name": "redis-server.service", "scope": "system", "group": "infra", "order": 20, "critical": True, "desc": "Redis (Nextcloud sessions)"},
    {"name": "php8.5-fpm.service", "scope": "system", "group": "nextcloud", "order": 30, "critical": True, "desc": "PHP-FPM Nextcloud pool"},
    {"name": "nginx.service", "scope": "system", "group": "infra", "order": 40, "critical": True, "desc": "Reverse proxy 8080/8081"},
    # Timers (system)
    {"name": "certbot-renew.timer", "scope": "system", "group": "infra", "order": 50, "critical": False, "desc": "LetsEncrypt renew"},
    {"name": "mdcheck_start.timer", "scope": "system", "group": "infra", "order": 51, "critical": False, "desc": "RAID check monthly"},
]


def _ctl(scope, *args, timeout=30):
    if scope == "user":
        cmd = ["systemctl", "--user"] + list(args)
    else:
        cmd = ["sudo", "systemctl"] + list(args)
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)


def service_status(entry):
    name = entry["name"]; scope = entry["scope"]
    active = _ctl(scope, "is-active", name).stdout.strip()
    enabled = _ctl(scope, "is-enabled", name).stdout.strip()
    # show details
    show = _ctl(scope, "show", name, "-p", "ActiveEnterTimestamp", "-p", "MemoryCurrent", "-p", "ExecMainStatus", "-p", "UnitFileState")
    detail = {}
    for line in show.stdout.splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            detail[k] = v
    return {
        **entry,
        "active": active,
        "enabled": enabled,
        "active_since": detail.get("ActiveEnterTimestamp"),
        "memory_current": detail.get("MemoryCurrent"),
        "exec_status": detail.get("ExecMainStatus"),
    }


def all_status():
    return [service_status(e) for e in SERVICE_CATALOG]


def validate_unit(name):
    if not UNIT_RE.match(name):
        raise ValueError("invalid unit name")


def manage_one(name, scope, action):
    if action not in ("start", "stop", "restart", "enable", "disable"):
        raise ValueError("invalid action")
    # find scope if not provided
    if scope is None:
        for e in SERVICE_CATALOG:
            if e["name"] == name:
                scope = e["scope"]
                break
        if scope is None:
            scope = "system"
    validate_unit(name)
    if action in ("enable", "disable"):
        # enable/disable may need daemon-reload? no
        r = _ctl(scope, action, name)
    else:
        r = _ctl(scope, action, name)
    return r.returncode == 0, (r.stdout + r.stderr).strip()


def group_action(group, action):
    entries = [e for e in SERVICE_CATALOG if e["group"] == group]
    if action == "start":
        ordered = sorted(entries, key=lambda e: e["order"])
    else:
        ordered = sorted(entries, key=lambda e: e["order"], reverse=True)
    results = []
    for e in ordered:
        ok, msg = manage_one(e["name"], e["scope"], action)
        results.append({"name": e["name"], "ok": ok, "msg": msg})
        if not ok and action in ("start",):
            # continue but record; caller decides
            pass
    return results

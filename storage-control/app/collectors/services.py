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
    # Stalwart (user)
    {"name": "stalwart.service", "scope": "user", "group": "stalwart", "order": 10, "critical": False, "desc": "Stalwart mail server (port 8082)"},
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
    # show details - get more properties for oneshot/timer services
    show = _ctl(scope, "show", name, "-p", "ActiveEnterTimestamp", "-p", "ActiveExitTimestamp", 
                "-p", "ExecMainStatus", "-p", "ExecMainCode", "-p", "UnitFileState", 
                "-p", "Type", "-p", "Result", "-p", "NextEligibleTimestamp")
    detail = {}
    for line in show.stdout.splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            detail[k] = v
    
    # For timer-triggered services, also check the timer's LastTriggerUSec
    last_trigger = None
    if name.endswith(".service"):
        timer_name = name.replace(".service", ".timer")
        timer_show = _ctl(scope, "show", timer_name, "-p", "LastTriggerUSec")
        for line in timer_show.stdout.splitlines():
            if "=" in line:
                k, v = line.split("=", 1)
                if k == "LastTriggerUSec" and v:
                    # Parse format like "Tue 2026-10-20 03:36:44 MSK" to ISO-like for template
                    import re
                    m = re.match(r'(\w{3})\s+(\d{4}-\d{2}-\d{2})\s+(\d{2}:\d{2}:\d{2})\s+\w+', v)
                    if m:
                        last_trigger = f"{m.group(2)}T{m.group(3)}"
                    else:
                        last_trigger = v
                    break
    
    # For oneshot services, active becomes "inactive" after completion.
    # Use Result and ExecMainStatus to determine actual outcome.
    svc_type = detail.get("Type", "")
    result = detail.get("Result", "")
    exec_status = detail.get("ExecMainStatus", "")
    exec_code = detail.get("ExecMainCode", "")
    
    # Determine display status
    display_active = active
    display_status = active
    if svc_type == "oneshot" and active == "inactive":
        # Check if it ran and what the result was
        if result == "success" or exec_status == "0":
            display_status = "success"
        elif result == "failure" or (exec_status and exec_status != "0"):
            display_status = "failed"
        elif active == "inactive" and not detail.get("ActiveEnterTimestamp"):
            display_status = "never-run"
    
    # For oneshot services, use ActiveEnterTimestamp as last run time if ActiveExitTimestamp not available
    # Fall back to timer's LastTriggerUSec for timer-triggered services
    last_run = detail.get("ActiveExitTimestamp") or detail.get("ActiveEnterTimestamp") or last_trigger
    
    return {
        **entry,
        "active": active,
        "enabled": enabled,
        "display_status": display_status,
        "active_since": detail.get("ActiveEnterTimestamp"),
        "active_exit": detail.get("ActiveExitTimestamp"),
        "last_run": last_run,
        "memory_current": detail.get("MemoryCurrent"),
        "exec_status": exec_status,
        "exec_code": exec_code,
        "unit_type": svc_type,
        "result": result,
        "next_run": detail.get("NextEligibleTimestamp"),
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

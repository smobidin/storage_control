import os
import subprocess
import platform
import time
from pathlib import Path
import psutil

from ..config import Config


def system_summary():
    temps = {}
    try:
        raw = psutil.sensors_temperatures(fahrenheit=False)
        for name, entries in raw.items():
            temps[name] = [{"label": e.label or name, "current": e.current, "high": e.high, "critical": e.critical} for e in entries]
    except Exception:
        temps = {}
    # fallback to sensors CLI if empty
    if not temps:
        try:
            out = subprocess.run(["sensors", "-j"], capture_output=True, text=True, timeout=5)
            if out.returncode == 0:
                import json
                data = json.loads(out.stdout)
                for chip, vals in data.items():
                    temps[chip] = vals
        except Exception:
            pass

    disks = []
    for p in psutil.disk_partitions(all=False):
        if p.device.startswith("/dev/loop"):
            continue
        if p.fstype in ("squashfs", "tmpfs", "devtmpfs", "overlay", "proc", "sysfs", "devpts"):
            continue
        if not p.device.startswith("/dev/"):
            continue
        try:
            u = psutil.disk_usage(p.mountpoint)
            disks.append({
                "device": p.device,
                "mountpoint": p.mountpoint,
                "fstype": p.fstype,
                "total": u.total,
                "used": u.used,
                "free": u.free,
                "percent": u.percent,
            })
        except PermissionError:
            continue

    vm = psutil.virtual_memory()
    sm = psutil.swap_memory()
    load1, load5, load15 = os.getloadavg()
    freq = psutil.cpu_freq()
    return {
        "hostname": platform.node(),
        "os": platform.platform(),
        "uptime_seconds": int(time.time() - psutil.boot_time()),
        "cpu": {
            "percent_total": psutil.cpu_percent(interval=None),
            "percent_per_core": psutil.cpu_percent(interval=None, percpu=True),
            "count_logical": psutil.cpu_count(logical=True),
            "count_physical": psutil.cpu_count(logical=False),
            "freq_mhz": freq.current if freq else None,
            "loadavg": [load1, load5, load15],
        },
        "memory": {
            "total": vm.total, "available": vm.available, "used": vm.used,
            "percent": vm.percent, "cached": getattr(vm, "cached", None),
        },
        "swap": {"total": sm.total, "used": sm.used, "percent": sm.percent},
        "disks": disks,
        "temps": temps,
    }

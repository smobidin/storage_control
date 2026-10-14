import re
import subprocess
from pathlib import Path


def _parse_mdadm(detail_text):
    out = {"state": "unknown", "raid_devices": None, "active_devices": None,
           "failed_devices": None, "spare_devices": None, "total_devices": None}
    if not detail_text:
        return out
    for key, field in (("state", r"State\s*:\s*(.+)"),
                      ("raid_devices", r"Raid Devices\s*:\s*(\d+)"),
                      ("active_devices", r"Active Devices\s*:\s*(\d+)"),
                      ("failed_devices", r"Failed Devices\s*:\s*(\d+)"),
                      ("spare_devices", r"Spare Devices\s*:\s*(\d+)"),
                      ("total_devices", r"Total Devices\s*:\s*(\d+)")):
        m = re.search(field, detail_text)
        if m:
            val = m.group(1).strip()
            out[key] = int(val) if val.isdigit() else val
    return out


def raid_summary():
    text = ""
    try:
        text = Path("/proc/mdstat").read_text()
    except Exception:
        text = ""
    arrays = []
    for m in re.finditer(r"^(md\d+)\s*:\s*(.+?)(?=^md\d+|\Z)", text, re.M | re.S):
        name = m.group(1)
        body = m.group(2)
        first = body.strip().splitlines()[0] if body.strip() else ""
        arrays.append({"name": name, "summary": first, "raw": body.strip()})

    detail_text = None
    try:
        out = subprocess.run(["sudo", "mdadm", "--detail", "/dev/md0"], capture_output=True, text=True, timeout=10)
        detail_text = out.stdout.strip() if out.returncode == 0 else (out.stderr or "mdadm failed")
    except Exception as e:
        detail_text = "error: %s" % e
    status = _parse_mdadm(detail_text or "")
    return {"arrays": arrays, "md0": status, "mdadm_detail": detail_text, "raw": text}

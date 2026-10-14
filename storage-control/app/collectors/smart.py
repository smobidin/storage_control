import re
import subprocess
from pathlib import Path


def _disk_devices():
    devices = []
    for pth in sorted(Path("/sys/block").iterdir(), key=lambda p: p.name):
        name = pth.name
        if name.startswith(("loop", "ram", "md", "zram", "dm-", "sr")):
            continue
        try:
            if int((pth / "size").read_text().strip()) <= 0:
                continue
        except (OSError, ValueError):
            continue
        dev = "/dev/" + name
        if Path(dev).exists():
            devices.append(dev)
    return devices or ["/dev/sda", "/dev/sdb"]


SMART_DEVICES = _disk_devices()


def _run(dev):
    try:
        r = subprocess.run(["sudo", "smartctl", "-a", dev], capture_output=True, text=True, timeout=30)
        return r.returncode, (r.stdout + r.stderr)
    except Exception as e:
        return 1, str(e)


def parse_smart(dev, text):
    health = "unknown"
    m = re.search(r"SMART overall-health self-assessment test result:\s*(.+)", text)
    if m:
        health = m.group(1).strip()
    temp = None
    m = re.search(r"Temperature_Celsius.*?(\d+)\s*(?:\(.*\))?\s*$", text, re.M)
    if not m:
        m = re.search(r"Current Drive Temperature:\s*(\d+)\s*C", text)
    if m:
        temp = int(m.group(1))
    attrs = {}
    for key in ("Reallocated_Sector_Ct", "Current_Pending_Sector", "Offline_Uncorrectable", "UDMA_CRC_Error_Count"):
        m = re.search(r"^\s*\d+\s+" + key + r"\s+0x[0-9a-f]+\s+\d+\s+\d+\s+\d+\s+\S+\s+\S+\s+\S+\s+(\d+)\s*$", text, re.M)
        if m:
            attrs[key] = int(m.group(1))
    model = None
    m = re.search(r"Device Model:\s*(.+)", text)
    if m:
        model = m.group(1).strip()
    serial = None
    m = re.search(r"Serial Number:\s*(.+)", text)
    if serial is None and m:
        serial = m.group(1).strip()
    power_on_hours = None
    m = re.search(r"^\s*9\s+Power_On_Hours\b.*\s(\d+)\s*$", text, re.M)
    if not m:
        m = re.search(r"Power On Hours:\s*(\d+)", text)
    if m:
        try: power_on_hours = int(m.group(1))
        except ValueError: power_on_hours = None
    return {"device": dev, "model": model, "serial": serial, "health": health, "temperature": temp, "attributes": attrs, "power_on_hours": power_on_hours, "raw_tail": text[-1500:]}


def smart_summary():
    out = []
    for dev in _disk_devices():
        rc, text = _run(dev)
        item = parse_smart(dev, text)
        item["rc"] = rc
        out.append(item)
    return out

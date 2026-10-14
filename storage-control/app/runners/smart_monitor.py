from datetime import datetime
from pathlib import Path
import json
import re

from ..extensions import db
from ..models import SmartDiskState, SmartAlert
from ..collectors.smart import _disk_devices, parse_smart, _run


def _parse_ata_error_count(text):
    m = re.search(r"ATA Error Count:\s*(\d+)", text or "")
    return int(m.group(1)) if m else 0


def _metric(label, value, previous):
    if previous is not None and value > previous:
        return "%s increased: %s -> %s" % (label, previous, value)
    return "%s: %s" % (label, value)


def _severity_for(state, issues):
    if any("critical" in x.lower() or "failed" in x.lower() for x in issues):
        return "critical"
    if issues:
        return "warning"
    return "info"


def run_smart_check():
    from wsgi import app
    with app.app_context():
        now = datetime.utcnow()
        for dev in _disk_devices():
            rc, text = _run(dev)
            item = parse_smart(dev, text)
            item["rc"] = rc
            ata_errors = _parse_ata_error_count(text)
            prev = SmartDiskState.query.filter_by(device=dev).first()
            attrs = item.get("attributes", {}) or {}
            reallocated = int(attrs.get("Reallocated_Sector_Ct", 0) or 0)
            pending = int(attrs.get("Current_Pending_Sector", 0) or 0)
            offline = int(attrs.get("Offline_Uncorrectable", 0) or 0)
            metrics = []
            metrics.append(_metric("Reallocated sectors", reallocated, prev.reallocated if prev else None))
            metrics.append(_metric("Current pending sectors", pending, prev.pending if prev else None))
            metrics.append(_metric("Offline uncorrectable sectors", offline, prev.offline_uncorrectable if prev else None))
            metrics.append(_metric("ATA errors", ata_errors, prev.ata_error_count if prev else None))

            issues = []
            health = item.get("health") or "unknown"
            # SMART self-assessment is stored even when smartctl itself cannot
            # run cleanly, so include both its value and exit code.
            health_failed = health not in ("PASSED", "PASSED!") or rc != 0
            if health_failed:
                issues.append("SMART self-assessment: %s (smartctl rc=%s)" % (health, rc))

            temp = item.get("temperature")
            temp_issue = None
            if temp is not None:
                if temp >= 60:
                    temp_issue = "Temperature critical: %s C" % temp
                elif temp >= 50:
                    temp_issue = "Temperature high: %s C" % temp
                if temp_issue:
                    issues.append(temp_issue)

            # Attach every relevant attribute when the disk is degraded or when
            # a counter is non-zero, so the alert is actionable on its own.
            if health_failed or any(v > 0 for v in (reallocated, pending, offline, ata_errors)):
                issues.extend(metrics)
                if temp is not None and not temp_issue:
                    issues.append("Temperature: %s C" % temp)

            # Resolve old alerts before storing the new result.
            SmartAlert.query.filter_by(device=dev, active=True).update(
                {"active": False, "resolved_at": now}, synchronize_session=False)
            if issues:
                alert = SmartAlert(
                    device=dev,
                    severity="critical" if any("increased" in x or "failed" in x.lower() for x in issues) else "warning",
                    message="; ".join(issues),
                    active=True,
                )
                db.session.add(alert)

            if prev is None:
                state = SmartDiskState(
                    device=dev,
                    model=item.get("model"),
                    serial=item.get("serial"),
                    health=item.get("health"),
                    reallocated=attrs.get("Reallocated_Sector_Ct", 0),
                    pending=attrs.get("Current_Pending_Sector", 0),
                    offline_uncorrectable=attrs.get("Offline_Uncorrectable", 0),
                    ata_error_count=ata_errors,
                    temperature=temp,
                    power_on_hours=item.get("power_on_hours"),
                    smartctl_rc=rc,
                    raw_summary=item.get("model"),
                )
                db.session.add(state)
            else:
                prev.model = item.get("model")
                prev.serial = item.get("serial")
                prev.health = item.get("health")
                prev.reallocated = attrs.get("Reallocated_Sector_Ct", 0)
                prev.pending = attrs.get("Current_Pending_Sector", 0)
                prev.offline_uncorrectable = attrs.get("Offline_Uncorrectable", 0)
                prev.ata_error_count = ata_errors
                prev.temperature = temp
                prev.smartctl_rc = rc
                prev.power_on_hours = item.get("power_on_hours")
                prev.last_checked_at = now
            db.session.commit()


if __name__ == "__main__":
    run_smart_check()

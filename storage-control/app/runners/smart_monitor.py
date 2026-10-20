from datetime import datetime, timezone
from pathlib import Path
import json
import re

from ..extensions import db
from ..models import SmartDiskState, SmartAlert, SmartDiskCounterHistory
from ..collectors.smart import _disk_devices, parse_smart, _run


def _parse_ata_error_count(text):
    m = re.search(r"ATA Error Count:\s*(\d+)", text or "")
    return int(m.group(1)) if m else 0


def _load_resolved_counters(state):
    """Load resolved counters baseline from JSON."""
    if state and state.resolved_counters:
        try:
            return json.loads(state.resolved_counters)
        except (json.JSONDecodeError, TypeError):
            return None
    return None


def _save_resolved_counters(state, reallocated, pending, offline, ata_errors):
    """Save current counters as resolved baseline."""
    state.resolved_counters = json.dumps({
        "reallocated": reallocated,
        "pending": pending,
        "offline": offline,
        "ata_errors": ata_errors,
    })


def _compare_counters(current, baseline, label):
    """Compare current value against baseline. Return (increased, message)."""
    if baseline is None:
        return False, f"{label}: {current}"
    if current > baseline:
        return True, f"{label} increased: {baseline} -> {current}"
    return False, f"{label}: {current} (baseline: {baseline})"


def _counters_dict(reallocated, pending, offline, ata_errors):
    return {
        "reallocated": reallocated,
        "pending": pending,
        "offline": offline,
        "ata_errors": ata_errors,
    }


def run_smart_check():
    from wsgi import app
    with app.app_context():
        now = datetime.now(timezone.utc)
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
            temp = item.get("temperature")
            health = item.get("health") or "unknown"

            # Load baseline counters from last user resolution
            baseline = _load_resolved_counters(prev)

            # Compare current counters against baseline
            metrics = []
            increased_counters = []
            for label, current, base_key in [
                ("Reallocated sectors", reallocated, "reallocated"),
                ("Current pending sectors", pending, "pending"),
                ("Offline uncorrectable sectors", offline, "offline"),
                ("ATA errors", ata_errors, "ata_errors"),
            ]:
                base_val = baseline.get(base_key) if baseline else None
                increased, msg = _compare_counters(current, base_val, label)
                metrics.append(msg)
                if increased:
                    increased_counters.append(label)

            issues = []
            health_failed = health not in ("PASSED", "PASSED!") or rc != 0
            if health_failed:
                issues.append(f"SMART self-assessment: {health} (smartctl rc={rc})")

            temp_issue = None
            if temp is not None:
                if temp >= 60:
                    temp_issue = f"Temperature critical: {temp} C"
                elif temp >= 50:
                    temp_issue = f"Temperature high: {temp} C"
                if temp_issue:
                    issues.append(temp_issue)

            # Attach metrics when disk is degraded or any counter is non-zero
            if health_failed or any(v > 0 for v in (reallocated, pending, offline, ata_errors)):
                issues.extend(metrics)
                if temp is not None and not temp_issue:
                    issues.append(f"Temperature: {temp} C")

            # Determine if we should alert: only if counters increased since baseline
            # or if we have no baseline and there are non-zero counters or health failure
            should_alert = False
            if increased_counters:
                # Counters increased since user's last resolution - always alert
                should_alert = True
            elif baseline is None:
                # No baseline yet - alert on health failure or non-zero counters
                if health_failed or any(v > 0 for v in (reallocated, pending, offline, ata_errors)):
                    should_alert = True
            # If baseline exists and no counters increased, don't alert (even if health_failed)

            # Resolve old alerts before storing new result
            SmartAlert.query.filter_by(device=dev, active=True).update(
                {"active": False, "resolved_at": now}, synchronize_session=False)

            if should_alert and issues:
                alert = SmartAlert(
                    device=dev,
                    severity="critical" if health_failed or any("increased" in x for x in metrics) else "warning",
                    message="; ".join(issues),
                    active=True,
                    alert_counters=json.dumps(_counters_dict(reallocated, pending, offline, ata_errors)),
                )
                db.session.add(alert)

            # Save history record
            history = SmartDiskCounterHistory(
                device=dev,
                reallocated=reallocated,
                pending=pending,
                offline_uncorrectable=offline,
                ata_error_count=ata_errors,
                temperature=temp,
                health=health,
                smartctl_rc=rc,
                checked_at=now,
                source="auto",
            )
            db.session.add(history)

            # Update or create disk state
            if prev is None:
                state = SmartDiskState(
                    device=dev,
                    model=item.get("model"),
                    serial=item.get("serial"),
                    health=health,
                    reallocated=reallocated,
                    pending=pending,
                    offline_uncorrectable=offline,
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
                prev.health = health
                prev.reallocated = reallocated
                prev.pending = pending
                prev.offline_uncorrectable = offline
                prev.ata_error_count = ata_errors
                prev.temperature = temp
                prev.smartctl_rc = rc
                prev.power_on_hours = item.get("power_on_hours")
                prev.last_checked_at = now
            db.session.commit()


def run_manual_smart_check():
    """Manual trigger for SMART check - same logic as auto but with source='manual'."""
    from wsgi import app
    with app.app_context():
        now = datetime.now(timezone.utc)
        results = []
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
            temp = item.get("temperature")
            health = item.get("health") or "unknown"

            baseline = _load_resolved_counters(prev)

            # Compare against baseline
            increased_counters = []
            for label, current, base_key in [
                ("Reallocated sectors", reallocated, "reallocated"),
                ("Current pending sectors", pending, "pending"),
                ("Offline uncorrectable sectors", offline, "offline"),
                ("ATA errors", ata_errors, "ata_errors"),
            ]:
                base_val = baseline.get(base_key) if baseline else None
                if base_val is not None and current > base_val:
                    increased_counters.append(label)

            # Save history with manual source
            history = SmartDiskCounterHistory(
                device=dev,
                reallocated=reallocated,
                pending=pending,
                offline_uncorrectable=offline,
                ata_error_count=ata_errors,
                temperature=temp,
                health=health,
                smartctl_rc=rc,
                checked_at=now,
                source="manual",
            )
            db.session.add(history)

            # Update state
            if prev is None:
                state = SmartDiskState(
                    device=dev,
                    model=item.get("model"),
                    serial=item.get("serial"),
                    health=health,
                    reallocated=reallocated,
                    pending=pending,
                    offline_uncorrectable=offline,
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
                prev.health = health
                prev.reallocated = reallocated
                prev.pending = pending
                prev.offline_uncorrectable = offline
                prev.ata_error_count = ata_errors
                prev.temperature = temp
                prev.smartctl_rc = rc
                prev.power_on_hours = item.get("power_on_hours")
                prev.last_checked_at = now

            results.append({
                "device": dev,
                "model": item.get("model"),
                "health": health,
                "temperature": temp,
                "reallocated": reallocated,
                "pending": pending,
                "offline": offline,
                "ata_errors": ata_errors,
                "increased_since_resolve": increased_counters,
                "baseline": baseline,
            })
        db.session.commit()
        return results


if __name__ == "__main__":
    run_smart_check()
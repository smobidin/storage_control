import subprocess, re, os
from pathlib import Path
from ..config import STORAGE_HOME

IMMICH_SRC = STORAGE_HOME / "immich" / "src"
NEXTCLOUD_ROOT = STORAGE_HOME / "nextcloud"


def _run(args, cwd):
    try:
        r = subprocess.run(args, cwd=str(cwd), capture_output=True, text=True, timeout=60)
        return r.returncode, (r.stdout + r.stderr).strip()
    except Exception as e:
        return 1, str(e)


def _git_version(cwd):
    """Version of a git checkout, as shown on the dashboard.

    `git describe --tags --exact-match` only answers when HEAD sits exactly on
    a tag; a checkout carrying local commits (branch `local/patches`) falls
    through to a commit hash, which reads as version 0 in comparisons and
    makes the UI claim an update is available. So: exact tag, else the nearest
    reachable tag plus how many commits are stacked on it (v3.3.3+1), and the
    hash only when the checkout is not reachable from any tag.
    """
    rc, out = _run(["git", "describe", "--tags", "--exact-match"], cwd)
    if rc == 0 and out:
        return out
    rc, tag = _run(["git", "describe", "--tags", "--abbrev=0"], cwd)
    if rc == 0 and tag:
        rc2, ahead = _run(["git", "rev-list", "--count", "%s..HEAD" % tag], cwd)
        ahead = (ahead or "").strip()
        if rc2 == 0 and ahead.isdigit() and ahead != "0":
            return "%s+%s" % (tag, ahead)
        return tag
    rc, out = _run(["git", "rev-parse", "--short", "HEAD"], cwd)
    return out if rc == 0 else "unknown"


def immich_current():
    return _git_version(IMMICH_SRC)


def immich_tags(limit=30):
    rc, out = _run(["git", "tag", "--sort=-creatordate"], IMMICH_SRC)
    if rc != 0:
        return []
    tags = []
    for line in out.splitlines():
        t = line.strip()
        if t.startswith("v") and "-" not in t.lstrip("v"):
            # strip pre-release tags like v3.3.1-rc1
            tags.append(t)
    return tags[:limit]


def nextcloud_current():
    return _git_version(NEXTCLOUD_ROOT)


def nextcloud_current_major():
    current = nextcloud_current()
    match = re.search(r"(\d+)\.", current)
    if match:
        return int(match.group(1))
    info = release_info("nextcloud", current)
    latest = info.get("latest") or ""
    match = re.search(r"(\d+)\.", latest)
    return int(match.group(1)) if match else 0


def nextcloud_tags(limit=30):
    # avoid blocking the request with network fetch; update run performs fetch
    major = nextcloud_current_major()
    rc, out = _run(["git", "tag", "--sort=-creatordate"], NEXTCLOUD_ROOT)
    if rc != 0:
        return []
    out_tags = []
    for line in out.splitlines():
        t = line.strip()
        # stable tags only: v35.0.1, never v35.0.2rc2 or v35.0.1-rc1
        if not re.fullmatch(r"v\d+\.\d+\.\d+", t):
            continue
        tag_major = t.split(".")[0].lstrip("v")
        # keep the installed major line and anything newer; no downgrades
        if int(tag_major) >= major:
            out_tags.append(t)
    return out_tags[:limit]

import json
import time
import urllib.request

_CACHE = {}
_CACHE_TTL = 300

_RELEASE_REPOS = {
    "immich": "immich-app/immich",
    "nextcloud": "nextcloud/server",
}

_MIGRATION_KEYWORDS = (
    "migration", "migrate", "database", "db ", "occ ", "upgrade",
    "backup", "breaking", "important", "env", "config",
    "maintenance", "index", "rebuild", "npm", "pnpm", "php",
)


def _version_key(value):
    if not value:
        return ()
    # Only the leading numeric part: a local-commit marker ("v3.3.3+1") or a
    # pre-release suffix ("v3.3.3-rc1") must not outrank "v3.3.3" itself.
    match = re.match(r"\d+(?:\.\d+)*", (value or "").lstrip("vV"))
    if not match:
        return ()
    return tuple(int(x) for x in match.group(0).split("."))


def _fetch_release(repo, timeout=10):
    url = "https://api.github.com/repos/%s/releases/latest" % repo
    req = urllib.request.Request(
        url,
        headers={
            "User-Agent": "storage-control",
            "Accept": "application/vnd.github+json",
        },
    )
    with urllib.request.urlopen(req, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def _migration_steps(body):
    steps = []
    seen = set()
    for raw in (body or "").splitlines():
        line = raw.strip().lstrip("#").lstrip("-*•").strip()
        if not line or len(line) < 8:
            continue
        lowered = line.lower()
        if any(keyword in lowered for keyword in _MIGRATION_KEYWORDS):
            if line not in seen:
                seen.add(line)
                steps.append(line)
    return steps[:15]


def release_info(target, current, timeout=10):
    now = time.monotonic()
    cache_key = (target, current)
    cached = _CACHE.get(cache_key)
    if cached and (now - cached["at"]) < _CACHE_TTL:
        return cached["value"]

    repo = _RELEASE_REPOS[target]
    value = {
        "current": current,
        "latest": current,
        "available": False,
        "error": None,
        "body": "",
        "migration_steps": [],
        "url": None,
    }
    try:
        data = _fetch_release(repo, timeout=timeout)
        latest = (data.get("tag_name") or "").strip()
        body = data.get("body") or ""
        value["latest"] = latest or current
        value["body"] = body
        value["url"] = data.get("html_url")
        value["migration_steps"] = _migration_steps(body)
        value["available"] = bool(
            latest
            and latest != current
            and _version_key(latest) > _version_key(current)
        )
    except Exception as exc:
        value["error"] = str(exc)

    _CACHE[cache_key] = {"at": now, "value": value}
    return value


def immich_release_info(current=None):
    target_current = current or immich_current()
    return release_info("immich", target_current)


def nextcloud_release_info(current=None):
    target_current = current or nextcloud_current()
    return release_info("nextcloud", target_current)


def add_candidate(tags, latest):
    if not latest or latest in tags:
        return list(tags)
    return [latest] + list(tags)

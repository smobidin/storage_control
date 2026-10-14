import subprocess, json, os, threading, time, shlex
from pathlib import Path
from datetime import datetime

BASE_RUNS = None

def log_write(path, text):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a", encoding="utf-8") as f:
        f.write(text)
        f.flush()


def run_cmd(cmd, cwd=None, env=None, log_path=None, timeout=3600):
    """Run a command, append stdout/stderr to log_path. Returns (rc, output)."""
    if log_path:
        log_write(log_path, f"\n$ {' '.join(cmd) if isinstance(cmd, list) else cmd}\n")
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
        out = p.stdout + p.stderr
        if log_path:
            log_write(log_path, out)
        return p.returncode, out
    except subprocess.TimeoutExpired as e:
        msg = f"TIMEOUT after {timeout}s\n"
        if log_path:
            log_write(log_path, msg)
        return 124, msg
    except Exception as e:
        msg = f"ERROR: {e}\n"
        if log_path:
            log_write(log_path, msg)
        return 1, msg


def async_call(func, *args, **kwargs):
    t = threading.Thread(target=func, args=args, kwargs=kwargs, daemon=True)
    t.start()
    return t

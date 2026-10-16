import os
from pathlib import Path
from dotenv import load_dotenv

load_dotenv(Path(__file__).resolve().parents[2] / ".env", override=False)

STORAGE_HOME = Path(os.environ.get("STORAGE_HOME", "/home/storage"))

IMMICH_SRC = STORAGE_HOME / "immich" / "src"
IMMICH_ENV = STORAGE_HOME / "immich" / ".env"
IMMICH_PATCHES_DIR = Path(__file__).resolve().parents[2] / "tools" / "immich" / "patches"
NEXTCLOUD_ROOT = STORAGE_HOME / "nextcloud"
STALWART_HOME = STORAGE_HOME / "stalwart"
STALWART_BIN = STALWART_HOME / "bin" / "stalwart"
STALWART_DATA = Path("/mnt/data/stalwart")
BACKUP_ROOT = Path("/mnt/backup/backups")
DATA_DIR = Path(os.environ.get("STORAGE_CONTROL_DATA", str(STORAGE_HOME / ".local" / "share" / "storage-control")))
RUNS_DIR = DATA_DIR / "runs"


class Config:
    SECRET_KEY = os.environ.get("SECRET_KEY", "dev-insecure-change-me")
    SQLALCHEMY_DATABASE_URI = "sqlite:///" + str(DATA_DIR / "app.db")
    SQLALCHEMY_TRACK_MODIFICATIONS = False
    WTF_CSRF_ENABLED = True
    WTF_CSRF_TIME_LIMIT = None
    DATA_DIR = DATA_DIR
    RUNS_DIR = RUNS_DIR
    IMMICH_SRC = IMMICH_SRC
    IMMICH_ENV = IMMICH_ENV
    IMMICH_PATCHES_DIR = IMMICH_PATCHES_DIR
    NEXTCLOUD_ROOT = NEXTCLOUD_ROOT
    STALWART_HOME = STALWART_HOME
    STALWART_BIN = STALWART_BIN
    STALWART_DATA = STALWART_DATA
    BACKUP_ROOT = BACKUP_ROOT
    ADMIN_USERNAME = os.environ.get("ADMIN_USERNAME", "admin")
    ADMIN_PASSWORD = os.environ.get("ADMIN_PASSWORD")
    STORAGE_HOME = STORAGE_HOME
    # system paths
    PROC_MDSTAT = Path("/proc/mdstat")

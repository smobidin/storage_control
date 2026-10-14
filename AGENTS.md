# AGENTS.md

Repo is a NAS monitoring/management console for Immich + Nextcloud. Docs at repo root; Flask app is in `storage-control/`.

## Sources of truth

- `doc/system.md` — installed NAS hosts/paths.
- `doc/pnan.md` — implementation plan.
- Executable config overrides docs. `storage-control/app/config.py` loads `.env` from the **repository root** via `Path(__file__).resolve().parents[2]`, despite `storage-control/README.md` saying `storage-control/.env`.
- If docs conflict with config or code, trust config/code.

## App shape

- Entrypoint: `storage-control/wsgi.py` -> `create_app()` in `storage-control/app/__init__.py`.
- Blueprints there: auth, dashboard, services, backups, planner (`/scheduler`), updates.
- All templates are under `storage-control/app/templates`; blueprints register it via `../../templates`.
- SQLite DB is auto-created at `$STORAGE_CONTROL_DATA/app.db`.
- Admin is seeded only if `ADMIN_PASSWORD` is set; an existing admin password is reset.
- APScheduler starts unless `ENABLE_SCHEDULER=false`; startup failure is logged but app still boots.

## Run / verify (from `storage-control/`)

- Install: `python3 -m venv .venv && .venv/bin/pip install -r requirements.txt`
- Dev: `SECRET_KEY=... ADMIN_PASSWORD=... ENABLE_SCHEDULER=true .venv/bin/python -c "from wsgi import app; app.run(host='0.0.0.0', port=8765)"`
- Prod: `.venv/bin/gunicorn -w 1 -k gthread --threads 4 -b 0.0.0.0:8765 wsgi:app`
- User service is installed by `tools/install.sh`; it binds `0.0.0.0:8765` by default (all interfaces), so use firewall/nginx or install with `--bind 127.0.0.1:8765`.
- No automated tests. After edits: `.venv/bin/python -m compileall -q .`, then `curl -s http://127.0.0.1:8765/login`.
- Keep exactly one Gunicorn worker; long update/preserve jobs run in daemon threads and use subprocess.

## Environment and data

- `SECRET_KEY`, `ADMIN_PASSWORD`, optional `ADMIN_USERNAME`, `STORAGE_HOME`, `STORAGE_CONTROL_DATA`, `ENABLE_SCHEDULER`.
- Data/logs/runs are under `$STORAGE_CONTROL_DATA` (default `/home/storage/.local/share/storage-control`).
- Backups are under `/mnt/backup/backups`.

## Dashboard and health gotchas

- App Health card must stay above Host; it shows service badges for Immich and Nextcloud plus current/available versions.
- App Health and service table depend on `/api/health/services`; the route stays in `dashboard/routes.py` because dashboard JS calls it.
- Other live endpoints are `/api/health/summary`, `/api/health/smart`, `/api/health/smart/status`, and `/updates/api/availability`.
- Dashboard version availability comes from GitHub Releases and is cached for about 5 minutes.
- Disks shows only physical block-backed filesystems; loop/virtual and non-`/dev/` devices are deliberately filtered in `app/collectors/system.py`.
- Temperature collection uses `psutil.sensors_temperatures()` with fallback to the `sensors` CLI.
- Fan support was removed as unused; do not reintroduce a `fans` field unless backed by real data.
- Jinja templates must have valid tag pairing; missing openers previously caused 500s.
- Python files must not end with stray markers such as `PY`.

## Update and preserve operations

- Updates page reports GitHub release availability and changelog for Immich and Nextcloud.
- `pnpm`/`node` for Immich live only in `~/immich/node/bin` (corepack shims) and are NOT on the storage-control service PATH; `runners/update_immich.py::_tool_env()` prepends that dir plus `COREPACK_ENABLE_DOWNLOAD_PROMPT=0` for the pnpm/build/migrations steps. Do not call `pnpm` bare in a runner.
- Build JS step commonly fails because `npm` is absent on host; Nextcloud ships prebuilt `dist`, so this is usually non-fatal.
- Immich smoke test must poll, not fire once: `systemctl restart` returns before the API binds `:2283`. It hits `/api/server/ping` because `/api/health` is gone upstream and any unknown path returns 200 HTML from the SPA catch-all.
- The core workflow plugin is loaded from `~/immich/build/plugins/immich-plugin-core/{manifest.json,dist}` (that is the `server/Dockerfile` layout; the old `build/plugins-core-dist` symlink was never read by anything). Missing it logs `Failed to import plugin ...` on every boot. Rebuild: `pnpm --filter @immich/sdk --filter @immich/plugin-sdk --filter @immich/plugin-core build`, then copy `packages/plugin-core/{manifest.json,dist}` there. It needs `extism-js` (`~/immich/tools/extism-js`) and `wasm-merge` (`~/immich/tools/binaryen/binaryen-version_124/bin`) on PATH — neither is on the service PATH.
- Nextcloud updates must fetch without recursive submodules: use `git fetch origin --tags --no-recurse-submodules`; the runner already does.
- After checkout, Nextcloud needs `git submodule update --init --recursive`. If this is skipped, `3rdparty` stays on the old commit and `occ upgrade` can fail with missing paths / Symfony errors.
- `occ upgrade` may exit 1 due to stale optional app references even when status and DB are fine. Verify `php occ status`, especially `versionstring`, `maintenance`, and `needsDbUpgrade`.
- `runners/update_nextcloud.py` has a `viewer_sync` step (after `restart_services`): `/apps*/*` is git-ignored, so checkout never refreshes the bundled-but-separate `viewer` app, and on a major jump Nextcloud disables it — office files then download instead of opening Collabora, because the `LoadViewer` event never fires and richdocuments never registers its handler. The step re-extracts `apps/viewer` from `https://download.nextcloud.com/server/releases/nextcloud-<tag>.tar.bz2` (viewer is not on the app store, so the release tarball is the only source) and re-enables it. Patch updates just compare `<nextcloud max-version>` in `appinfo/info.xml` and skip the ~250 MB download.
- Preserve snapshots live under `/mnt/backup/backups/preserves` and are tracked in `update_preserve`. They support prepare, restore, delete, and live logs through `/updates/preserve/<id>/{log,api,...}`.
- `~/immich/src` carries one local patch (`checkDiskUsage`: `frsize` -> `bsize`, Node returns no `frsize` here). It lives on branch `local/patches` there and as `tools/immich/patches/0001-storage-statfs-bsize.patch` in this repo. `runners/update_immich.py` re-applies every `tools/immich/patches/*.patch` in the non-fatal `apply_local_patches` step right after `git checkout` (before the server build, so the patch reaches `dist`). Identity in that repo is repo-local `storage <storage@nas.local>`; never push that branch upstream (origin is immich-app/immich).
- Prepare stashes local Git changes before backup, so preserve's file copies may predate the stash. For custom tracked files such as `.htaccess`, recover them from `git show refs/preserve/<id>:...` rather than from the copied file.
- On failure the runner turns maintenance mode off where possible; verify it manually before retrying.

## Host backups

- `backup-immich-nc.timer` (04:00) dumps immich/nextcloud DBs and configs to `/mnt/backup/backups`. Its stale twin at `~/immich/tools/backup_immich_nc.sh` still points at the non-existent `/mnt/recovery`; the live script is `/home/storage/tools/backup_immich_nc.sh`.
- `backup-home-rsync.timer` (05:00) runs `tools/backup_home_rsync.sh`: an rsync mirror of `/home/storage` to `/mnt/backup/backups/home-storage/{current,deleted,logs}`, excluding `/.cache/`. The system disk `/dev/sde2` holds `/` and `/home/storage` and is failing, so this is the main protection for the home directory. Media lives on `/mnt/data` (RAID) and is not copied.

- `Runner app/runners/smart_monitor.py` performs the nightly SMART check; it stores `SmartDiskState` and active `SmartAlert` records. Startup also runs one baseline check asynchronously.
- SMART discovery is dynamic from `/sys/block`; loops, RAID, RAM, empty, and virtual block devices are excluded.
- Dashboard renders SMART alarm rows red and lists active alarms from `/api/health/smart/status`.
- `tools/immich/night_fix_thumbnails.cjs` is the user's own nightly thumbnail fixer, run by `immich-nightfix.timer` (03:30). It lives in this repo, not in `~/immich/src`, because Preserve's `git stash push -u` would swallow it as an untracked file. It loads `sharp`/`pg`/`thumbhash`/exiftool from immich's `node_modules` via `createRequire`.

## System-service access

- Whitelisted operations are in `scripts/sudoers-storage-control`.
- Collector endpoints depend on `sudo mdadm --detail /dev/md0`, `sudo smartctl -a` for physical block devices, and system service controls.
- Update/install flows assume systemd units such as `nginx`, `php8.5-fpm`, `postgresql`, `redis-server`, `immich-server`, and `immich-ml`.

## Git

- Branch `main`, remote `origin` = `git@github.com-storage_gh:smobidin/storage_control.git` (GitHub, public).
- History was squashed to a single commit and force-pushed; the old repo was deleted so old SHAs are gone server-side.
- Git hooks live in `tools/githooks/` (enabled locally via `git config core.hooksPath tools/githooks`).
- `tools/githooks/pre-commit` runs `gitleaks protect --staged`; if gitleaks is not installed the commit is blocked (one-off bypass: `SKIP_GITLEAKS=1 git commit ...`).
- CI scans full history and tree on every push/PR: `.github/workflows/gitleaks.yml` (`gitleaks git .` + `gitleaks dir .`).
- Secrets never enter git: `.gitignore` covers `*.env`/`secrets.env`; real secrets live in `~/.config/storage-control/secrets.env`.
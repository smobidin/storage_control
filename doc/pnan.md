# План реализации системы мониторинга и управления (NAS)

> Новая система: **storage-control** — веб-приложение на Flask (Python), запускаемое как `systemd --user` сервис для пользователя `storage`.  
> Базируется на [`doc/system.md`](./system.md).

---

## 1. Цели и рамки

**Цели:**
- Единый веб-интерфейс для мониторинга здоровья NAS и ИТ-системы (Immich + Nextcloud).
- Управление обновлениями (git + сборка + миграции БД + предварительные бэкапы БД).
- CRUD для задач резервного копирования и просмотр/управление файлами бэкапов.
- Планировщик служебных задач (cron/systemd timers + собственные задачи через APScheduler).
- Управление сервисами (start/stop/restart) по одному и группами с учётом зависимостей.

**Вне рамок (первая версия):**
- Полноценная multi-user/RBAC (один администратор + токен доступа).
- Удалённый доступ к файловой системе beyond бэкапов.
- Автоматическое развёртывание через Ansible/Docker (только user systemd unit).

---

## 2. Высокоуровневая архитектура

```
┌──────────────────────────────────────────────────────────────────┐
│  nginx (8080/8081, user/system) reverse proxy                    │
│    ├─ photo.<DOMAIN>   → immich (2283)                           │
│    ├─ drive.<DOMAIN>   → nextcloud (8443)                        │
│    └─ control.<DOMAIN> → Flask app (127.0.0.1:8765)              │   <-- новый блок
└──────────────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────────────┐
│  Flask (storage user, systemd --user)                            │
│  ├─ Blueprints: dashboard, updates, backups, scheduler, services │
│  ├─ APScheduler (in-process, SQLite jobstore)                    │
│  ├─ SQLAlchemy + SQLite (~/.local/share/storage-control/app.db)   │
│  ├─ psutil / sensors / smartctl collectors                       │
│  └─ subprocess wrappers вокруг systemctl, git, npm, pg_dump       │
└──────────────────────────────────────────────────────────────────┘
```

**Стек:**
- Python 3.12 (Ubuntu 26.04)
- Flask 3.x + Blueprint
- Flask-SQLAlchemy + SQLite (WAL), Alembic для миграций схемы БД приложения
- APScheduler 3.x (BackgroundScheduler + SQLiteJobStore)
- Flask-Login (сессии, локальный пользователь)
- psutil, pySMART (или smartctl CLI), lm-sensors (`sensors` CLI), markdown (отчёты)
- GitPython (или subprocess git) — предпочесть subprocess для контроля вывода
- requests, packaging (semver сравнение)
- gunicorn за nginx (production) или встроенный (dev); для user service — gunicorn (1 worker + threads) либо `flask run` под systemd.

---

## 3. Структура проекта

```
/home/storage/storage-control/                 # или ~/storage-control
├── app/
│   ├── __init__.py                  # create_app factory
│   ├── config.py                    # Paths, secrets, service catalogue
│   ├── models.py                    # SQLAlchemy models
│   ├── auth/                        # Blueprint: login/logout
│   ├── dashboard/                   # BP: главная + health widgets
│   ├── updates/                     # BP: обновления immich/nextcloud
│   ├── backups/                     # BP: CRUD бэкапов
│   ├── scheduler/                   # BP: планировщик
│   ├── services/                    # BP: управление сервисами
│   ├── collectors/                  # модули сбора метрик
│   │   ├── system.py                # CPU/RAM/disk/temp/fans
│   │   ├── raid.py                  # /proc/mdstat + mdadm
│   │   ├── smart.py                 # smartctl
│   │   └── services.py              # состояние unit-ов
│   ├── runners/                     # длинные задачи
│   │   ├── update_immich.py
│   │   ├── update_nextcloud.py
│   │   ├── backup_task.py
│   │   └── migrate_db.py
│   ├── cli.py                       # (опционально) Flask CLI команды
│   └── templates/ + static/
├── migrations/                      # Alembic
├── systemd/
│   └── storage-control.service       # user unit
├── scripts/
│   ├── install.sh                   # установка зависимостей, venv, unit
│   ├── sudoers-storage-control       # NOPASSWD правила
│   └── nginx-control-site.conf      # пример server block
├── requirements.txt
├── wsgi.py
└── README.md
```

Доп. пути (из system.md):
- Immich src: `~/immich/src/`, env `~/immich/.env`
- Nextcloud: `~/nextcloud/` (ветка stable34)
- Бэкапы: `/mnt/backup/backups/{immich,nextcloud}/`
- Скрипт: `~/tools/backup_immich_nc.sh`
- Юниты: `~/.config/systemd/user/`, системные юниты nginx/postgresql/redis/php8.5-fpm

---

## 4. Данные (модели SQLAlchemy)

**Таблицы:**
- `users` — id, username (argon2 hash), created_at.
- `service_state_cache` — snapshot последней проверки.
- `backup_tasks` — id, name, kind (immich_db/nextcloud_db/immich_config/nextcloud_config/nginx/postgres/pkglist/all), schedule_cron (nullable), command (nullable — по умолчанию встроенный), enabled, last_run_at, last_status, last_log_path, retention_days (default 14), created/updated.
- `backup_runs` — id, task_id, started_at, finished_at, status (running/success/failed), outdir, files (JSON), log (tail), exit_code.
- `scheduled_jobs` — id, name, job_type (systemd_timer/systemd_service/custom_shell), target (unit name), schedule_cron (для custom), enabled, notes, last_run_at, last_status. Для системных таймеров — только зеркало статуса (read-only).
- `update_runs` — id, target (immich/nextcloud), from_version, to_version, steps (JSON), status, started/finished, log_path, backup_file.
- `audit_log` — id, actor, action, object, payload(JSON), ts, ip.

Job-ы APScheduler хранятся в своей SQLite-таблице (jobstore) отдельно; `scheduled_jobs` — метаданные UI.

Миграции схемы самого приложения — Alembic, запускаются при старте (через entrypoint или ручным `flask db upgrade` в install.sh).

---

## 5. Конфигурация приложения

`config.py`:
- `APP_ROOT`, `DATA_DIR`, `DB_PATH`, `BACKUP_ROOT=/mnt/backup/backups`, `IMMICH_SRC`, `NEXTCLOUD_ROOT`.
- `SERVICE_CATALOG` — словарь сервисов с зависимостями:
  - **immich-server** (user): deps → immich-ml (optional? actually ml independent), postgresql (system)
  - **immich-ml** (user): deps → postgresql, immich-server? ML работает независимо; рекомендовать старт после server.
  - **immich-nightfix.service/timer** (user)
  - **backup-immich-nc.timer** (user)
  - **certbot-ready** — system timers (read-only)
  - nginx, postgresql, redis-server, php8.5-fpm (system, требуется sudo)
- версии/теги обновления: `IMMICH_DEFAULT_CHANNEL=release`, `NEXTCLOUD_BRANCH=stable34`.
- Секреты из `~/.config/storage-control/secrets.env` (SECRET_KEY, admin hash). Файл created install.sh (chmod 600).

---

## 6. Модули collectors (дашборд здоровья)

### 6.1 Система (collectors/system.py)
- CPU: `psutil.cpu_percent(percpu=True)`, `cpu_count`, частоты `cpu_freq()`, load average `os.getloadavg()`.
- Память: `virtual_memory()`, swap.
- Диски: `disk_partitions()` + `disk_usage()` для /, /mnt/data, /mnt/backup, /home; показать фс, inode utilization.
- Температуры: `psutil.sensors_temperatures()` (coretemp/k10temp). Fallback на `sensors -j`.
- Вентиляторы: `psutil.sensors_fans()` + fallback `sensors`.
- Аптайм/хостнейм/ОС: `/etc/os-release`, `platform`.

### 6.2 RAID (collectors/raid.py)
- Парсинг `/proc/mdstat`: состояние (clean/degraded), диски, sync %, speed.
- `mdadm --detail /dev/md0` (через sudo) — состояние, failed disks.
- Проверка mdcheck таймеров статус.

### 6.3 SMART (collectors/smart.py)
- `smartctl -a /dev/sdX` (sda/sdb) через sudo; парсить ключевые атрибуты: Reallocated_Sector_Ct, Current_Pending_Sector, UDMA_CRC_Error_Count, temperature, overall health ("PASSED/FAILED").
- Кэшировать на 5 мин (чтобы не спамить).

### 6.4 Сервисы (collectors/services.py)
- Для каждого unit: `systemctl is-active`, `is-enabled`, `show` (ExecMainStatus, ActiveEnterTimestamp, MemoryCurrent), через subprocess. user vs system flag.
- Для таймеров: `systemctl list-timers` + next elapse.

### 6.5 API endpoints дашборда
- `GET /api/health/summary` — агрегат (CPU/RAM/disk/load/temp/fan/raid) для виджетов.
- `GET /api/health/services` — таблица всех сервисов.
- `GET /api/health/disks` — диски+SMART+RAID.
- `GET /api/health/history?metric=cpu&range=1h` — (опционально v2: influx? хранение в SQLite samples каждые 30с).

Автообновление фронтенда каждые 10–30 сек (JS fetch), SSE/WebSocket не обязательны (polling).

---

## 7. Раздел управления обновлениями

### 7.1 Immich
Шаги runner `update_immich.py`:
1. Проверить git status чистота (`git status --porcelain`), текущий tag/branch.
2. Проверить доступные tags (sorted, filter v3.x), предложить latest stable (Comparing semver, отказ от pre-release).
3. **Бэкап БД:** `pg_dump -Fc -h localhost -U storage -d immich -f /mnt/backup/backups/immich/immich_db_preupdate_<ts>.dump`.
4. Бэкап конфигов (~/immich/.env + ~/.config(systemd user units)).
5. `git fetch origin --tags && git checkout <tag>`.
6. `npm ci` (+ `npm install` fallback).
7. `npm run build` (web+server) — таймаут 20+ мин.
8. DB migrations: запустить immich migration команду. **Требуется уточнить точную команду** (Immich server при старте выполняет миграции автоматически — обычно нужно просто перезапустить immich-server; миграции TypeORM/Prisma запускаются при старте). В плане: флаг "auto-migrate on startup"; runner перезапускает сервис и проверяет лог на ошибки; опциональный шаг `npm run db:migrations:run`.
9. Перезапуск immich-server, immich-ml.
10. Smoke test: curl `http://127.0.0.1:2283/api/health` ожидаем 200.
11. При фейле — guided rollback (checkout previous tag + restore DB).

UI: wybór тега (dropdown с текущим, latest, список), чекбоксы (backup DB, backup configs, auto-start), progress log (SSE или polling `update_runs`).

### 7.2 Nextcloud
Шаги:
1. Проверить git status.
2. Проверить теги v34.0.x (оставаться в major 34; предупреждение при смене major).
3. **Бэкап БД:** `pg_dump -Fc` nextcloud_db.
4. Бэкап config.php + data dir metadata? (config.php, htaccess) tar.gz.
5. Включить maintenance mode: `php occ maintenance:mode --on`.
6. `git fetch origin --tags && git checkout <tag>`.
7. `composer install --no-dev` (если deps). + `npm ci` для фронта (Nextcloud использует make? Уточнить).
8. `php occ upgrade` (ключевая миграция СУБД).
9. Проверка целостности: `php occ integrity:check-core`, `php occ db:add-missing-indices`.
10. maintenance mode off.
11. Перезапуск php-fpm/nginx (system) при необходимости.
12. Smoke test: `php occ status` и HTTP HEAD на drive.

Безопасность: все операции через subprocess с таймаутом; логи в `~/.local/share/storage-control/runs/<id>.log`; UI показывает tail.

### 7.3 Общие правила runner-ов
- Выполнение в фоне (thread или отдельный subprocess), запись в `update_runs`.
- Блокировка повторного запуска (file lock / row `status=running`).
- Прерывание: кнопка "Abort" sends SIGTERM + отметка failed (best-effort).
- Уведомление по завершении: запись в audit_log; опционально email/webhook (v2).

---

## 8. Раздел управления резервными копиями

### 8.1 CRUD задач (`backup_tasks`)
- Create/Edit/Delete/Duplicate.
- Поля: name, kind (enum), schedule (preset: manual/daily 04:00/custom cron), enabled, retention_days, command template (для custom).
- Для встроенных видов — готовые runner-ы (используют существующий `backup_immich_nc.sh` логику или собственную Python-реализацию; рекомендовать port bash-скрипта на Python для лучшего мониторинга).
- Запуск по расписанию через APScheduler; ручной «Run now».

### 8.2 Файлы бэкапов
- Листинг каталогов `/mnt/backup/backups/{immich,nextcloud}/` и корня (nginx/postgres/pkg).
- Метаданные: name, size, mtime, type, linked run.
- Действия: скачать (send_file), удалить, проверить целостность pg_restore --list (для .dump), расчёт SHA256.
- Очистка по retention: ежедневный job удаляет старее N дней (типы по расширениям).
- Защита от path traversal (validate basename, normalize path).

### 8.3 restore-мастер (опционально v1.1)
- Выбор файла .dump → подтверждение → `pg_restore -c` для нужной БД с остановкой сервиса и бэкапом текущей БД.
- Рекомендовать включить в v1 как "danger zone" с двойным подтверждением.

---

## 9. Планировщик задач

### 9.1 Источники
- Системные таймеры (read-only просмотр): certbot-renew, mdcheck_start/continue, backup-immich-nc (user), immich-nightfix.
- Пользовательские таймеры (user units) — enable/disable + run now через systemctl --user.
- Служебные задачи storage-control (APScheduler): backup tasks, cleanup, health sampling,SMART check.
- Custom shell jobs (с upload скрипта / textarea) — с warning + audit.

### 9.2 UI
- Таблица всех job-ов: имя, источник, расписание (human+cron), следующий запуск, последний результат, enabled toggle.
- Для системных unit-ов: кнопки Enable/Disable/Start (некоторые требуют sudo) с предупреждением.
- Создание job-а: форма (name, cron, command, timeout, run-as), валидация cron (croniter).
- Лог последнего запуска.

### 9.3 Безопасность custom jobs
- Ограничить команды белым списком? Для single-admin можно разрешить, но требовать явного подтверждения; записывать в audit_log.
- Таймауты + max concurrent.

---

## 10. Раздел управления сервисами

### 10.1 Каталог сервисов и зависимости
Группы:
- **immich:** immich-server (deps: postgresql[sys], redis? no), immich-ml (deps: postgresql), immich-nightfix.timer, immich-nightfix.service.
- **nextcloud:** nginx[sys], php8.5-fpm[sys], redis-server[sys], postgresql[sys] (read-only status for system ones; управление через sudo или read-only).
- **infra:** nginx, postgresql, redis-server, php8.5-fpm, certbot.timer, mdcheck timers, backup timer.
- **self:** storage-control.

Для каждого: scope (user/system), prerequisite check (port free, DB reachable).

### 10.2 Действия
- start / stop / restart / status для single.
- Bulk: "start immich stack" (postgres check → immich-ml → immich-server), "restart all", "safe stop" (stop app before DB).
- Показ логов (journalctl tail) через SSE/polling, last 200 строк.
- Подтверждение для stop системных критичных сервисов (nginx/postgres).

### 10.3 Права
- User units: `systemctl --user ...` от storage напрямую.
- System units: через `sudo systemctl ...` с NOPASSWD по белому списку в `/etc/sudoers.d/storage-control`:
  ```
  storage ALL=(root) NOPASSWD: /bin/systemctl status *, /bin/systemctl is-active *, ...
  storage ALL=(root) NOPASSWD: /bin/systemctl start nginx, /bin/systemctl stop nginx, ...
  ```
  (аккуратно перечислить команды; избегать wildcard на destructive операциях или принимать риск для single-admin).
- Альтернатива: D-Bus polkit rules. Рекомендовать sudoers для простоты.

---

## 11. Аутентификация и безопасность

- Flask-Login, один локальный admin; пароль задаётся при установке (`install.sh` interactive или `STORAGE_ADMIN_PASSWORD` env), хэш argon2.
- Сессии cookie: HttpOnly, Secure (за nginx TLS), SameSite=Lax, срок жизни 8h.
- CSRF (Flask-WTF) для всех POST.
- Привязка к 127.0.0.1:8765; внешний доступ только через nginx TLS (новый server block control.<DOMAIN> или отдельный порт). Для setup добавим nginx site и cert SAN? Текущий cert покрывает photo/drive. Нужно расширить SAN до control.<DOMAIN> или использовать отдельный cert. План: certbot --expand -d photo... -d drive... -d control... (требует DNS A-record).
- Audit_log всех mutating действий (кто, когда, что, IP).
- Секреты не в git; `.secrets.env` chmod 600.
- subprocess: без shell=True где возможно; массив аргументов; валидация имён unit-ов по regex `^[a-zA-Z0-9_.@-]+\.?(service|timer|socket)$`.

---

## 12. Технические требования к задачам (runs)

- Все долгие операции (обновление, бэкап, restore) выполняются вне request-цикла:
  - Вариант A: фоновый thread внутри процесса + состояние в БД.
  - Вариант B: `subprocess.Popen` + tail лога. Предпочтительнее B для изоляции и убиваемости.
- Прогресс для UI: polling `/api/runs/<id>` (статус + tail last N lines + exit code).
- Идемпотентность/блокировки: flock на run-kind.

---

## 13. Планирование фаз (milestones)

**Фаза 0 — Bootstrap (1–2 дня)**
- Создать репозиторий/папку, venv, requirements, Flask factory, конфиг, SQLite, Alembic, auth, базовый layout.
- install.sh скелет: создание юнита, каталогов данных.
- Выдать `tools`/scripts.

**Фаза 1 — Дашборд здоровья (2–3 дня)**
- collectors system/raid/smart/services.
- templates dashboard widgets + polling JS.
- API endpoints.
- history sampling (optional v1.1).

**Фаза 2 — Управление сервисами (2 дня)**
- service catalogue, systemctl wrapper (user/system), bulk actions, logs view.
- sudoers rules.

**Фаза 3 — Бэкапы (2–3 дня)**
- models, CRUD UI, listing/verify/download/delete, APScheduler integration, retention job.
- (Опц) port bash backup to Python.

**Фаза 4 — Планировщик (2 дня)**
- tasks UI, system timers read-only, custom jobs, run history.

**Фаза 5 — Обновления (3–4 дня)**
- Immich runner (git/backup/build/migrate/restart/smoke).
- Nextcloud runner.
- progress UI + rollback helpers.

**Фаза 6 — Hardening & deploy (1–2 дня)**
- nginx site + cert expand, auth, audit, backups of app state.
- systemd user unit, linger enable, autostart.
- docs update in doc/system.md (новый раздел).

Итого: ~13–16 рабочих дней.

---

## 14. systemd unit (user) — черновик

`~/.config/systemd/user/storage-control.service`:
```ini
[Unit]
Description=Storage Control (Flask)
After=network.target

[Service]
Type=simple
WorkingDirectory=/home/storage/storage-control
EnvironmentFile=/home/storage/.config/storage-control/secrets.env
ExecStart=/home/storage/storage-control/.venv/bin/gunicorn -w 1 -k gthread --threads 4 -b 127.0.0.1:8765 wsgi:app
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```
- Включить linger: `loginctl enable-linger storage`.
- Установка: `systemctl --user daemon-reload && systemctl --user enable --now storage-control`.

---

## 15. nginx блок (черновик)

Добавить в `/etc/nginx/sites-available/proxy-sites` (или отдельный файл):
```nginx
server {
    listen 8080 ssl;
    server_name control.<DOMAIN>;
    ssl_certificate     /etc/letsencrypt/live/photo.<DOMAIN>/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/photo.<DOMAIN>/privkey.pem;
    location / {
        proxy_pass http://127.0.0.1:8765;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Proto https;
        proxy_read_timeout 600s;   # для долгих операций (лучше SSE/polling)
    }
}
```
Плюс certbot --expand для SAN control.<DOMAIN>.

---

## 16. Риски и митигация

- **Обновления ломают систему.** Митигация: pre-update DB+config backups, dry-run git diff, smoke tests, documented rollback. Ручное подтверждение major upgrades.
- **Недостаток прав на system units.** Митигация: sudoers whitelist + UI warnings; возможность запуска приложения под root? Нет — остаёмся storage + sudo.
- **Длинные сборки блокируют UI / таймауты.** Митигация: фоновые subprocess + polling; nginx proxy_read_timeout.
- **Точность команд миграций зависит от версии.** Митигация: вынести команды в config, логировать вывод, allow override. Перед первой реализацией проверить актуальные команды (`grep scripts package.json`, `php occ list`).
- **Целостность SMART/temp collections.** psutil может не отдавать fan/temp на некоторых машинах — fallback на `sensors` CLI; деградировать до "н/д".
- **SQLite конкурентный доступ.** WAL mode + один worker gunicorn (или threads) ограничивает; фоновые задачи в отдельных процессах пишут через свою сессию.

---

## 17. Открытые вопросы (требуют уточнения перед кодом)

1. Точная команда миграций Immich (npm scripts / автозапуск при старте сервера) — проверить `package.json` scripts.
2. Nextcloud build deps: composer/npm или `make` — проверить дерево `~/nextcloud`.
3. Добавлять ли домен `control.<DOMAIN>` в DNS и cert SAN — владелец.
4. Нужен ли email/webhook при завершении длинных операций — v1 manual polling достаточно?
5. Хранить ли историю метрик (CPU/RAM) в v1 — если да, добавить sampler + retention.
6. Restore мастер в v1 или v1.1.

---

## 18. Критерии готовности (Definition of Done)

- Приложение запускается как user service, автостарт при reboot.
- Дашборд показывает real-time: сервисы, CPU/RAM/диски/температуры/fans/RAID/SMART.
- Обновление immich и nextcloud выполняется end-to-end через UI с бэкапом БД до миграции и логами.
- Бэкапы: CRUD задач, список/скачивание/удаление файлов, автоплан.
- Планировщик: просмотр/управление系統ными и собственными задачами.
- Сервисы: start/stop/restart одиночно и группами с зависимостями.
- Все mutating действия пишутся в audit_log; ошибки не роняют процесс.

---

*План создан на основе doc/system.md. Реализацию начинать с Фазы 0.*

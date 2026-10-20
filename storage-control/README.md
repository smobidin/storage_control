# Storage Control

Веб-консоль мониторинга и управления для NAS (**Immich + Nextcloud + Stalwart**), проект `storage-control`.

Стек: Python 3.14, Flask 3.1, SQLAlchemy (SQLite), APScheduler, psutil, gunicorn, systemd user service.

## Назначение

- Дашборд здоровья: CPU, память, диски, температуры, RAID, **SMART (baseline tracking, history)**, индикаторы health Immich/Nextcloud/Stalwart, статусы сервисов.
- Обновления Immich, Nextcloud, **Stalwart**: `git fetch/checkout`, сборка, миграции БД, перезапуск, smoke-тесты.
- Резервные копии: CRUD задач, запуск по расписанию, просмотр/скачивание/проверка/удаление файлов.
- Планировщик: системные/user таймеры + собственные shell-задачи.
- Сервисы: start/stop/restart по одному и группами с зависимостями.

Основные источники архитектуры: `doc/pnan.md`, `doc/system.md`.

## Структура

```text
storage-control/
  app/
    __init__.py          # create_app(), bootstrap admin, scheduler
    config.py            # пути, STORAGE_* , SERVICE_CATALOG
    extensions.py        # db, login_manager, csrf, migrate
    models.py            # SQLAlchemy модели (SmartDiskState, SmartAlert, SmartDiskCounterHistory)
    collectors/           # system, raid, smart, services
    auth/, dashboard/, services/, backups/, planner/, updates/
    runners/             # backup/update long-running jobs, smart_monitor.py
    templates/, static/   # favicon.svg, site.webmanifest
  migrations/            # Alembic миграции
  requirements.txt
  wsgi.py
  scripts/              # full install, sudoers, nginx
  README.md
```

## Конфигурация и переменные среды

Переменные можно задать в `.env` **корня репозитория** (загружается автоматически `app/config.py`), либо в Shell. README-инструкция в `storage-control/` раньше указывала неверный путь.

| Переменная | Назначение |
|---|---|
| `SECRET_KEY` | секрет Flask |
| `ADMIN_PASSWORD` | пароль admin (только при наличии создаётся admin) |
| `ADMIN_USERNAME` | имя администратора, по умолчанию `admin` |
| `STORAGE_HOME` | домашний каталог storage, по умолчанию `/home/storage` |
| `STORAGE_CONTROL_DATA` | каталог данных/БД, по умолчанию `~/.local/share/storage-control` |
| `ENABLE_SCHEDULER` | `true/false` |

Данные приложения:
- БД SQLite: `$STORAGE_CONTROL_DATA/app.db`.
- Логи запусков: `$STORAGE_CONTROL_DATA/runs/`.

## Запуск

Разработка:
```bash
cd storage-control
python3 -m venv .venv
.venv/bin/pip install -r requirements.txt
SECRET_KEY=<тут_суть_секрета> ADMIN_PASSWORD=<тут_суть_секрета> ENABLE_SCHEDULER=true \
  .venv/bin/python -c "from wsgi import app; app.run(host='0.0.0.0', port=8765)"
```

Прод:
```bash
gunicorn -w 1 -k gthread --threads 4 -b 0.0.0.0:8765 wsgi:app
```

В проде используется один worker (долгие задачи в фоновых потоках).

## Установка как user systemd service

Полная установка: `scripts/install.sh`. Он ставит системные пакеты, venv и
sudoers, а установку самого user-сервиса передаёт в `../tools/install.sh`.

Только пользовательский сервис, без системных шагов:
`../tools/install.sh` (удаление — `../tools/uninstall.sh`). Подробности и
опции — в `../tools/README.md`.

Важные шаги вручную:
1. Создать venv и установить зависимости.
2. Заполнить `~/.config/storage-control/secrets.env`.
3. Установить sudoers: `sudo cp scripts/sudoers-storage-control /etc/sudoers.d/storage-control`.
4. Установить и запустить сервис: `../tools/install.sh`.

`ENABLE_SCHEDULER=true` по умолчанию.

## Маршруты и API

- `/` — дашборд.
- `/services/` — сервисы.
- `/backups/` — задачи и файлы бэкапов.
- `/scheduler/` — планировщик.
- `/updates/` — обновления.
- `/api/health/summary`, `/api/health/services`, `/api/health/smart`, `/api/health/smart/status`
- `/api/health/smart/refresh` — **ручное SMART-сканирование** (POST)
- `/api/health/smart/alert/<id>/resolve` — **resolve алерта с сохранением baseline** (POST)
- `/api/health/smart/history/<device>` — **история счётчиков** (GET)

## Мониторинг

Сбор метрик:
- `collectors/system.py`: CPU, память, swap, физические диски, температуры (`psutil.sensors_temperatures`, fallback `sensors`).
- `collectors/raid.py`: `/proc/mdstat`, `mdadm --detail /dev/md0`.
- `collectors/smart.py`: динамический перебор физических block-устройств `/sys/block` с установкой `smartctl -a`.
- `runners/smart_monitor.py`: nightly SMART-проверка (03:30 via timer), `SmartDiskState`/`SmartAlert`/`SmartDiskCounterHistory`, алерты только при **росте счётчиков** относительно baseline.
- `collectors/services.py`: `systemctl is-active/is-enabled/show` (user + system scope).

**SMART Dashboard (новая логика):**
- Таблица с колонками: Device, Model, Health, Temp, Reallocated, Pending, Offline, ATA Errors, Baseline
- **Жирный шрифт** для счётчиков, выросших относительно baseline
- Baseline сохраняется при нажатии "Resolve" на алерте
- Кнопка **"Refresh SMART Info"** — ручной прогон (источник `manual`)
- История измерений: `/api/health/smart/history/<device>` (последние 100, с пометкой `auto`/`manual`)
- Ночной скан не создаёт новый алерт, если счётчики не выросли

Проверка дашборда:
```bash
curl -s http://127.0.0.1:8765/api/health/summary | head
```

## Обновления

### Immich
- pre-update бэкап БД и конфигов;
- `pnpm`/`node` берутся из `~/immich/node/bin` (corepack): раннер добавляет эту директорию в PATH шагов установки/сборки, т.к. в PATH сервиса её нет;
- `git fetch --tags`, `git checkout <tag>`;
- `pnpm install --frozen-lockfile`;
- `pnpm run build` в `server` и `web`;
- сборка core-плагина (`@immich/plugin-core` → wasm через extism-js + wasm-merge из `~/immich/tools`) и установка в `~/immich/build/plugins/immich-plugin-core` — нефатально, при отсутствии инструментов только WARN;
- `pnpm run migrations:run` с `DB_URL` из `~/immich/.env`;
- restart `immich-ml`, `immich-server`;
- smoke `curl http://127.0.0.1:2283/api/server/ping` (с ретраями: серверу нужно несколько секунд после restart; `/api/health` в новых версиях immich убран).

### Nextcloud
- pre-update бэкап БД и `config/config.php`;
- maintenance mode on;
- `git fetch/checkout`;
- `make build-js-production`;
- `php occ upgrade`;
- `integrity:check-core`, `db:add-missing-indices`;
- maintenance mode off;
- restart `php8.5-fpm`, `nginx`.

### Stalwart
- pre-update бэкап бинарника (`/mnt/backup/backups/stalwart/<run_id>/stalwart.bak`);
- скачивание в `/tmp/stalwart-update-<run_id>_*` (auto-clean);
- версия: `~/stalwart/bin/stalwart --version`;
- замена бинарника через `copy2` + `os.replace` (workaround cross-device `/tmp` → `/home`);
- smoke: `curl http://127.0.0.1:8082/admin/` (admin UI endpoint);
- systemd user unit: `stalwart.service` (RequiresMountsFor=/mnt/data/stalwart).

Запуск выполняется фоном (`async_call`), прогресс polled через `/updates/api/runs/<id>`.
GitHub Releases проверяются через `/updates/api/availability` (кэш ~5 мин).
Пункты `Preserve`: prepare создает бэкапы и git stash, restore откатывает код/БД/конфиг, опциональное удаление после проверки.

## Бэкапы

- Таблица `backup_tasks`: имя, тип, cron, команда, enabled, retention.
- Типы: `immich_db`, `nextcloud_db`, `stalwart_db`, `immich_config`, `nextcloud_config`, `stalwart_config`, `all`, `custom`.
- `BackupRun` хранит статус, файлы, лог, exit code.
- Retention очищает старые файлы `.dump/.tar.gz/.txt` каждые сутки в 02:30.
- Файлы сканируются рекурсивно в `/mnt/backup/backups`.

## Планировщик

- `/scheduler/` показывает APScheduler jobs и системные/user таймеры.
- Пользовательские shell-задачи запускаются через cron, состояние в `scheduled_jobs`.
- Таймеры можно enable/disable/start/stop (через sudo для system scope).

## Сервисы

Сервисы описаны в `collectors/services.py` (`SERVICE_CATALOG`). Поддерживаются единичные действия и групповые (`immich`, `nextcloud`, `stalwart`, `infra`, `backup`).

Права описаны в `scripts/sudoers-storage-control` (whitelist `systemctl` и `smartctl`).

## Логи и отладка

- Gunicorn: stdout/stderr в `/tmp/opencode/startup.log`.
- При 500 смотреть stack trace в логах.
- Проверка импорта:
  ```bash
  .venv/bin/python -m compileall -q .
  ```
- Типичные ошибки:
  - дубль `<style>` или незакрытый `{% for %}` в Jinja;
  - отсутствие `ADMIN_PASSWORD` (admin не создастся);
  - sudoers не установлен;
  - stray-токен `PY` в конце Python-файлов (не коммитить).

## Внешний доступ

nginx-блок: `scripts/nginx-control-site.conf`. Для вашего домена (например `control.example.com`) расширить SAN сертификата Let's Encrypt.

## Безопасность

- Flask-Login, argon2, CSRF.
- Приложение слушает интерфейс; вне доверенных сетей использовать reverse proxy TLS.
- Аудит мутаций в `audit_log`.
- Секреты в `~/.config/storage-control/secrets.env` (chmod 600).
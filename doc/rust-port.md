# План: storage-control на Rust (перенос знаний, не кода)

Статус: план v2, утверждён. Реализация не начата.
Ветка: `go-to-rust` (от `main`).
Заменяет: `storage-control/` (Flask + SQLAlchemy + APScheduler + gunicorn).
Предыдущая версия плана (транслитерация 1:1) отклонена: обратная совместимость не нужна.

## 1. Подход

Переписать на Rust **с нуля по архитектуре**, перенеся из Python то, что является
знанием, а не реализацией:

- **переносим дословно по смыслу**: операционные инварианты раннеров (порядок команд,
  флаги, обходные пути — §5), потому что они выстраданы в проде;
- **переписываем свободно**: роутинг, шаблоны, модели данных, JSON-DTO, планировщик,
  конфиг, обработку ошибок;
- **сохраняем как внешние контракты**: argv для sudoers, пути, env-переменные, URL;
- **не сохраняем**: формат Flask-сессий и CSRF, Jinja-шимы, точные JSON-ключи и HTML
  байт-в-байт, bug-for-bug поведение, `alembic_version`, `apscheduler_jobs`.

Данные переносятся одноразовым импортером в **новую, чистую схему**.

Приёмка — не диффом с Flask, а тестами на фикстурах реального вывода + чек-листом
инвариантов §5 + реальным обновлением каждого сервиса.

## 2. Три яруса совместимости

### 2.1 Сохраняем (внешние контракты, менять дорого)

| Что | Почему |
|---|---|
| `sudo systemctl …`, `sudo smartctl -a *`, `sudo mdadm --detail *` | правила в `/etc/sudoers` (root), `scripts/sudoers-storage-control` |
| `journalctl --user -u` / `sudo journalctl -u` | тот же whitelist и та же семантика scope |
| Пути `/mnt/backup/backups`, `/mnt/data`, `/tmp` | дисковая дисциплина, systemd-юниты, скрипты бэкапов |
| `$STORAGE_CONTROL_DATA`, `$STORAGE_HOME`, `STORAGE_CONTROL_DATA/runs/*.log` | логи и артефакты ищутся по этим путям в документации |
| env: `SECRET_KEY`, `ADMIN_USERNAME`, `ADMIN_PASSWORD`, `STORAGE_HOME`, `STORAGE_CONTROL_DATA`, `ENABLE_SCHEDULER` | `~/.config/storage-control/secrets.env` + user-юнит |
| URL-пути страниц и API | закладки, PWA-манифест, внешние ссылки в документации |
| `pg_dump`/`pg_restore` строки подключения (`postgresql://storage@localhost:5432/immich`, `…nextcloud_db…`) | совместимость с `tools/backup_immich_nc.sh` и восстановлением |

### 2.2 Переписываем свободно

- шаблонизатор: Jinja2 → **askama** (типизированный контекст, проверка на компиляции);
- HTML-разметка и CSS остаются (это не legacy, а UI), но inline-JS переезжает в
  `static/*.js`, а данные приходят из JSON-DTO, спроектированных заново;
- модели данных и схема SQLite (§6);
- JSON-ключи ответов (`/api/*`);
- формат сессионной cookie и CSRF-токена (свой формат; argon2-хэши читаются);
- планировщик (cron-реестр в памяти вместо APScheduler и его таблицы);
- конфиг: env + явный `--env-file`, без автозагрузки `.env` из корня репозитория;
- тексты ошибок/исключений в логах (но не имена шагов — они диагностические маркеры
  в AGENTS.md, см. §2.3);
- обработка ошибок: `Result` + один тип `AppError` вместо исключений и `try/except`
  на каждом шаге.

### 2.3 Переносим дословно (инварианты)

Имена шагов раннеров (`backup_db`, `git_checkout`, `viewer_sync`, …), маркеры
`=== name ===` / `FAILED:` / `PREPARE OK` в логах, порядок шагов, поведение при
сбое (что фатально, что нет, что откатывается) — §5.

## 3. Что есть сейчас (инвентаризация)

### 3.1 Маршруты (38)

```
auth      GET|POST /login                 POST /logout
dashboard GET /  /help
          GET /api/health/summary  /api/health/smart  /api/health/services
          GET /api/health/smart/status  /api/health/smart/history/<device>
          POST /api/health/smart/refresh  /api/health/smart/alert/<id>/resolve
services  GET /services/  /services/logs
          POST /services/manage  /services/group
backups   GET /backups/  /backups/files/download
          POST /backups/tasks  /backups/tasks/<tid>/{update,delete,run}
          POST /backups/files/{delete,verify}
scheduler GET /scheduler/  /scheduler/unit/<scope>/<unit>
          POST /scheduler/jobs  /scheduler/jobs/<jid>/{run,delete}  /scheduler/timer/toggle
updates   GET /updates/  /updates/api/runs/<rid>  /updates/api/availability
          GET /updates/preserve/<pid>/log  /updates/preserve/<pid>/api
          POST /updates/{immich,nextcloud,stalwart}/run
          POST /updates/preserve/prepare  /updates/preserve/<pid>/{restore,delete}
```

### 3.2 Данные в живой базе (на 2026-10-23, 140 КБ)

| Таблица | Строк | Ценность |
|---|---|---|
| `smart_disk_counter_history` | 183 | история счётчиков 5 дисков |
| `smart_alert` | 19 | история алертов (в т.ч. resolved) |
| `smart_disk_state` | 5 | у `/dev/sdd`, `/dev/sde` заполнены `resolved_counters` — базовые линии |
| `update_preserve` | 3 «ready» | на диске **177 МБ** снапшотов в `/mnt/backup/backups/preserves`, известны только БД |
| `update_run` | 11 | история обновлений |
| `backup_task` | 5 | задачи (все без cron, запуск ручной) |
| `audit_log` | 22 | журнал действий |
| `user` | 1 | argon2-хэш (переносится как есть) |
| `alembic_version`, `apscheduler_jobs` | 1 / 2 | **не переносятся** |

### 3.3 Коллекторы

- `system.py` — temps (psutil → hwmon, фолбэк `sensors -j`), диски (фильтр loop/virtual/не-`/dev/`),
  CPU `percent(interval=None)` + per-core, freq, loadavg, memory/swap, uptime, hostname, os.
- `raid.py` — `/proc/mdstat` + `sudo mdadm --detail /dev/md0`.
- `smart.py` — устройства из `/sys/block`, `sudo smartctl -a`, regex-парсинг.
- `services.py` — каталог 12 юнитов (scope/group/order/critical/desc), `_ctl()`,
  спецлогика oneshot, `LastTriggerUSec`, `NextEligibleTimestamp`, group-actions по `order`.

### 3.4 Раннеры, бэкапы, планировщик

`update_immich.py` (12 шагов), `update_nextcloud.py` (14 шагов + viewer_sync),
`update_stalwart.py` (8 шагов), `preserve.py` (prepare/restore/delete), `backup_task.py`
(6 видов задач + retention), `smart_monitor.py` (baseline/history/alerts).
Cron-джобы: `retention_cleanup` 02:30, `smart_nightly_check` 01:00, `backup_task_<id>`,
`sched_job_<id>`; всё в UTC.

## 4. Стек

| Роль | Выбор |
|---|---|
| HTTP | axum 0.8 + tokio, tower/tower-http |
| Шаблоны | **askama** (compile-time, `{% extends %}`/`{% block %}`), встроены в бинарник |
| БД | **rusqlite (bundled)**, чистый SQL, новая схема, отдельный импортер |
| Auth | `argon2` (PHC-совместим с argon2-cffi), своя подписанная cookie (hmac+sha2), CSRF = HMAC от id сессии |
| Даты | `chrono`; хранение — `INTEGER` unix-millis UTC, форматирование для UI — в Rust |
| JSON | `serde` + отдельные DTO |
| Cron | `croner` (5-польные выражения, `next_run` для UI) |
| Система | `sysinfo` + `/proc` + `/sys/class/hwmon` + `libc::statvfs` |
| HTTP-клиент | `reqwest` (rustls) для GitHub Releases |
| Markdown | `pulldown-cmark` (`/help`) |
| Логи | `tracing` + `tracing-subscriber` в journald |

Архивы (`tar`/`curl`) — внешними командами, как сейчас: это дешевле и совпадает с
документированными процедурами. `tar`/`bzip2`/`flate2` в зависимостях не нужны.

## 5. Инварианты, которые обязаны пережить переписывание

Это ядро плана: каждый пункт — причина, по которой что-то сломалось в прошлом.

### 5.1 Immich (`update_immich`)

1. `backup_db`: `pg_dump --dbname=postgresql://storage@localhost:5432/immich -Fc -f
   /mnt/backup/backups/immich/immich_db_preupdate_<ts>.dump`, `ts=%Y-%m-%d_%H%M`. Фатально.
2. `backup_config`: `tar czf …/immich_config_preupdate_<ts>.tar.gz -C $STORAGE_HOME immich/.env`. Фатально.
3. `git_fetch`: `git fetch origin --tags` в `~/immich/src`. Фатально.
4. Перед checkout: залогировать грязные tracked-файлы (`git status --porcelain`, без `??`),
   снять копии файлов, которые трогают локальные патчи, в `mkdtemp` **на /tmp**.
5. `reset_patched_files`: `git checkout -- <paths>` для файлов патча. Фатально.
6. `git_checkout <tag>`; при ошибке — восстановить копии из п.4. Фатально.
7. `apply_local_patches` (не фатально): `git apply --reverse --check` → уже применён;
   `git apply --check` → apply; иначе `git apply --3way`; при конфликте `git reset --hard HEAD`.
   Пустой каталог патчей — нормальный случай, лог «no local patches».
8. `pnpm_version`: `pnpm --version` + сравнение с `packageManager` из `package.json`.
   Только WARN, не фатально. Env: `~/immich/node/bin` первым в PATH,
   `COREPACK_ENABLE_DOWNLOAD_PROMPT=0`, `NPM_CONFIG_UPDATE_NOTIFIER=false`.
9. `pnpm_install`: `pnpm install --frozen-lockfile` (1800 с). Фатально.
10. `build_server` → `pnpm run build` в `server/`; `build_web` → то же в `web/`. Оба фатальны.
11. `build_plugin_core` (не фатально): `pnpm --filter @immich/sdk --filter @immich/plugin-sdk
    --filter @immich/plugin-core build`, затем копирование `packages/plugin-core/{manifest.json,dist}`
    в `~/immich/build/plugins/immich-plugin-core/`. PATH дополняется
    `tools/extism-js` и `tools/binaryen/binaryen-*/bin`.
12. `migrations`: `pnpm run migrations:run` в `server/` с env = tool env + ключи из
    `~/immich/.env` (1800 с). Фатально.
13. `restart_services`: `systemctl --user restart immich-ml.service immich-server.service`.
14. `smoke_test`: polling до 180 с каждые 5 с по `http://127.0.0.1:2283/api/server/ping`,
    затем `/api/health`. Именно polling: `systemctl restart` возвращается раньше, чем API
    забиндит порт. `/api/health` пробуется вторым: он отдаёт 200 HTML из SPA-джаббера.

### 5.2 Nextcloud (`update_nextcloud`)

1. `backup_db` (`nextcloud_db`), `backup_config` (`config/config.php`) — фатально.
2. `maintenance_on`: `php occ maintenance:mode --on`. Фатально.
3. `git_fetch`: `git fetch origin --tags --no-recurse-submodules` (без рекурсии — иначе
   падает). Фатально.
4. `git_checkout <tag>` — фатально.
5. `git_submodules`: `git submodule update --init --recursive` (1800 с). Обязательно:
   иначе `3rdparty` остаётся старым и `occ upgrade` падает на Symfony. Фатально.
6. `build_js`: `make build-js-production` (1800 с). **Не** фатально (`npm` может отсутствовать,
   `dist` в релизе уже собран).
7. `occ_upgrade`: `php occ upgrade` (1800 с). При rc≠0 — прерывание (даже если это «stale
   app references»), с выключением maintenance в обработчике ошибки.
8. `verify_upgrade`: `occ status`, парсинг полей: `installed=true`, `needsDbUpgrade=false`,
   `versionstring == tag без 'v'`. maintenance здесь не проверяется (она ещё включена).
9. `integrity_check` (`occ integrity:check-core`) и `add_missing_indices` — не фатальны.
10. `maintenance_off` — фатально. `restart_services`: `sudo systemctl restart php8.5-fpm nginx`.
11. `viewer_sync` — фатально: `apps/*` игнорируется git'ом, поэтому `viewer` не обновляется
    и на мажорном переходе отключается, после чего офисные файлы скачиваются вместо
    открытия в Collabora. Логика: прочитать `<nextcloud max-version>` из
    `apps/viewer/appinfo/info.xml`; если покрывает — выйти; иначе скачать
    `https://download.nextcloud.com/server/releases/nextcloud-<ver>.tar.bz2`
    (`curl -sSfL --retry 3 --connect-timeout 30`), распаковать только `nextcloud/apps/viewer`
    в staging `$RUNS_DIR/viewer_stage` (`--strip-components=1`), проверить max-version,
    подменить каталог, `occ app:enable viewer`, проверить в `occ app:list --enabled`,
    удалить архив (~250 МБ).
12. `smoke_test`: `occ status` + парсинг (уже с проверкой maintenance). `rc=0` не означает
    успех: если в выводе `An unhandled exception has been thrown` или
    `Traceback (most recent call last)` — шаг считается проваленным.

### 5.3 Stalwart (`update_stalwart`)

1. `stop_service`: `systemctl --user stop stalwart.service`.
2. `backup_binary`: копия в `/mnt/backup/backups/stalwart/<run_id>/stalwart.bak` (это артефакт
   бэкапа, а не staging).
3. Staging только в `/tmp/stalwart-update-<run_id>_*`; удаляется и при успехе, и при сбое.
4. `download`: `https://github.com/stalwartlabs/stalwart/releases/download/<tag>/stalwart-x86_64-unknown-linux-gnu.tar.gz`.
5. `extract` + поиск бинарника, `replace_binary`: копия в `stalwart.new` рядом с целевым
   файлом (кросс-девайс!), `chmod 755`, затем rename в целевой путь.
6. `start_service`, при rc≠0 — фатально; в любом обработчике ошибки сервис принудительно
   поднимается.
7. `smoke`: `http://127.0.0.1:8082/admin/`, до 30 попыток по 2 с, принимать 200/301/302/303.

### 5.4 Preserve

- prepare: при грязном дереве `git stash push -u -m "storage-control preserve <id>"` и
  ссылка `refs/preserve/<id>`; дамп БД; архив конфига; копии `.env` / `config.php` +
  `.htaccess`; для Stalwart — остановка сервиса на время `tar` данных и запуск обратно.
- restore: только из `ready|failed|restored`; Nextcloud — maintenance on → конфиги →
  `git checkout <git_before>` → `stash apply` → `pg_restore --clean --if-exists` →
  maintenance off → рестарт; Immich — стоп сервисов → restore → `.env` → checkout →
  stash apply → старт; Stalwart — стоп → распаковка данных → старт.
- delete: удаление каталога + строки.

### 5.5 Бэкапы и retention

- Виды: `immich_db`, `nextcloud_db`, `immich_config`, `nextcloud_config`, `all`
  (`tools/backup_immich_nc.sh`), `custom` (`bash -lc <command>`).
- **Исправить:** в Python `stalwart_db` и `stalwart_config` объявлены в UI, но в
  `execute_backup_task` отсутствуют (падают с «unknown kind»). В Rust реализовать:
  RocksDB-данные тарболом (с остановкой сервиса на время снятия копии) + конфиг.
- Retention: не заходить в `home-storage` и `preserves`; удалять только `.dump`,
  `.tar.gz`, `.txt` старше `retention_days`; один обход дерева на прогон, а не на задачу.
- Листинг файлов: глубина 2, лимит 500, сортировка по mtime desc, `safe_path` через
  каноникализацию и проверку префикса.

### 5.6 SMART

- Сравнение с `resolved_counters` (базовые линии): алерт только если счётчик вырос или
  базовой линии ещё нет; при наличии базовой линии и отсутствии роста — не алертить.
- `severity=critical` при провале health или росте любого счётчика, иначе `warning`.
- Каждый прогон: снять старые активные алерты, записать историю (`source=auto|manual`),
  обновить состояние.
- Порог температуры: ≥60 °C critical, ≥50 °C high.

### 5.7 Сервисы и планировщик

- `display_status` для oneshot: `success` при `Result=success`/`ExecMainStatus=0`,
  `failed` при `failure`/непустом не-0 статусе, `never-run` при отсутствии
  `ActiveEnterTimestamp`; `last_run` = `ActiveExitTimestamp` → `ActiveEnterTimestamp` → `LastTriggerUSec`.
- Group-actions: start — по возрастанию `order`, остальные — по убыванию.
- Cron в UTC; id джоб те же; `ENABLE_SCHEDULER=false` глушит всё; ресинхронизация при
  создании/изменении/удалении задач.
- Блокировка «одна операция на target» + проверка незавершённой записи в БД; зависшая
  `running`-строка после падения процесса остаётся известной проблемой и документируется.

## 6. Новая схема БД

Файл: `$STORAGE_CONTROL_DATA/storage-control.db` (старый `app.db` не трогаем).

Отличия от текущей:

- все метки времени — `INTEGER` unix-millis UTC (никаких naive-дат текстом);
- `CHECK` на статусы (`pending|running|success|failed`, `pending|preparing|ready|restoring|restored|failed`);
- FK `backup_run.task_id → backup_task.id` с `ON DELETE SET NULL`, `PRAGMA foreign_keys=ON`;
- уникальные индексы там, где они уже де-факто есть (`smart_disk.device`, `user.username`);
- JSON-поля (`steps`, `files`, `resolved_counters`, `alert_counters`, `extra_files`) остаются
  текстом: это непрозрачные для БД структуры, но с валидацией при чтении;
- `alembic_version` и `apscheduler_jobs` не создаются.

### Импортер

`storage-control import --from <app.db> --to <storage-control.db>`:

1. читает старую базу в read-only режиме, ничего не пишет и не удаляет;
2. переносит: `user`, `audit_log`, `update_run`, `backup_task`, `backup_run`,
   `scheduled_job`, `update_preserve`, `smart_disk_state`, `smart_disk_counter_history`,
   `smart_alert`;
3. даты парсит как naive UTC → millis; битые значения — в отчёт, не падает;
4. после импорта сканирует `/mnt/backup/backups/preserves/*/*` и добавляет `preserve`-строки
   для каталогов без записи (защита от осиротевших 177 МБ);
5. печатает сводку (сколько строк по таблицам, что пропущено) и пишет `import-report.txt`
   рядом с новой базой;
6. отказывается работать, если целевая база не пуста (кроме `--force`).

Приёмка импортера: тест на фикстурной копии старой схемы (DDL + образцы строк) —
roundtrip без потерь, базовые линии SMART на месте, 3 preserve привязаны.

## 7. Структура крейта

```
storage-control-rs/
  Cargo.toml
  src/main.rs                 # CLI: serve | import | smart-check | retention | backup | version
  src/config.rs               # env + --env-file; пути и дефолты как сегодня
  src/db/mod.rs               # пул, PRAGMA, миграции схемы (idempotent)
  src/db/schema.sql           # новая DDL
  src/db/models.rs            # типы строк
  src/db/repo/{user,audit,backup,updates,preserve,smart,jobs}.rs
  src/importer.rs             # импорт из старой app.db + добор preserve-каталогов
  src/web/mod.rs              # роутер axum
  src/web/{session,csrf,error,flash}.rs
  src/web/routes/{auth,dashboard,services,backups,scheduler,updates}.rs
  src/web/dto.rs              # JSON-DTO
  src/collect/{system,services,raid,smart}.rs
  src/versions.rs             # git describe/tags, Releases API + кэш 5 мин, migration_steps
  src/runners/{common,backup_task,preserve,update_immich,update_nextcloud,update_stalwart,smart_monitor}.rs
  src/scheduler.rs            # cron-реестр + ресинхронизация
  templates/*.html            # askama
  static/{favicon.svg,site.webmanifest,dashboard.js,updates.js,scheduler.js}
  tests/fixtures/             # реальные выводы команд (снимаются с живого хоста)
  tests/{parsers,runners,importer,versions,cron}.rs
tools/capture-fixtures.sh     # снять фикстуры, пока Flask жив
```

Правила:

- `runners/*.rs` — по модулю на раннер, по функции на шаг, порядок и имена шагов из §5;
- `runners/common.rs`: `run_cmd` с объединением stdout+stderr, лог-файлом, таймаутом
  (rc 124) и ошибкой запуска (rc 1), плюс `log_write` с теми же маркерами;
- `web/routes/*.rs` — по модулю на раздел, URL-пути из §3.1;
- ошибки в одном `AppError` (404/400/500 + сообщение), без разбросанных `try/except`.

## 8. Фазы

### Фаза 0. Фикстуры (0.5 дня)

`tools/capture-fixtures.sh` снимает с живого хоста: `smartctl -a` по всем дискам,
`/proc/mdstat`, `sudo mdadm --detail /dev/md0`, `systemctl show` по всем 12 юнитам,
`systemctl --user list-timers`, `journalctl -n 300`, `occ status`, `git describe` для
immich/nextcloud, ответ GitHub Releases (по одному на репозиторий), `.schema` старой базы
и образцы строк. Плюс копия `app.db` в `tests/fixtures/legacy/` (140 КБ, без секретов —
там только хэш админа; в git не коммитить, добавить в `.gitignore`).

**Приёмка:** фикстуры воспроизводимы, парсеры можно тестировать без хоста.

### Фаза 1. Каркас (1.5 дня)

config, пул SQLite, `schema.sql`, `AppError`, сессия/CSRF/flash, askama-контекст,
`/login` + `/logout` (argon2), базовый layout, 404, JSON-DTO и клиентские `static/*.js`.

**Приёмка:** логин существующим админом (проверка на импортированном хэше), все страницы
открываются, CSRF отбивает чужой POST, cookie переживает рестарт.

### Фаза 2. Импортер + коллекторы + дашборд (2 дня)

Импортер (§6), system/raid/smart/services, SMART-монитор целиком.

**Приёмка:** на импортированной базе дашборд показывает те же диски/температуры/RAID/сервисы;
`smart_disk_state` содержит базовые линии `/dev/sdd` и `/dev/sde`; ручной SMART-чек пишет
историю с `source=manual`. Тесты парсеров на фикстурах.

### Фаза 3. Services + Scheduler (1.5 дня)

Каталог юнитов, `_ctl`, `display_status`, логи, group-actions, audit, unit-text модалка,
`list-timers`.

**Отклонение (осознанное):** вместо 3–4 вызовов `systemctl` на юнит — один
`systemctl show` на scope для всех юнитов сразу (~48 процессов на запрос → 2).
Логика `display_status` сохраняется, поведение на фикстурах сравнивается со старым.

**Приёмка:** состояние 12 юнитов совпадает с выводом старого приложения на снятых фикстурах;
start/stop/restart/enable/disable работают через тот же sudoers.

### Фаза 4. Backups (1 день)

CRUD, шесть видов задач (включая починенные `stalwart_*`), retention, листинг, download,
delete, verify.

**Приёмка:** реальный прогон `immich_db` и `immich_config` даёт дампы/архивы, которые
принимает `pg_restore --list`; retention не трогает `home-storage` и `preserves`.

### Фаза 5. Updates (2.5–3 дня)

`versions.rs`, три раннера по §5.1–5.3, preserve по §5.4, polling-API и логи.

**Приёмка:** каждый раннер проверен на «сухом» прогоне (недоступный tag/URL): шаги
записаны в правильном порядке, `status=failed`, Nextcloud выходит из maintenance,
Stalwart поднят. Затем — по одному реальному обновлению каждого сервиса на Rust.

### Фаза 6. Планировщик (1 день)

Cron-реестр, ресинхронизация, стартовый SMART-чек, `ENABLE_SCHEDULER=false`.

**Приёмка:** `/scheduler` показывает те же джобы и корректный next_run; тестовая
backup-задача с cron срабатывает; флаг глушит всё.

### Фаза 7. Приёмка и переключение (1 день + обкатка)

1. Rust на `:18080`, `ENABLE_SCHEDULER=false`, отдельная БД (импорт на копии).
2. Чек-лист §5 пройден вручную; тесты зелёные.
3. `tools/storage-control.service` → `ExecStart=…/.local/bin/storage-control serve --bind 0.0.0.0:8765`;
   `tools/install.sh` ставит бинарник (venv/gunicorn не нужны).
4. Импорт боевой базы в `storage-control.db`, старый `app.db` остаётся как страховка.
5. Обкатка ≥1 недели: ночной SMART, retention 02:30, ручной бэкап, одно обновление сервиса.
6. Обновить `AGENTS.md` и `storage-control/README.md`: проверка — `cargo build --release`,
   `cargo test`, `curl -s http://127.0.0.1:8765/login`; описать `import` и CLI.

### Фаза 8. Снос Python (0.5 дня)

Удаление `storage-control/` отдельным коммитом после обкатки (включая `.venv` из
`.gitignore`). Откат до этого момента — вернуть старый юнит; базы независимы.

## 9. Тесты и приёмка

- `cargo test` — парсеры (`smartctl`, `mdadm`, `occ status`, `git describe`), cron → next_run,
  сравнение версий и `migration_steps`, `run_cmd` (rc/таймаут/лог), `safe_path` и листинг,
  импортер на фикстурной базе.
- Интеграционные: раннеры против фейкового окружения (подменённые `PATH`-скрипты для
  `git`/`pnpm`/`systemctl`/`curl`) — проверяют порядок и имена шагов, обработку rc≠0,
  отсутствие повторного запуска на busy-target.
- `cargo clippy -- -D warnings`, `cargo fmt --check` (по желанию — в CI рядом с gitleaks).

## 10. Риски

1. **Потеря операционных знаний.** Главный риск. Снимается §5 + фикстуры + реальный
   прогон каждого раннера до переключения.
2. **Осиротевшие preserve-каталоги.** Снимается добором при импорте и проверкой списка
   в UI после переключения.
3. **Потеря базовых линий SMART.** Проверяется конкретно: `resolved_counters` у `/dev/sdd`
   и `/dev/sde` на месте, повторных алертов на старых секторах нет.
4. **`/tmp` как staging** (tmpfs ~7 ГБ) и кросс-девайс rename для Stalwart — §5.3.
5. **Nextcloud: rc=0 ≠ успех** — маркеры + парсинг `occ status` — §5.2.
6. **pnpm/corepack** — §5.1 п.8: без правильного PATH и переменных сборка виснет или
   ломает пин `packageManager`.
7. **Два планировщика одновременно** — на время обкатки у Rust `ENABLE_SCHEDULER=false`
   и отдельная БД; обновления в этот период запускаются только на одном инстансе.
8. **AppError вместо исключений:** легко потерять «не фатальные» шаги — они перечислены
   в §5 явно и покрыты тестами на порядок.

## 11. Открытые вопросы

1. Добавлять ли CI-workflow с `cargo test`/`clippy` (сейчас только gitleaks).
2. Переименовывать ли раздел `Backups` в UI — нет, оставляем как есть.
3. Нужен ли веб-доступ к истории импорта (`import-report.txt`) — по умолчанию нет, только файл.

## 12. Definition of Done

- [ ] Все 38 маршрутов работают; UI функционально эквивалентен старому.
- [ ] Данные перенесены импортером: SMART-история и базовые линии, preserves (3, 177 МБ),
      update runs, задачи бэкапов, audit, админ.
- [ ] Каждый инвариант §5 покрыт тестом или ручной проверкой.
- [ ] Реальные обновления Immich, Nextcloud, Stalwart и один restore прошли на Rust.
- [ ] Ночной SMART, retention и бэкапы работают по расписанию в новом планировщике.
- [ ] `tools/install.sh` ставит бинарник; venv и gunicorn не нужны.
- [ ] `AGENTS.md`/README обновлены (сборка, тесты, `import`, CLI).
- [ ] Python-дерево удалено отдельным коммитом после обкатки.

## 13. Оценка

~6–8k строк Rust (импортер и новая схема добавляют, отсутствие шимов и batching
коллекторов — экономят). 9–12 рабочих дней с приёмкой и обкаткой.

Самый дорогой участок — фаза 5 (раннеры обновлений), самый рискованный — фаза 2
(данные и цифры дашборда после импорта).

---

## 14. Прогресс реализации

Обновляется по ходу работ; тут только фактическое состояние, план — в §8.

### Сделано

**Фаза 0 — фикстуры.** `tools/capture-fixtures.sh` снимает с живого хоста
`smartctl -a`, `mdadm --detail`, `systemctl show/is-active/is-enabled/list-timers`,
git-теги Immich/Nextcloud, ответы GitHub Releases и копию `app.db`.
Сырьё лежит в `storage-control-rs/tests/fixtures/raw/` и **в git не попадает**
(серийники дисков + копия БД): каталог в `.gitignore`, в репозитории остаются
только обезличенные литералы в тестах.

**Фаза 1 — каркас.** `storage-control-rs/` собирается и проходит smoke-тест:
логин/логаут/CSRF/404/статика/редирект с `/` без сессии.
- `config.rs` — те же переменные (`SECRET_KEY`, `ADMIN_USERNAME`, `ADMIN_PASSWORD`,
  `STORAGE_HOME`, `STORAGE_CONTROL_DATA`, `ENABLE_SCHEDULER`), `.env` только по
  `--env-file`/`STORAGE_CONTROL_ENV_FILE`, реальное окружение приоритетнее.
- `db/` — rusqlite bundled, `storage-control.db`, схема §6, WAL, FK, `CHECK`.
- `security.rs` — argon2id; тест проверяет **реальный хеш argon2-cffi**, то есть
  пароль администратора переносится импортом без сброса.
- `web/session.rs` — одна подписанная HMAC-SHA256 cookie: uid + csrf + flash.
  CSRF проверяется в каждом POST-хендлере (`refresh`/`resolve` — как в Flask, без CSRF).
- askama-шаблоны `base/login/dashboard/help/404/placeholder`, CSS перенесён.

**Фаза 2 — коллекторы, SMART, дашборд, импортёр.**
- `exec.rs` — аналог `subprocess.run(capture_output, timeout)`: два потока-читателя
  (нет дедлока на 64-КБ pipe), таймаут → rc 124, stdin закрыт.
- `collect/smart.rs` — те же регэкспы, что в Python (health, `Temperature_Celsius`,
  четыре атрибута, `ATA Error Count`, `Power_On_Hours`), `raw_tail` 1500 байт по
  границе UTF-8; устройства из `/sys/block` с тем же списком исключений.
- `collect/raid.rs` — `/proc/mdstat` + `sudo mdadm --detail /dev/md0`; разбор
  массива без look-ahead (в Rust-regex его нет), поля mdadm — те же.
- `collect/system.rs` — CPU/память/swap/loadavg/disks через sysinfo; температуры
  читаются из `/sys/class/hwmon` (даёт ту же группировку по чипам, что psutil),
  fallback `sensors -j`; `cached` = `Cached + SReclaimable`.
- `collect/services.rs` — каталог 12 юнитов; **батчинг**: 6 вызовов вместо ~48
  (`show` по всем юнитам сразу, разбор по `Id=`), логика `oneshot`/`display_status`
  и парсинг `LastTriggerUSec` перенесены дословно.
- `runners/smart_monitor.rs` — правила алертов перенесены дословно (алерт только
  при росте счётчика или отсутствии baseline; critical при health-fail/росте;
  старые алерты устройства закрываются каждый прогон; baseline = «Resolve»).
- `/api/health/{summary,services,smart,smart/status,smart/refresh,smart/alert/{id}/resolve,smart/history/{device}}`
  + дашборд с теми же карточками и id; JS вынесен в `static/dashboard.js`
  (встраивается в бинарник через `include_bytes!`).
- `importer.rs` — read-only импорт `app.db`: user, audit, smart_disk, smart_alert,
  smart_sample, update_run, backup_task/run (потерявшие задачу прогоны получают
  `task_id=NULL`), scheduled_job, preserve; TEXT-даты → unix-millis; осиротевшие
  каталоги `preserves/*/*` импортируются как `ready` с пометкой; без `--force`
  непустая целевая БД отвергается; отчёт в `import-report.txt`.

### Проверено на живом хосте

- `cargo test` — 47 тестов.
- Дашборд: CPU 16 threads / 8 cores, 14.4 GiB RAM, диски `/`, `/boot/efi`,
  `/mnt/backup`, `/mnt/data`, RAID `clean 2/2`, температуры amdgpu/k10temp/spd5118.
- Услуги: 12 юнитов с реальными статусами (oneshot `immich-nightfix` → `success`
  с временем последнего запуска таймера).
- SMART: `refresh` по 4 дискам, алерт по `/dev/sde` (`rc=64`, ATA errors 2),
  `resolve` записал baseline; следующий авто-прогон алерт не поднимает.
- Импорт копии `app.db`: 183 sample, 19 alert, 5 дисков (baseline у sdd/sde),
  11 update_run, 5 backup_task, 3 preserve, 22 audit, 1 user; сервер на этой БД
  отдаёт 5 дисков и историю (54 записи sda).

### Отклонения от Python (осознанные)

- `storage` имеет `NOPASSWD: ALL`, поэтому read-only `systemctl show/journalctl/
  list-timers` вызываются как раньше, но батчем; sudo-контракт не расширялся.
- `mdstat` разбирается построчно (в Rust-regex нет look-ahead) — результат тот же.
- Сессия — не Flask-cookie; логин по неверному паролю отвечает 303 + flash
  вместо 200 с перерисовкой формы.
- `/api/health/smart` и `history` отдают RFC3339 (как Flask `isoformat`), но без
  микросекунд.

### Дальше

Фаза 3 (Services UI: управление, журналы, детали), фаза 4 (Backups + retention),
фаза 5 (Updates + preserve), фаза 6 (Scheduler UI), фаза 7 (rollout).

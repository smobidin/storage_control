# tools — Storage Control systemd user service

Обвязка запускает `storage-control` как **пользовательский** сервис systemd
(`systemctl --user`). Root не нужен: unit ставится в
`~/.config/systemd/user/storage-control.service`.

## Состав

| Файл | Назначение |
|---|---|
| `storage-control.service` | шаблон user-юнита; плейсхолдеры `@...@` подставляет `install.sh` |
| `install.sh` | устанавливает, включает и запускает сервис |
| `uninstall.sh` | останавливает, отключает и удаляет unit |
| `README.md` | эта инструкция |

## Быстрый старт

```bash
cd /home/storage/dev/stoage_system_tools/tools
./install.sh
```

Что делает `install.sh`:

1. Проверяет `wsgi.py` и наличие `gunicorn` в `storage-control/.venv`.
2. Если нет `~/.config/storage-control/secrets.env`, создаёт его со случайными
   `SECRET_KEY` и `ADMIN_PASSWORD` (пароль печатается один раз).
3. Рендерит шаблон в `~/.config/systemd/user/storage-control.service`.
4. Включает linger для пользователя (чтобы сервис жил после logout).
5. Выполняет `systemctl --user daemon-reload`, `enable` и `restart`.
6. Показывает статус и подсказки по управлению.

По умолчанию сервис слушает `0.0.0.0:8765`, то есть все интерфейсы.
Для внешнего доступа ограничьте доступ firewall/nginx; если сервис должен
быть доступен только локально, установите с `--bind 127.0.0.1:8765`.

## Если виртуального окружения ещё нет

```bash
./install.sh --setup-venv
```

Флаг создаст `storage-control/.venv` и поставит `requirements.txt` (нужен
`python3-venv` и доступ к PyPI).

## Основные опции

```text
--app-dir PATH     каталог приложения (по умолчанию ../storage-control)
--venv PATH        виртуальное окружение с gunicorn
--env-file PATH    файл секретов (по умолчанию ~/.config/storage-control/secrets.env)
--bind ADDR        адрес bind (по умолчанию 0.0.0.0:8765)
--workers N        число gunicorn-воркеров; держите 1
--threads N        число потоков gunicorn
--setup-venv       создать venv и установить зависимости
--no-create-env    не создавать файл секретов автоматически
--no-enable        не включать автозапуск при входе
--no-start         не перезапускать сервис сейчас
```

Пример: слушать только localhost, без немедленного старта:

```bash
./install.sh --bind 127.0.0.1:8765 --no-start
```

## Управление

```bash
systemctl --user status storage-control
systemctl --user restart storage-control
systemctl --user stop storage-control
systemctl --user start storage-control
journalctl --user -u storage-control -f
```

## tools/immich — ночной фикс превьюшек

`immich/night_fix_thumbnails.cjs` — самодельный скрипт, который чинит ассеты
без `thumbhash` (Samsung HEIC через libheif 1.23.4, RAW через exiftool/dcraw,
BMP/PSD через ImageMagick, видео через ffmpeg). Запускается юнитом
`immich-nightfix.service` по таймеру `immich-nightfix.timer` каждый день в 03:30.

Скрипт лежит **в этом репозитории**, а не в `~/immich/src`: Preserve в
storage-control делает `git stash push -u`, который уносит untracked-файлы
immich, — 2026-10-14 скрипт так и пропал. Юнит запускает **обёртку**
`%h/dev/stoage_system_tools/tools/immich/night_fix_wrapper.sh`, а она — сам
скрипт `night_fix_thumbnails.cjs` (см. ниже: напрямую скрипт запускать нельзя,
он падает на teardown'е).

Зависимости (`sharp`, `pg`, `thumbhash`, `exiftool-vendored.pl`) берутся из
`node_modules` immich-server через `createRequire`: `NODE_PATH` не действует на
динамический `import()`, а каталоги `.pnpm` меняют имена при каждом
`pnpm install`. `DB_URL` юнит подставляет из `~/immich/.env`.

```bash
systemctl --user start immich-nightfix.service     # разовый прогон
journalctl --user -u immich-nightfix.service -n 20
```

Код возврата: 0 — чинить было нечего или всё починено; 1 — что-то осталось
без thumbhash (в journal попадает список `FAIL <asset id>`).

Скрипт запускается не напрямую, а через `immich/night_fix_wrapper.sh`: после
прохода по очереди он падает на teardown'е нативного модуля sharp/libvips
(«known issue с очисткой native-модуля»). Сигнал зависит от версии node:
24.15.0 — SIGABRT (код 134), 24.22.1 — SIGSEGV (код 139).

Важно: краш **уничтожает собственный код возврата скрипта** — с загруженным
sharp и `process.exit(0)`, и `process.exit(1)` дают на выходе 134/139. Поэтому
исход обёртка восстанавливает из вывода, а не из кода процесса:

| что напечатал скрипт | смысл | код обёртки |
|---|---|---|
| `nothing to fix` | чинить было нечего | 0 |
| `done: N fixed, 0 failed, 0 still without preview` | очередь пройдена, всё починено | 0 |
| `done: N fixed, F failed, L still without preview` | часть ассетов осталась без превью | 1 |
| маркера нет вовсе | упал посреди работы | код сигнала (134/139) — видно в journal как ошибка юнита |

Вывод обёртка стримит в journal через `tee` (а не копит в файл), так что
убитый прогон всё равно оставляет в журнале свой прогресс.

## tools/immich/patches — локальные патчи immich

Каталога больше нет: патч `checkDiskUsage` (`frsize` → `bsize`) удалён
2026-10-21 вместе с обновлением Node (см. ниже). Шаг `apply_local_patches` в
`runners/update_immich.py` корректно переживает отсутствие каталога — пишет
«no local patches» и возвращает 0.

### Почему патч был нужен и почему исчез

Апстрим починил расчёт дисковой емкости в **v3.3.0** (коммит `3af49d9b4`,
#31983): `checkDiskUsage` переключили с `stats.bsize` на `stats.frsize`.
Ошибка апстрима была обратной — на virtiofs `bsize` это размер I/O-трансфера
(1 MiB), а `frsize` реальный размер блока (4 KiB), из-за чего `blocks * bsize`
завышал объём в 256 раз.

Но `frsize` появился в Node только в **v24.16.0** (nodejs/node#62277, «fs:
expose frsize field in statfs», коммит `40ccfdecf9`, 2026-05-21). На хосте
стоял `~/immich/node/bin/node` **v24.15.0** — на один патч-релиз ниже, и
`fs.statfs()` отдавал `{type, bsize, blocks, bfree, bavail, files, ffree}` без
`frsize`. Код апстрима на таком node даёт `undefined * number` → `NaN` в
`diskSize` / `diskAvailable` / `diskUsagePercentage`.

Поэтому 2026-10-21 Node обновлён до **v24.22.1** (LTS «Krypton», ровно та
версия, которую пиннит апстрим в `.nvmrc` для v3.3.5) и патч снят. Проверено:
`fs.statfs().frsize` есть, `frsize == bsize == 4096`, `/api/server/storage`
отдаёт `diskSizeRaw` 3936685645824 — байт в байт как `df`.

Замечания на будущее:

- Раннер Node не обновляет: `_tool_env()` в `runners/update_immich.py` только
  подставляет `~/immich/node/bin` в `PATH`. Версия рантайма живёт своей
  жизнью, пока её не заменят руками.
- ABI (modules 137) не менялся между 24.15.0 и 24.22.1, поэтому нативные
  модули (sharp 0.35.5, bcrypt, msgpackr) пересобирать не пришлось.
- Официальный тарбол Node **не содержит** corepack-шимов `pnpm`/`pnpx`/`yarn`
  — и в 24.22.1, и в 24.15.0 (шимы в той установке были созданы руками). После
  любой переустановки Node их нужно пересоздать:

  ```bash
  cd ~/immich/node/bin
  ln -sfn ../lib/node_modules/corepack/dist/pnpm.js   pnpm
  ln -sfn ../lib/node_modules/corepack/dist/pnpx.js   pnpx
  ln -sfn ../lib/node_modules/corepack/dist/yarn.js   yarn
  ln -sfn ../lib/node_modules/corepack/dist/yarnpkg.js yarnpkg
  ```

  Иначе `pnpm` из раннера резолвится не из `~/immich/node/bin`, а из
  `~/.local/bin`, и работает уже не тот corepack.
- Старая установка сохранена в `~/immich/node.bak-24.15.0` — это и откат, и
  источник самого патча, если Node придётся откатывать ниже 24.16.0 (тогда
  `frsize` пропадёт и дашборд снова покажет `NaN`).
- Коммит патча в `~/immich/src` сохранён под тегом `local/statfs-bsize-patch`
  (`0dab4eb7e`). В истории **этого** репозитория остался только patch-файл
  (`git show 3f5596e:tools/immich/patches/0001-storage-statfs-bsize.patch`,
  в его заголовке упомянут исходный SHA) — самого объекта коммита здесь нет.

Если Node всё-таки придётся откатывать ниже 24.16.0 (где `frsize` ещё не
было), патч надо вернуть — иначе дашборд снова покажет `NaN`:

```bash
cd ~/immich/src
git cherry-pick local/statfs-bsize-patch        # коммит 0dab4eb7e
pnpm --filter immich build                       # пакет сервера называется "immich"
```

Если контекст сместился и cherry-pick ругается:

```bash
git cherry-pick --abort
# patch-файл лежит в истории этого репозитория:
git show 3f5596e:tools/immich/patches/0001-storage-statfs-bsize.patch > /tmp/statfs-bsize.patch
git apply --3way /tmp/statfs-bsize.patch
```

И **обязательно** сбросить файл перед следующим обновлением immich: каталога
`tools/immich/patches/` сейчас нет, поэтому `_patched_files()` в раннере не
знает про изменённый `server/src/repositories/storage.repository.ts` и не
сбросит его перед `git checkout <tag>` — checkout откажется перезаписывать
локальные правки.

```bash
cd ~/immich/src
git checkout -- server/src/repositories/storage.repository.ts
```

Шаг `apply_local_patches` в `runners/update_immich.py` после `git checkout
<tag>` прогоняет каждый файл из `tools/immich/patches/*.patch` (`--check` →
обычный `apply` → `--3way`) и делает это **до** сборки сервера, чтобы правка
попала в `dist`. Шаг нефатальный: если патч больше не применяется, в лог
пишется предупреждение и обновление продолжается, а дерево откатывается
через `git reset --hard`, чтобы в `dist` не уехали маркеры конфликта. Сейчас
каталога `tools/immich/patches/` нет, и шаг штатно отрабатывает вхолостую
(«no local patches», rc 0) — но код в раннере остаётся на месте, чтобы
патчи можно было вернуть без правок самого раннера.

## tools/backup_home_rsync.sh — зеркало домашней директории

Системный диск (`/dev/sde2`, на нём `/` и `/home/storage`) с проблемами, поэтому
каждую ночь дом копируется на отдельный диск `/mnt/backup` (`/dev/sdc1`):

```bash
systemctl --user start backup-home-rsync.service    # разовый прогон
journalctl --user -u backup-home-rsync.service -n 30
```

Таймер `backup-home-rsync.timer` — 05:00 (намеренно после `backup-immich-nc.timer`
в 04:00, который пишет дампы БД и конфиги в `/mnt/backup/backups`).

Раскладка назначения:

| Путь | Что |
|---|---|
| `/mnt/backup/backups/home-storage/current/` | зеркало `/home/storage` |
| `/mnt/backup/backups/home-storage/deleted/` | удалённые и перезаписанные файлы (`--backup-dir`), хранятся `KEEP_DAYS` |
| `/mnt/backup/backups/home-storage/logs/` | логи rsync, ротация та же |

Особенности:

- копируется всё, кроме `/home/storage/.cache/` (3.1 G регенерируемого кэша:
  `immich_ml`, `playwright`, `pnpm`, `uv`);
- медиафайлы сюда **не** попадают — они лежат на `/mnt/data` (RAID) и на
  проблемный системный диск не претендуют;
- зеркало с `--delete`, но удаляемое не пропадает: оно уезжает в `deleted/`,
  и только ротация через 30 дней его убирает;
- запускает пользователь storage, root не нужен;
- `rc=24` (файлы исчезли во время копирования) считается нормой при живой
  системе, остальные ненулевые коды — ошибка с логом в `logs/`.

## tools/backup_immich_nc.sh — дампы БД и конфигов

Работает парой к зеркалу выше: таймер `backup-immich-nc.timer` в **04:00**
кладёт в `/mnt/backup/backups` то, что нужно для быстрого восстановления
сервисов (ротация — 14 дней):

| Архив | Содержимое |
|---|---|
| `immich/immich_db_<ts>.dump` | `pg_dump -Fc` базы immich |
| `immich/immich_config_<ts>.tar.gz` | `immich/.env`, юниты immich-server/immich-ml/nightfix, `night_fix_thumbnails.cjs` |
| `nextcloud/nextcloud_db_<ts>.dump` | `pg_dump -Fc` базы nextcloud |
| `nextcloud/nextcloud_config_<ts>.tar.gz` | `nextcloud/config/config.php` |
| `nginx_config_<ts>.tar.gz` | `sites-available/{proxy-sites,nextcloud}`, `conf.d/websocket.conf`, letsencrypt |
| `postgres_config_<ts>.tar.gz` | `postgresql.conf`, `pg_hba.conf` (через `sudo`) |
| `backup_units_<ts>.tar.gz` | юниты и таймеры обоих бэкапов |
| `pkg_list_<ts>.txt` | `dpkg --get-selections` |

```bash
systemctl --user start backup-immich-nc.service   # разовый прогон
journalctl --user -u backup-immich-nc.service -n 30
```

Скрипту нужен passwordless `sudo` для `tar` из `/etc/postgresql/18/main` —
разрешение лежит в `/etc/sudoers.d/storage`. Сам дом целиком копирует
`backup_home_rsync.sh`, поэтому здесь приоритет у консистентности (дампы БД
через `pg_dump`, а не rsync по живой базе).

## Удаление сервиса

```bash
./uninstall.sh
```

Скрипт останавливает и отключает unit, удаляет файл юнита и делает
`daemon-reload`. Секреты (`secrets.env`), база и логи **не удаляются**.

## Важно

- Сервис пользовательский: все команды идут через `systemctl --user`.
- При старте приложение сбрасывает пароль `admin` на `ADMIN_PASSWORD` из
  `secrets.env`, если переменная задана. Если вы уже пользовались системой,
  впишите текущий пароль в `secrets.env` или сразу смените его после установки.
- Приложение обязано работать в одном gunicorn-воркере (долгие update/preserve
  задачи запускаются в daemon-потоках). Не увеличивайте `--workers` без
  переработки этих задач.
- Сервис по умолчанию слушает все интерфейсы (`0.0.0.0:8765`). Если NAS
  доступен из недоверенной сети, закройте порт firewall или поставьте
  nginx/TLS; для локального доступа используйте `--bind 127.0.0.1:8765`.

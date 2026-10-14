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
immich, — 2026-10-14 скрипт так и пропал. Юнит запускает его по пути
`%h/dev/stoage_system_tools/tools/immich/night_fix_thumbnails.cjs`.

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

## tools/immich/patches — локальные патчи immich

`patches/0001-storage-statfs-bsize.patch` — правка `checkDiskUsage` в
`server/src/repositories/storage.repository.ts`: `frsize` → `bsize`. Node на
этом хосте не возвращает `frsize` в `fs.statfs`, поэтому `available/free/total`
уходили в `NaN` на дашборде.

Тот же коммит лежит веткой **`local/patches`** в репозитории `~/immich/src`
(автор `storage <storage@nas.local>`, там не настроена глобальная идентичность).
Ветку в origin не пушить: `origin` у immich — это апстрим `immich-app/immich`.
Копия патча в этом репозитории нужна как страховка, если immich-репо пересоздадут.

Апдейтер возвращает патч сам: шаг `apply_local_patches` в
`runners/update_immich.py` после `git checkout <tag>` прогоняет каждый файл из
`tools/immich/patches/*.patch` (`--check` → обычный `apply` → `--3way`) и делает
это **до** сборки сервера, чтобы правка попала в `dist`. Шаг нефатальный:
если патч больше не применяется (например, апстрим починил причину), в лог
пишется предупреждение и обновление продолжается, а дерево откатывается через
`git reset --hard`, чтобы в `dist` не уехали маркеры конфликта.

Ручной путь — на случай если апдейтером не пользуемся:

```bash
cd ~/immich/src
git cherry-pick local/patches              # быстрый путь
# если в новом теге контекст сместился:
git cherry-pick --abort
git apply --3way ~/dev/stoage_system_tools/tools/immich/patches/0001-storage-statfs-bsize.patch
# если ветка вдруг потерялась — достаём из патч-файла выше
pnpm --filter @immich/server build         # иначе правка не попадёт в dist
```

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

# Системная документация Immich + Nextcloud на nas

## Общая информация

- **Хост:** nas (AOOSTAR WTR MAX, Ryzen 7 PRO 8845HS)
- **OS:** Ubuntu 26.04
- **RAID:** md0 raid1 (sda + sdb, ~3.6T usable)
- **Пользователь:** storage (uid=1001)
- **PostgreSQL:** 18.6 (PGDG репозиторий)
- **nginx:** 1.28.3 (reverse proxy на портах 8080/8081)
- **Redis:** системный, для Nextcloud сессий

---

## Immich

### Установка и исходники

- **Репозиторий:** `~/immich/src/` — git clone из https://github.com/immich-app/immich.git
- **Версия:** v3.3.1 (собрана из исходников, не Docker)
- **Ветка:** release (tag v3.3.1)
- **Сборка:**
  ```bash
  cd ~/immich/src
  npm ci  # или npm install
  npm run build  # web + server
  ```
- **ML-модели:** отдельный pip-пакет `immich_model` из https://github.com/immich-app/ml-models.git
  - Установлен в venv: `~/immich/src/machine-learning/.venv/`

### Запуск и сервисы

Сервисы работают под пользователем `storage` через systemd user units:

| Сервис | Описание | Порт | Команда |
|--------|----------|------|---------|
| `immich-server.service` | Основной API + web | 2283 | `systemctl --user start immich-server` |
| `immich-ml.service` | ML inference (распознавание лиц, объекты) | 3003 | `systemctl --user start immich-ml` |
| `immich-nightfix.service` | Ночной фикс миниатюр | — | `systemctl --user start immich-nightfix` |
| `immich-nightfix.timer` | Таймер: ежедневно 03:00 | — | `systemctl --user enable immich-nightfix.timer` |

**Юниты лежат:** `~/.config/systemd/user/`

**Логи:** `journalctl --user -u immich-server -f`

**Конфиг:** `~/immich/.env` (DB_URL, порты, пути)

**Запуск вручную:**
```bash
cd ~/immich/src/server && node dist/main.js  # server
cd ~/immich/src/machine-learning && .venv/bin/python -m immich_ml  # ML
```

### Пользовательские файлы

- **Библиотека:** `/mnt/data/immich/library/`
  - `upload/` — оригиналы загруженных файлов (UUID-структура)
  - `thumbs/` — миниатюры
  - `encoded-video/` — перекодированное видео
  - `profile/` — аватары
- **Внешняя библиотека:** `/mnt/backup/external_media_library/` (симлинк или mount)

### Служебные файлы

- **БД:** PostgreSQL, база `immich`, пользователь `storage`
- **DB URL:** `postgresql://storage:PASSWORD@localhost:5432/immich`
- **Кэш:** `~/.cache/immich/`
- **Логи задач:** `/mnt/data/immich/library/backups/`

---

## Nextcloud

### Установка и исходники

- **Репозиторий:** `~/nextcloud/` — git clone из https://github.com/nextcloud/server.git
- **Версия:** v34.0.4 (GitHub release tag)
- **Ветка:** stable34
- **Обновление:** `git fetch origin --tags && git checkout v34.0.X`
- **Веб-рут:** `~/nextcloud/`

### Запуск и сервисы

Nextcloud работает через nginx + PHP-FPM (системные юниты, не user):

| Компонент | Описание | Порт/Сокет | Сервис |
|-----------|----------|------------|--------|
| nginx | Reverse proxy + SSL | 8080/8081 | `nginx` (system) |
| PHP-FPM | Nextcloud pool | `/run/php/php8.5-fpm-nextcloud.sock` | `php8.5-fpm` (system) |
| Redis | Сессии + кэш | 6379 (dbindex=1) | `redis-server` (system) |
| PostgreSQL | БД nextcloud_db | 5432 | `postgresql` (system) |

**Конфиг Nextcloud:** `~/nextcloud/config/config.php`
- БД: `nextcloud_db`, пользователь `nextcloud_db`
- Redis: localhost:6379, dbindex=1

**Конфиг PHP-FPM pool:** `/etc/php/8.5/fpm/pool.d/nextcloud.conf`

**Конфиг nginx:** `/etc/nginx/sites-available/nextcloud` (включён в proxy-sites)

### Пользовательские файлы

- **Данные:** `/mnt/data/nextcloud/`
  - Файлы пользователей
  - `.ncdata` — маркер директории
- **Логи Nextcloud:** `/mnt/data/nextcloud/nextcloud.log`

### Служебные файлы

- **БД:** PostgreSQL, база `nextcloud_db`, пользователь `nextcloud_db`
- **Кэш:** Redis (dbindex=1)
- **Локи:** `~/nextcloud/data/.ocdata`

---

## Whiteboard (Nextcloud)

Совместные доски: приложение Nextcloud `whiteboard` (v2.0.0) + отдельный Node WebSocket-сервер
для real-time-синхронизации.

### Компоненты

| Компонент | Путь / юнит | Порт |
|-----------|-------------|------|
| Приложение Nextcloud | `/home/storage/nextcloud/apps/whiteboard` | — |
| WS-сервер | `/var/lib/whiteboard-websocket/` (распакован из релиза v2.0.0, не git) | 3002 |
| systemd сервис | `whiteboard-websocket.service` (system) | 3002 |
| Node.js | `/home/storage/immich/node/bin/node` (v24) | — |

- **Юнит:** `/etc/systemd/system/whiteboard-websocket.service`
- **Логи:** `journalctl -u whiteboard-websocket -f`

### Конфигурация

`occ config:app:get whiteboard <key>`:

- `collabBackendUrl` = `https://drive.itexperts.team/whiteboard`
- `jwt_secret_key` — общий секрет с WS-сервером (должен совпадать с `JWT_SECRET_KEY` в юните)

Переменные юнита: `NEXTCLOUD_URL=https://drive.itexperts.team`, `PORT=3002`,
`JWT_SECRET_KEY=<jwt_secret_key>`, `STORAGE_STRATEGY=lru`, `MAX_UPLOAD_FILE_SIZE=20`, `TLS=false`,
`CHROME_EXECUTABLE_PATH=/home/storage/.cache/ms-playwright/chromium-1248/chrome-linux64/chrome`.
Секрет хранится в БД Nextcloud (`occ config:app:get whiteboard jwt_secret_key`) и в юните — в git не попадает.

### Маршрутизация (два слоя nginx)

```
Клиент → :8080 proxy-sites (TLS) → :8443 nextcloud → 127.0.0.1:3002
```

- `/whiteboard/` в `/etc/nginx/sites-available/nextcloud` → `http://127.0.0.1:3002`.
- Внешний блок `drive.itexperts.team` в `proxy-sites` **обязан** пробрасывать WebSocket:
  `proxy_http_version 1.1;` + `proxy_set_header Upgrade $http_upgrade;` +
  `proxy_set_header Connection $connection_upgrade;`. Без этих строк апгрейд отбрасывается
  на внешнем слое, `$http_upgrade` во внутреннем nginx пуст и handshake не даёт `101`
  (клиент сообщает «websocket error»).
- `map $http_upgrade $connection_upgrade` — `/etc/nginx/conf.d/websocket.conf`.

### Запись (recording)

Запись доски идёт через headless-Chromium (`puppeteer-core`); при каждом подключении клиента
`RecordingControlService.checkAvailability` проверяет, запускается ли браузер. Пакетного
`chromium`/`google-chrome` в системе нет, поэтому путь задан явно в юните:
`CHROME_EXECUTABLE_PATH=/home/storage/.cache/ms-playwright/chromium-1248/chrome-linux64/chrome`.

Это уже установленный Playwright Chromium. Автодетект в `ConfigUtility.js` не работает:
`playwright-core` 1.63 резолвит `chromium-1243`, а установлен `chromium-1248`. Если Playwright
обновится и сменит ревизию, путь надо поправить (либо поставить системный Chrome и указать
`/usr/bin/google-chrome-stable`).

Симптом при поломке: тост «Chromium failed to start (…)» в UI и `[Puppeteer] Launch check failed`
в `journalctl -u whiteboard-websocket`.

### Локальные патчи (теряются при обновлении приложения/сервера)

- `apps/whiteboard/lib/Listener/AddContentSecurityPolicyListener.php` —
  `addAllowedFontDomain('https://esm.sh')`: excalidraw грузит шрифты с esm.sh, иначе CSP
  `font-src` их блокирует.
- `websocket_server/Services/SocketService.js` — `path: '/whiteboard/socket.io/'` в
  `createSocketServer()`: сервер смонтирован под `/whiteboard/`, в upstream поля `path` нет.

Из-за патча слушателя подпись app-store не совпадает, и `occ integrity:check-app whiteboard`
показывает 1 `INVALID_HASH` (косметика).

### Проверка

```bash
systemctl status whiteboard-websocket
# Ожидается 101 Switching Protocols:
curl -sk --max-time 5 -o /dev/null -w '%{http_code}\n' \
  -H 'Upgrade: websocket' -H 'Connection: Upgrade' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==' \
  'https://drive.itexperts.team/whiteboard/socket.io/?EIO=4&transport=websocket'

# CSP (нужна аутентификация — без неё Nextcloud отдаёт 401 со своим CSP `font-src 'self' data:`):
# приложение паролей: php occ user:add-app-password <user>, потом удалить токен
curl -sk -u '<user>:<app-password>' -D- -o /dev/null 'https://drive.itexperts.team/apps/files/' \
  | grep -i content-security-policy
# Ожидается `font-src 'self' data: https://esm.sh` и `connect-src … wss://drive.itexperts.team`.

# Поиск ошибок записи:
journalctl -u whiteboard-websocket --since '-1 hour' | grep -i 'puppeteer\|chromium'
```

Для запуска `occ` нужен `chmod 660 config/config.php`, затем вернуть `640`.

---

## nginx Reverse Proxy

### Конфигурация

- **Основной конфиг:** `/etc/nginx/sites-available/proxy-sites`
- **SSL сертификаты:** `/etc/letsencrypt/live/photo.itexperts.team/`
  - Покрывает: `photo.itexperts.team` + `drive.itexperts.team` (SAN)
- **WebSocket support:** `/etc/nginx/conf.d/websocket.conf` (`map $http_upgrade $connection_upgrade`)
- **Внутренний сайт Nextcloud:** `/etc/nginx/sites-available/nextcloud` (TLS :8443)

### Server blocks

| Домен | Порт | Upstream | Назначение |
|-------|------|----------|------------|
| photo.itexperts.team | 8080 ssl | 127.0.0.1:2283 | Immich |
| drive.itexperts.team | 8080 ssl | 127.0.0.1:8443 (HTTPS) | Nextcloud |
| Все | 8081 | → 301 https | HTTP redirect + ACME |

Внутри `drive.itexperts.team` (site `nextcloud`, :8443): `/whiteboard/` → `http://127.0.0.1:3002`
(WebSocket-сервер Whiteboard, см. раздел Whiteboard).

**Заголовки Upgrade в двух слоях:** `proxy_set_header Upgrade`/`Connection` + `proxy_http_version 1.1`
нужны и во внешнем блоке `drive.itexperts.team` (`proxy-sites`), и во внутреннем `/whiteboard/`
(`nextcloud`). Иначе апгрейд теряется и WebSocket не поднимается.

**ACME webroot:** `/var/www/letsencrypt` (для certbot)

---

## Фоновые задачи (cron / timers)

### Системные (root)

| Задача | Расписание | Описание |
|--------|-----------|----------|
| `certbot-renew.timer` | Ежедневно 03:00 ±10min | Обновление SSL-сертификатов LetsEncrypt |
| `mdcheck_start.timer` | 1-го числа каждого месяца ~20:14 | Проверка целостности RAID1 (md0) |
| `mdcheck_continue.timer` | По необходимости | Продолжение mdcheck, если не уложился |
| `postgresql` | Always on | PostgreSQL 18.6 |
| `redis-server` | Always on | Redis для Nextcloud |
| `nginx` | Always on | Reverse proxy |
| `php8.5-fpm` | Always on | PHP-FPM для Nextcloud |
| `whiteboard-websocket` | Always on | WebSocket-сервер Whiteboard (порт 3002) |

### Пользовательские (storage)

| Задача | Расписание | Описание |
|--------|-----------|----------|
| `backup-immich-nc.timer` | Ежедневно 04:00 ±10min | Бэкап БД + конфигов Immich/Nextcloud/Stalwart |
| `immich-nightfix.timer` | Ежедневно 03:00 | Фикс миниатюр Immich |

**Статус таймеров:**
```bash
systemctl list-timers --all           # системные
systemctl --user list-timers --all    # пользовательские
```

---

## Резервное копирование

### Скрипт

- **Путь:** `~/tools/backup_immich_nc.sh`
- **Запуск:** systemd timer `backup-immich-nc.timer` (04:00 daily)
- **Логи:** `/mnt/backup/backups/logs/`

### Что бэкапится

| Компонент | Формат | Путь назначения |
|-----------|--------|----------------|
| Immich БД | pg_dump -Fc (custom) | `/mnt/backup/backups/immich/immich_db_YYYY-MM-DD_HHMM.dump` |
| Immich конфиги | tar.gz | `/mnt/backup/backups/immich/immich_config_YYYY-MM-DD_HHMM.tar.gz` |
| Nextcloud БД | pg_dump -Fc | `/mnt/backup/backups/nextcloud/nextcloud_db_YYYY-MM-DD_HHMM.dump` |
| Nextcloud конфиги | tar.gz | `/mnt/backup/backups/nextcloud/nextcloud_config_YYYY-MM-DD_HHMM.tar.gz` |
| nginx + SSL | tar.gz | `/mnt/backup/backups/nginx_config_YYYY-MM-DD_HHMM.tar.gz` |
| PostgreSQL конфиг | tar.gz | `/mnt/backup/backups/postgres_config_YYYY-MM-DD_HHMM.tar.gz` |
| Список пакетов | txt | `/mnt/backup/backups/pkg_list_YYYY-MM-DD_HHMM.txt` |

### Ротация

- Хранение: **14 дней**
- Автоудаление старых `.dump`, `.tar.gz`, `.txt`, логов

### Восстановление

**Immich БД:**
```bash
pg_restore -h localhost -U storage -d immich -c immich_db_YYYY-MM-DD_HHMM.dump
```

**Nextcloud БД:**
```bash
pg_restore -h localhost -U nextcloud_db -d nextcloud_db -c nextcloud_db_YYYY-MM-DD_HHMM.dump
```

**Конфиги:**
```bash
tar xzf immich_config_YYYY-MM-DD_HHMM.tar.gz -C /home/storage/
tar xzf nextcloud_config_YYYY-MM-DD_HHMM.tar.gz -C /home/storage/nextcloud/
```

---

## SSL / Certbot

- **Провайдер:** LetsEncrypt (ACME v2)
- **Валидатор:** webroot (`/var/www/letsencrypt`)
- **Порт для ACME:** 8081 (HTTP)
- **Обновление:** `certbot renew --quiet` (ежедневно 03:00)
- **Deploy hook:** `/etc/letsencrypt/renewal-hooks/deploy/reload-nginx.sh`
  - Перезагружает nginx после успешного обновления сертификата
- **Конфиги renewal:** `/etc/letsencrypt/renewal/`

---

## PostgreSQL

- **Версия:** 18.6
- **Источник:** PGDG (PostgreSQL Global Development Group)
- **Пакеты:** `postgresql-18`, `postgresql-18-pgvector`, `postgresql-18-vchord`
- **Конфиг:** `/etc/postgresql/18/main/postgresql.conf`
- **HBA:** `/etc/postgresql/18/main/pg_hba.conf`
- **Data dir:** `/var/lib/postgresql/18/main/`
- **Базы:**
  - `immich` (пользователь `storage`)
  - `nextcloud_db` (пользователь `nextcloud_db`)

---

## RAID

- **Устройство:** `/dev/md0`
- **Тип:** raid1 (зеркало)
- **Диски:** sda + sdb
- **Размер:** ~3.6T usable
- **Проверка:** mdcheck (1-го числа каждого месяца, макс 6 часов)
- **Статус:** `cat /proc/mdstat`

---

## Полезные команды

```bash
# Статус сервисов
systemctl --user status immich-server immich-ml
systemctl status nginx postgresql redis-server php8.5-fpm whiteboard-websocket

# Логи
journalctl --user -u immich-server -f
journalctl -u whiteboard-websocket -f
sudo tail -f /var/log/nginx/error.log

# БД
psql "$(grep DB_URL ~/immich/.env | cut -d= -f2-)"
psql -h localhost -U nextcloud_db -d nextcloud_db

# RAID
cat /proc/mdstat
sudo mdadm --detail /dev/md0

# Бэкап вручную
~/tools/backup_immich_nc.sh

# Размер библиотек
du -sh /mnt/data/immich/library /mnt/data/nextcloud
```

---

## Nextcloud Office (Collabora Online)

### Установка

Nextcloud Office (richdocuments) работает через локально запущенный Collabora Online (CODE) в виде AppImage.
CODE запускается автоматически на порту 9983 при первом обращении к редактированию документа.

**AppImage:** `/home/storage/nextcloud/apps/richdocumentscode/collabora/Collabora_Online.AppImage`

**Процессы при работе:**
```bash
ps aux | grep coolwsd
# coolwsd (главный процесс) + coolforkit-ns (worker-процессы)
```

**Логи Collabora:** `/tmp/coolwsd.<random>/coolwsd.log`

### Конфигурация в Nextcloud

Настройки в `oc_appconfig` (таблица `oc_appconfig`, appid=`richdocuments`):

| Ключ | Значение |
|------|----------|
| `wopi_backend_mode` | `local` |
| `wopi_url` | `https://drive.itexperts.team/apps/richdocumentscode/proxy.php?req=` |
| `public_wopi_url` | `https://drive.itexperts.team` |
| `wopi_allowlist` | `127.0.0.1/32` |
| `disable_certificate_verification` | (пусто = проверка включена) |

### Проблема с NAT loopback и решение

**Симптом:** Фоновое задание `OCA\Richdocuments\Backgroundjobs\ObtainCapabilities` (cron 04:05) падает с ошибкой:
```
Failed to fetch discovery: cURL error 28: Operation timed out after 5001 milliseconds
for https://drive.itexperts.team/apps/richdocumentscode/proxy.php?req=/hosting/discovery
```

**Причина:** Cron-процесс (www-data) пытается подключиться к внешнему домену `drive.itexperts.team`, который резолвится в внешний IP (149.62.52.187). Запрос уходит через роутер и возвращается обратно (NAT loopback), но роутер/файрвол блокирует или задерживает его.

**Решение:** Добавить локальную запись в `/etc/hosts`:
```bash
echo "127.0.0.1 drive.itexperts.team" | sudo tee -a /etc/hosts
```

После этого запрос разрешается локально и идёт через локальный nginx:
`localhost:443` (внешний nginx) → `127.0.0.1:8443` (внутренний Nextcloud).

**Проверка:**
```bash
curl -k https://drive.itexperts.team/apps/richdocumentscode/proxy.php?req=/hosting/discovery
# Должен вернуть WOPI discovery XML
```

### Обновление Collabora Online (AppImage)

Collabora Online обновляется отдельно от Nextcloud. AppImage распространяется через Nextcloud App Store.

**Источник обновления:**
- Nextcloud App Store: https://apps.nextcloud.com/apps/richdocumentscode/releases?platform=26
- Прямая ссылка на tar.gz: `https://github.com/CollaboraOnline/richdocumentscode/releases/download/26.4.402/richdocumentscode.tar.gz`

**Процесс обновления:**
```bash
# 1. Остановить сервис
sudo systemctl stop collabora-online.service

# 2. Скачать и распаковать новый AppImage
wget https://github.com/CollaboraOnline/richdocumentscode/releases/download/26.4.402/richdocumentscode.tar.gz
tar -xzf richdocumentscode.tar.gz

# 3. Заменить AppImage
sudo cp richdocumentscode/collabora/Collabora_Online.AppImage /home/storage/nextcloud/apps/richdocumentscode/collabora/Collabora_Online.AppImage
sudo chown cool:cool /home/storage/nextcloud/apps/richdocumentscode/collabora/Collabora_Online.AppImage
sudo chmod +x /home/storage/nextcloud/apps/richdocumentscode/collabora/Collabora_Online.AppImage

# 4. Запустить сервис
sudo systemctl restart collabora-online.service

# 5. Проверить
sudo systemctl status collabora-online.service
curl -k https://drive.itexperts.team/apps/richdocumentscode/proxy.php?req=/hosting/discovery
```

**Версия совместимости:**
- Nextcloud 26/27 (richdocuments 12.x) → Collabora 26.04.x

---

*Документация актуальна на: $(date +%F)*

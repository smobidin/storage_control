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

## Обозначения (сетевые данные замаскированы)

Репозиторий публичный, поэтому все конкретные адреса, домены и имена интерфейсов
в документации заменены масками. Реальные значения хранятся **вне репозитория**:
`~/.config/storage-control/network-map.md`.

| Маска | Что означает |
|---|---|
| `<DOMAIN>` | базовый домен (публичная зона) |
| `mail.<DOMAIN>`, `drive.<DOMAIN>`, `photo.<DOMAIN>`, `home.<DOMAIN>` | поддомены сервисов |
| `<WAN_IP>` | статический публичный адрес роутера — A-запись всех поддоменов и PTR почты |
| `<ALT_EGRESS_IP>` | второй публичный адрес (альтернативный маршрут провайдера, не для почты) |
| `<NAS_LAN_IP>` | LAN-адрес NAS |
| `<LAN_CIDR>`, `<LAN_PREFIX>` | локальная сеть и её первые два октета |
| `<ROUTER_LAN_IP>` | LAN-адрес роутера (он же source NAT при hairpin) |
| `<VPN_CIDR>`, `<VPN_CLIENT_IP>` | пул входящего WireGuard-VPN и адрес клиента в нём |
| `<VPN_IF>` | имя VPN-интерфейса на роутере |
| `<VPN_ZONE>` | имя зоны VPN в fw4 |
| `<ROUTER_NAME>` | hostname роутера |
| `<ADMIN_EMAIL>`, `<USER_EMAIL>` | адрес из DMARC-записи и адрес из примеров логов |

При правках документации новые реальные значения сюда **не вписывать** — только маски,
а расшифровку дополнять в локальном файле.

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
| `immich-nightfix.timer` | Таймер: ежедневно 03:30 ±10min | — | `systemctl --user enable immich-nightfix.timer` |

**Юниты лежат:** `~/.config/systemd/user/`

**Логи:** `journalctl --user -u immich-server -f`

**Логи ночного фикса:** `journalctl --user -u immich-nightfix.service` — отдельного
файла-лога нет, вывод идёт в журнал systemd (юнит без `StandardOutput=`,
журнал персистентный: `/var/log/journal`).

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

- `collabBackendUrl` = `https://drive.<DOMAIN>/whiteboard`
- `jwt_secret_key` — общий секрет с WS-сервером (должен совпадать с `JWT_SECRET_KEY` в юните)

Переменные юнита: `NEXTCLOUD_URL=https://drive.<DOMAIN>`, `PORT=3002`,
`JWT_SECRET_KEY=<jwt_secret_key>`, `STORAGE_STRATEGY=lru`, `MAX_UPLOAD_FILE_SIZE=20`, `TLS=false`,
`CHROME_EXECUTABLE_PATH=/home/storage/.cache/ms-playwright/chromium-1248/chrome-linux64/chrome`.
Секрет хранится в БД Nextcloud (`occ config:app:get whiteboard jwt_secret_key`) и в юните — в git не попадает.

### Маршрутизация (два слоя nginx)

```
Клиент → :8080 proxy-sites (TLS) → :8443 nextcloud → 127.0.0.1:3002
```

- `/whiteboard/` в `/etc/nginx/sites-available/nextcloud` → `http://127.0.0.1:3002`.
- Внешний блок `drive.<DOMAIN>` в `proxy-sites` **обязан** пробрасывать WebSocket:
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
  'https://drive.<DOMAIN>/whiteboard/socket.io/?EIO=4&transport=websocket'

# CSP (нужна аутентификация — без неё Nextcloud отдаёт 401 со своим CSP `font-src 'self' data:`):
# приложение паролей: php occ user:add-app-password <user>, потом удалить токен
curl -sk -u '<user>:<app-password>' -D- -o /dev/null 'https://drive.<DOMAIN>/apps/files/' \
  | grep -i content-security-policy
# Ожидается `font-src 'self' data: https://esm.sh` и `connect-src … wss://drive.<DOMAIN>`.

# Поиск ошибок записи:
journalctl -u whiteboard-websocket --since '-1 hour' | grep -i 'puppeteer\|chromium'
```

Для запуска `occ` нужен `chmod 660 config/config.php`, затем вернуть `640`.

---

## nginx Reverse Proxy

### Конфигурация

- **Основной конфиг:** `/etc/nginx/sites-available/proxy-sites` (внешний доступ, порты 8080/8081)
- **Локальный конфиг:** `/etc/nginx/sites-available/proxy-sites-local` (split DNS, порты 80/443)
- **SSL сертификаты:** `/etc/letsencrypt/live/photo.<DOMAIN>/`
  - Покрывает: `photo.<DOMAIN>` + `drive.<DOMAIN>` (SAN)
- **WebSocket support:** `/etc/nginx/conf.d/websocket.conf` (`map $http_upgrade $connection_upgrade`)
- **Ограничение доступа к почте:** `/etc/nginx/conf.d/mail-lan-access.conf` (`geo $mail_lan_client`)
- **Формат лога:** в `/etc/nginx/nginx.conf` определён `log_format main_ext` — к стандартному
  combined добавлены `host`, `port`, `scheme`, `sni`. Без них по `access.log` нельзя понять,
  пришёл запрос по домену или по IP; `sni` показывает, какое имя прислал клиент при TLS-хендшейке.
- **Внутренний сайт Nextcloud:** `/etc/nginx/sites-available/nextcloud` (TLS :8443)

### Server blocks

| Домен | Порт | Upstream | Назначение |
|-------|------|----------|------------|
| photo.<DOMAIN> | 8080 ssl | 127.0.0.1:2283 | Immich |
| drive.<DOMAIN> | 8080 ssl | 127.0.0.1:8443 (HTTPS) | Nextcloud |
| mail.<DOMAIN> | 8080 ssl | 127.0.0.1:8082 | Stalwart web UI, **только <LAN_CIDR>** |
| photo/drive/itexperts/www | 8081 | → 301 https | HTTP redirect + ACME |
| mail.<DOMAIN> | 8081 | → 301 https | HTTP redirect, **только <LAN_CIDR>** (ACME — всем) |
| те же имена | 80 / 443 ssl | те же апстримы | локальный доступ (split DNS) |

### Доступ к mail.<DOMAIN> только из локальной сети

Веб-интерфейс Stalwart (`mail.<DOMAIN>`, HTTP и HTTPS) открыт **только из
`<LAN_CIDR>`, VPN-пула `<VPN_CIDR>`** и с loopback; из интернета отдаётся `403`.
VPN-пул добавлен, потому что клиент входящего VPN получает `<VPN_CLIENT_IP>` и при заходе
по FQDN приходит именно с этого адреса. Правило продублировано
на всех четырёх точках входа (`:8080 ssl`, `:443 ssl`, `:8081`, `:80`), потому что
внешний трафик приходит через проброс роутера (`443→8080`, `80→8081`), а локальный —
напрямую на 80/443.

Две тонкости, из-за которых конфиг выглядит нестандартно:

- В блоках HTTP (`:8081`, `:80`) стоит `if ($mail_lan_client = 0) { return 403; }`, а не
  `allow`/`deny`: `return 301` выполняется в rewrite-фазе, **до** access-фазы, поэтому
  `deny all` в том же `location` был бы проигнорирован. В блоках с `proxy_pass`
  (`:8080 ssl`, `:443 ssl`) access-фаза отрабатывает до content, там обычные
  `allow <LAN_CIDR>; deny all;` — они и стоят.
- `location /.well-known/acme-challenge/` **намеренно не фильтруется**: Let's Encrypt
  валидирует домен с публичных адресов при продлении сертификата `<DOMAIN>`
  (SAN `mail.<DOMAIN>`, `home.<DOMAIN>`, истекает 2027-01-14). Если закрыть
  этот путь, автопродление сломается.

Клиенты из LAN заходят через hairpin роутера и видны как `<ROUTER_LAN_IP>` (либо напрямую
своим `<LAN_PREFIX>.x.x` при split DNS) — оба варианта попадают в разрешённую подсеть.
Проверка после правок: `sudo certbot renew --dry-run --cert-name <DOMAIN>
--no-random-sleep-on-renew` (обязательно проходит) и `curl -sk --resolve
mail.<DOMAIN>:443:<WAN_IP> https://mail.<DOMAIN>/admin/` → `200`.

Внутри `drive.<DOMAIN>` (site `nextcloud`, :8443): `/whiteboard/` → `http://127.0.0.1:3002`
(WebSocket-сервер Whiteboard, см. раздел Whiteboard).

**Заголовки Upgrade в двух слоях:** `proxy_set_header Upgrade`/`Connection` + `proxy_http_version 1.1`
нужны и во внешнем блоке `drive.<DOMAIN>` (`proxy-sites`), и во внутреннем `/whiteboard/`
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
| `immich-nightfix.timer` | Ежедневно 03:30 ±10min | Фикс миниатюр Immich |

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

## Журналы работы системы

Полный перечень того, где лежат логи всех сервисов. Пути проверены на хосте.

### Два уровня журнала — и права на них

Хост использует **journald** (журнал systemd) как основной источник логов.
Юниты делятся на два уровня, и это важно: пользователь `storage` состоит только
в группах `storage` и `www-data` — **ни `adm`, ни `systemd-journal`**, поэтому
системный журнал ему недоступен без `sudo`.

| Уровень | Команда | Что видно | sudo нужен? |
|---------|---------|-----------|-------------|
| Пользовательский | `journalctl --user -u <юнит>` | Юниты пользователя `storage`: immich, stalwart, storage-control, бэкапы, ночной фикс | **нет** |
| Системный | `sudo journalctl -u <юнит>` | nginx, php8.5-fpm, postgresql, redis, whiteboard-websocket, cron, certbot | **да** |

Журнал **персистентный**: `/var/log/journal` существует, `Storage=persistent`
(значение по умолчанию, в `journald.conf` строка закомментирована), поэтому
записи переживают ребут. Ограничений по объёму и сроку нет (`SystemMaxUse=`
пусто, `MaxRetentionSec=0`), объём пользовательского журнала на момент
проверки — 132 МБ.

Параллельно работает rsyslog и дублирует часть записей в
`/var/log/syslog` (139 МБ) — там, в частности, строки `CRON[...]`.

> Проверка «а что вообще пишется в журнал прямо сейчас»:
> `journalctl --user -f` (свои) и `sudo journalctl -f` (системные).

### Шпаргалка

| Что смотрим | Команда |
|-------------|---------|
| Immich API/worker | `journalctl --user -u immich-server -f` |
| Immich ML | `journalctl --user -u immich-ml -f` |
| Ночной фикс превью | `journalctl --user -u immich-nightfix.service` |
| Nextcloud (приложение) | `tail -f /mnt/data/nextcloud/nextcloud.log` |
| nginx | `sudo tail -f /var/log/nginx/error.log` |
| PostgreSQL | `sudo tail -f /var/log/postgresql/postgresql-18-main.log` |
| Redis | `sudo journalctl -u redis-server -f` |
| Whiteboard websocket | `sudo journalctl -u whiteboard-websocket -f` |
| Nextcloud Office (Collabora) | `sudo journalctl -u collabora-online -f` |
| Stalwart (сервис) | `journalctl --user -u stalwart -f` |
| Stalwart (SMTP, доставка) | `grep "delivery.delivered" /var/log/stalwart/stalwart.$(date +%F) \| sed 's/\x1b\[[0-9;]*m//g'` |
| Storage-control (веб-консоль) | `journalctl --user -u storage-control -f` |
| Storage-control (лог прогона) | `ls ~/.local/share/storage-control/runs/` |
| Бэкап БД и конфигов | `tail -f /mnt/backup/backups/logs/backup.log` |
| Зеркало дома | `journalctl --user -u backup-home-rsync.service` |
| Cron | `sudo grep CRON /var/log/syslog \| tail` |

### Immich

Всё в **пользовательском журнале**, отдельных файлов-логов нет.

| Юнит | Что в нём | Как смотреть |
|------|-----------|--------------|
| `immich-server.service` | Основной лог: NestJS, ошибки job handler'ов, миграции, `AssetDetectFaces` и прочие микросервисные задачи | `journalctl --user -u immich-server.service` |
| `immich-ml.service` | ML-инференс (распознавание лиц/объектов) | `journalctl --user -u immich-ml.service` |
| `immich-nightfix.service` | Ночной фикс превью (см. ниже) | `journalctl --user -u immich-nightfix.service` |

Живой пример ошибки, которую ищем при разборе превью:

```
окт 21 22:55:37 nas start.sh[19618]: [Nest] 19618 - ERROR [Microservices:{...}]
  Unable to run job handler (AssetDetectFaces): Error: Machine learning repository not been setup
```

**Ночной фикс превью.** Отдельного файла-лога нет: юнит не переопределяет
`StandardOutput=`, поэтому весь вывод уходит в журнал systemd. Обёртка
(`tools/immich/night_fix_wrapper.sh`) стримит вывод через `tee`, так что
прогресс виден в журнале в реальном времени и переживает убийство прогона.
Временный файл обёртки — **не лог**: это буфер для проверки маркера, удаляется
на выходе.

```bash
journalctl --user -u immich-nightfix.service            # последние записи
journalctl --user -u immich-nightfix.service --since "-7d"
```

### Nextcloud

У Nextcloud **два независимых источника**: собственный лог приложения и
системные логи веб-стека.

**Собственный лог приложения** (JSON, level 0-3, ротации нет — файл растёт):

```
/mnt/data/nextcloud/nextcloud.log
```

```bash
tail -f /mnt/data/nextcloud/nextcloud.log
grep -i "login failed" /mnt/data/nextcloud/nextcloud.log | tail
```

Пример записи:

```json
{"reqId":"l1YFNvrtfuJgKhxPZ8CI","level":2,"time":"2026-10-21T10:12:18+00:00",
 "user":"stas","app":"core","method":"POST","url":"/ocs/v2.php/apps/appstore/api/v1/apps/update",
 "message":"Login failed: 'stas' (Remote IP: '127.0.0.1')","version":"35.0.1.1"}
```

**Системные логи веб-стека** (нужен `sudo`):

| Компонент | Файл/источник | Ротация |
|-----------|---------------|---------|
| nginx (access) | `/var/log/nginx/access.log` | `access.log.1`, `access.log.N.gz` |
| nginx (error) | `/var/log/nginx/error.log` | по logrotate |
| PHP-FPM | `/var/log/php8.5-fpm.log` | `php8.5-fpm.log.1` |
| PostgreSQL | `/var/log/postgresql/postgresql-18-main.log` | `postgresql-18-main.log.1` |
| Redis | только журнал, файла `/var/log/redis` нет | — |

```bash
sudo tail -f /var/log/nginx/error.log
sudo tail -f /var/log/php8.5-fpm.log
sudo tail -f /var/log/postgresql/postgresql-18-main.log
sudo journalctl -u redis-server -f
```

**Фоновая задача Nextcloud** (`cron.php`, каждые 5 минут, crontab
пользователя `storage`): вывод не перенаправлен, поэтому cron отправляет его
по почте (`/var/mail` на практике пуст — cron.php молчит, если всё в порядке),
а факт выполнения пишется в `/var/log/syslog` строкой `CRON[...]: (storage)
CMD (/usr/bin/php -f /home/storage/nextcloud/cron.php)`. Ошибки самого
background job видны в `nextcloud.log`.

**occ** (`occ upgrade`, `occ maintenance:mode`) пишет вывод только в терминал —
в постоянный лог он не попадает, поэтому вывод `occ` стоит сохранять при
обновлениях вручную.

**Nextcloud Office (Collabora Online)** — отдельный системный юнит
`collabora-online.service` с `StandardOutput=journal`, то есть его вывод в
**системном журнале** (нужен `sudo`). Сам AppImage дополнительно может писать
`/tmp/coolwsd.<random>/coolwsd.log` — этот каталог в `/tmp`, поэтому лог
исчезает при ребуте.

```bash
sudo journalctl -u collabora-online.service -f
ls /tmp/coolwsd.*/coolwsd.log 2>/dev/null
```

### Whiteboard (websocket)

Системный юнит, вывод — в **системный журнал** (нужен `sudo`):

```bash
sudo journalctl -u whiteboard-websocket.service -f
```

Здесь же идут ошибки Puppeteer/Chromium (`[Puppeteer] Launch check failed`) —
по ним видно, что запись в whiteboard не работает.

Проверка websocket (нужен `--http1.1`, иначе curl уходит в HTTP/2 и socket.io
отвечает `400 {"code":3,"message":"Bad request"}`):

```bash
curl -sk --http1.1 --max-time 5 -H 'Upgrade: websocket' -H 'Connection: Upgrade' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==' \
  -o /dev/null -w '%{http_code}\n' \
  'https://drive.<DOMAIN>/whiteboard/socket.io/?EIO=4&transport=websocket'
```

### Stalwart

**Сервис** (старт, конфиг, ошибки listener'ов) — в пользовательском журнале:

```bash
journalctl --user -u stalwart.service -f
```

**SMTP и доставка писем в журнале НЕ логируются** — это отдельные файлы с
ротацией по дате:

```
/var/log/stalwart/stalwart.<YYYY-MM-DD>
```

```bash
# доставка (успех/провал) и результаты аутентификации
grep -E "delivery\.delivered|delivery\.message-rejected|smtp\.dkim-pass|smtp\.dmarc-pass|smtp\.iprev-pass|smtp\.spf-from-pass|smtp\.error" \
  /var/log/stalwart/stalwart.$(date +%F)
# убрать ANSI-цвета, чтобы читалось:
grep "delivery.delivered" /var/log/stalwart/stalwart.$(date +%F) | sed 's/\x1b\[[0-9;]*m//g'
```

Формат строки (цвета — ANSI escape, мешают grep'у по полям):

```
2026-10-19T18:38:44Z INFO Message delivered (delivery.delivered) queueId = 335869638392939520,
  from = "<ADMIN_EMAIL>", to = ["<USER_EMAIL>"], size = 2478,
  hostname = "gmail-smtp-in.l.google.com", code = 250, details = "OK"
```

Что на что смотреть:

| Событие | Значит |
|---------|--------|
| `delivery.delivered` / `Message delivered` | письмо принято сервером получателя (в поле `hostname` — его имя) |
| `delivery.message-rejected` | отказ, письмо не доставлено |
| `smtp.dkim-pass`, `smtp.dmarc-pass`, `smtp.iprev-pass`, `smtp.spf-from-pass` | проверки подписи прошли (у каждой есть и `*-fail`) |
| `smtp.error` | ошибка на уровне SMTP-сессии |
| `code = 250, details = "OK"` | поле появляется в **локальной** доставке (DSN внутрь сервера, `hostname = "localhost"`), не в приёме Gmail'ом |

По этим файлам проверяется SPF/DKIM/DMARC и факт приёма письма; в journald
этого нет.

### Storage-control (веб-консоль)

**Собственный лог приложения** — stdout gunicorn'а в пользовательском журнале.
Туда же уходит вывод APScheduler'а: ночного SMART-монитора и всех runner'ов
обновления/подготовки.

```bash
journalctl --user -u storage-control.service -f
```

**Логи конкретных прогонов** — отдельные файлы в каталоге данных
`$STORAGE_CONTROL_DATA/runs` (`~/.local/share/storage-control/runs`):

| Файл | Что внутри |
|------|-----------|
| `update_immich_<timestamp>.log` | Пошаговый лог апдейта immich (build, migrations, restart) |
| `update_nextcloud_<timestamp>.log` | Апдейт nextcloud |
| `update_stalwart_<timestamp>.log` | Апдейт stalwart |
| `prepare_immich_<n>.log` | Подготовка (preserve) перед апдейтом |
| `prepare_nextcloud_<n>.log` | То же для nextcloud |
| `prepare_stalwart_<n>.log` | То же для stalwart |

```bash
ls -lt ~/.local/share/storage-control/runs/ | head
tail -50 ~/.local/share/storage-control/runs/update_immich_*.log
```

Прогоны видны и в веб-интерфейсе (страница Updates), живой лог preserve
отдаётся по `/updates/preserve/<id>/log`.

**SMART** отдельным файлом не пишется: состояние дисков и активные аварии
хранятся в БД приложения (`app.db`, таблицы `SmartDiskState` / `SmartAlert`) и
показываются на дашборде (`/api/health/smart/status`); вывод самого
`smartctl` — в журнале storage-control.

### Бэкапы и фоновые задачи

| Задача | Куда пишет | Как смотреть |
|--------|-----------|--------------|
| `backup-immich-nc.service` (04:00) | **Файл**, не журнал: юнит перенаправляет stdout+stderr через `StandardOutput=append:` | `tail -f /mnt/backup/backups/logs/backup.log` |
| stderr `pg_dump`/`tar` | `/mnt/backup/backups/logs/*_pgdump_*.err`, `*_tar_*.err` | `ls /mnt/backup/backups/logs/*.err` |
| `backup-home-rsync.service` (05:00) | stdout — в пользовательский журнал; сам rsync — в свой файл | `journalctl --user -u backup-home-rsync.service` и `/mnt/backup/backups/home-storage/logs/rsync-<timestamp>.log` |
| `immich-nightfix.service` (03:30) | Пользовательский журнал | `journalctl --user -u immich-nightfix.service` |

Особенность `backup-immich-nc`: это **единственный** юнит, который пишет не в
журнал, а в файл, поэтому `journalctl --user -u backup-immich-nc` покажет
почти пусто — смотреть надо `backup.log`. Хвост файла содержит `ls -lht`
артефактов бэкапа.

Зеркало `/home/storage` целиком: `/mnt/backup/backups/home-storage/`
(`current/`, `deleted/`, `logs/`), ротация — по `KEEP_DAYS`.

Preserve-снапшоты: `/mnt/backup/backups/preserves/{immich,nextcloud,stalwart}/`.

### Система и ОС

| Что | Где | Команда |
|-----|-----|---------|
| Cron (все задачи) | `/var/log/syslog`, строки `CRON[...]` | `sudo grep CRON /var/log/syslog \| tail` |
| Cron (журнал systemd) | системный журнал | `sudo journalctl -u cron.service` |
| Certbot | только журнал, каталога `/var/log/letsencrypt` нет | `sudo journalctl -u certbot-renew.service` |
| Collabora Online | системный журнал + `/tmp/coolwsd.<random>/coolwsd.log` (в /tmp, исчезает при ребуте) | `sudo journalctl -u collabora-online -f` |
| sysstat (история нагрузки) | `/var/log/sysstat/saDD` | `sar -f /var/log/sysstat/sa21 -u` |
| RAID (md0) | журнал + `/proc/mdstat` | `sudo journalctl -u mdmonitor-oneshot.service`, `cat /proc/mdstat` |
| Ротация системных логов | `/etc/logrotate.d/` | `sudo cat /etc/logrotate.d/nginx` |

### Ротация и хранение — сводка

- **journald** — персистентный, без лимитов по объёму и сроку. Единственное
  место, где лежат логи immich, stalwart (сервис), storage-control, whiteboard,
  Collabora, nginx/php/postgres/redis (в виде записей юнитов), cron, certbot.
- **Файлы** — `/var/log/nginx/*`, `/var/log/php8.5-fpm.log*`,
  `/var/log/postgresql/*`, `/var/log/stalwart/stalwart.<дата>`,
  `/var/log/sysstat/saDD`, `/var/log/syslog` (rsyslog).
- **Приложения** — `/mnt/data/nextcloud/nextcloud.log` (ротации нет),
  `~/.local/share/storage-control/runs/*.log` (ротации нет).
- **Бэкап-диск** — `/mnt/backup/backups/logs/backup.log`,
  `/mnt/backup/backups/logs/*.err`,
  `/mnt/backup/backups/home-storage/logs/rsync-*.log`.

Кто что чистит:

| Что | Чем | Срок |
|-----|-----|------|
| `/var/log/nginx/*.log` | logrotate (`/etc/logrotate.d/nginx`) | daily, rotate 14, compress |
| `/var/log/php8.5-fpm.log` | logrotate | weekly, rotate 12 |
| `/var/log/postgresql/*.log` | logrotate (`postgresql-common`) | по конфигу пакета |
| `/var/log/letsencrypt/*.log` | logrotate | weekly, rotate 12 (сам каталог на хосте отсутствует) |
| `/var/log/stalwart/stalwart.<дата>` | сам stalwart | ротация по дате, старые файлы не вычищаются — см. `ls /var/log/stalwart/` |
| `/mnt/backup/backups/logs/*` (включая `backup.log`) | скрипт бэкапа: `find "$DEST/logs" -type f -mtime +14 -delete` | 14 дней (`KEEP_DAYS`) |
| `/mnt/backup/backups/{immich,nextcloud,stalwart}/*.dump` | тот же скрипт | 14 дней |
| `/mnt/backup/backups/home-storage/logs/rsync-*.log` и `deleted/` | скрипт зеркала | 30 дней (`KEEP_DAYS`) |
| `~/.local/share/storage-control/runs/*.log` | никто | растут, разбирать вручную |

> Осторожно с `find "$DEST/logs" -type f -mtime +14 -delete`: `backup.log` там
> единственный живой файл, он выживает только потому, что юнит дописывает его
> каждый день (mtime обновляется). Если бэкап перестанет ходить — файл уедет по
> ротации вместе с остальными.

## Полезные команды

```bash
# Статус сервисов
systemctl --user status immich-server immich-ml
systemctl status nginx postgresql redis-server php8.5-fpm whiteboard-websocket

# Логи (полная карта — в разделе «Журналы работы системы»)
journalctl --user -u immich-server -f
sudo journalctl -u whiteboard-websocket -f
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
| `wopi_url` | `https://drive.<DOMAIN>/apps/richdocumentscode/proxy.php?req=` |
| `public_wopi_url` | `https://drive.<DOMAIN>` |
| `wopi_allowlist` | `127.0.0.1/32` |
| `disable_certificate_verification` | (пусто = проверка включена) |

### Доступ к доменам из входящего VPN (роутер)

**Симптом:** с телефона под VPN (`<VPN_CLIENT_IP>`) на `drive/mail/photo.<DOMAIN>` — ошибка
сертификата, при этом в `/var/log/nginx/access.log` **ни одной** строки с `<VPN_CLIENT_IP>`,
а в `error.log` нет ни одного TLS-сбоя. По IP (`<NAS_LAN_IP>`) при этом всё открывается.

**Причина:** все три имени резолвятся в `<WAN_IP>` — это WAN-адрес **роутера**
(`ip route get <WAN_IP>` → `local ... table local`). Проброс портов на роутере
(`443→<NAS_LAN_IP>:8080`, `80→8081`) в fw4 разворачивается только для тех интерфейсов,
для которых есть переход в цепочке `dstnat`. Было:

```
chain dstnat {
    iifname "br-lan" jump dstnat_lan     # LAN + отражение (ip saddr <LAN_CIDR>)
    iifname "eth1"   jump dstnat_wan     # интернет
}
```

Ветки для `<VPN_IF>` не было, поэтому пакет из VPN не попадал в DNAT и доставался самому
роутеру — на `:443` отвечает LuCI с самоподписанным `CN=OpenWrt`, отсюда «ошибка
сертификата». Прямой маршрут `<VPN_CIDR> → <NAS_LAN_IP>` при этом работает
(`firewall.@forwarding[4].src='<VPN_ZONE>' → dest='lan'`), поэтому заход по IP NAS
проходил.

**Решение** — добавить DNAT для VPN-зоны `<VPN_ZONE>` с обязательной привязкой к адресу
назначения (`src_dip`). Без `src_dip` завернулся бы **весь** HTTPS из VPN на NAS:

```sh
uci set firewall.https_vpn=redirect
uci set firewall.https_vpn.name='https-vpn'
uci set firewall.https_vpn.target='DNAT'
uci set firewall.https_vpn.src='<VPN_ZONE>'
uci set firewall.https_vpn.src_dip='<WAN_IP>'
uci set firewall.https_vpn.src_dport='443'
uci set firewall.https_vpn.proto='tcp'
uci set firewall.https_vpn.dest='lan'
uci set firewall.https_vpn.dest_ip='<NAS_LAN_IP>'
uci set firewall.https_vpn.dest_port='8080'

uci set firewall.http_vpn=redirect
uci set firewall.http_vpn.name='http-vpn'
uci set firewall.http_vpn.target='DNAT'
uci set firewall.http_vpn.src='<VPN_ZONE>'
uci set firewall.http_vpn.src_dip='<WAN_IP>'
uci set firewall.http_vpn.src_dport='80'
uci set firewall.http_vpn.proto='tcp'
uci set firewall.http_vpn.dest='lan'
uci set firewall.http_vpn.dest_ip='<NAS_LAN_IP>'
uci set firewall.http_vpn.dest_port='8081'

uci commit firewall && /etc/init.d/firewall reload
```

Проверка (должны появиться `iifname "<VPN_IF>" jump dstnat_<VPN_ZONE>` и оба правила
с `ip daddr <WAN_IP>`):

```sh
nft list ruleset | sed -n '/chain dstnat {/,/^}/p' | grep <VPN_IF>
nft list ruleset | sed -n '/chain dstnat_<VPN_ZONE>/,/^}/p'
```

Признак успеха на NAS — строки в `access.log` с `host="<домен>"` и `sni="<домен>"`
от `<VPN_CLIENT_IP>` (в логе для этого добавлены `host`, `port`, `scheme`, `sni`).
Настроено и проверено 2026-10-22.

### Проблема с NAT loopback и решение

**Симптом:** Фоновое задание `OCA\Richdocuments\Backgroundjobs\ObtainCapabilities` (cron 04:05) падает с ошибкой:
```
Failed to fetch discovery: cURL error 28: Operation timed out after 5001 milliseconds
for https://drive.<DOMAIN>/apps/richdocumentscode/proxy.php?req=/hosting/discovery
```

**Причина:** Cron-процесс (www-data) пытается подключиться к внешнему домену `drive.<DOMAIN>`, который резолвится в внешний IP (<WAN_IP>). Запрос уходит через роутер и возвращается обратно (NAT loopback), но роутер/файрвол блокирует или задерживает его.

**Решение:** Добавить локальную запись в `/etc/hosts`:
```bash
echo "127.0.0.1 drive.<DOMAIN>" | sudo tee -a /etc/hosts
```

После этого запрос разрешается локально и идёт через локальный nginx:
`localhost:443` (внешний nginx) → `127.0.0.1:8443` (внутренний Nextcloud).

**Проверка:**
```bash
curl -k https://drive.<DOMAIN>/apps/richdocumentscode/proxy.php?req=/hosting/discovery
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
curl -k https://drive.<DOMAIN>/apps/richdocumentscode/proxy.php?req=/hosting/discovery
```

**Версия совместимости:**
- Nextcloud 26/27 (richdocuments 12.x) → Collabora 26.04.x

---

*Документация актуальна на: $(date +%F)*

# stoage_system_tools

Репозиторий документации и проекта `storage-control` — консоли мониторинга и управления NAS (**Immich + Nextcloud + Stalwart**).

- `doc/system.md` — актуальная системная документация (хосты, пути, IP, сервисы).
- `doc/pnan.md` — план реализации.
- `storage-control/` — приложение (Flask) и README с подробностями.
- `tools/` — установка/удаление `storage-control` как systemd user service.

Запуск приложения: см. `storage-control/README.md`.

## Состав стека

| Сервис | Роль | Мониторинг в storage-control |
|--------|------|------------------------------|
| **Immich** | Фото/видео библиотека | Health, версия, обновления, БД бэкапы |
| **Nextcloud** | Файлы, Calendar, Contacts, Whiteboard | Health, версия, обновления, БД бэкапы |
| **Stalwart** | Почта (JMAP/IMAP/SMTP) | Health, версия, обновления, SMTP smoke-test |
| **PostgreSQL** | БД для Immich/Nextcloud/Stalwart | Бэкапы, статус сервиса |
| **Redis** | Кэш/сессии | Статус сервиса |
| **Nginx** | Reverse proxy, TLS терминация | Статус сервиса |
| **PHP-FPM** | Nextcloud runtime | Статус сервиса |

## Dashboard фичи

- **App Health**: Immich / Nextcloud / Stalwart — текущая/доступная версия, статус сервисов
- **Host**: CPU, RAM, Swap, Uptime, Температуры (CPU/NVMe/диски)
- **Disks**: использование файловых систем (фильтр: только физические block-устройства)
- **SMART**: таблица с Reallocated/Pending/Offline/ATA Errors, **baseline tracking**, ручной Refresh, история
- **RAID**: mdadm статус /dev/md0
- **Services**: systemd статусы с группировкой и зависимостями

## Ключевые изменения (2026-10)

- **SMART analytics v2**: baseline counters при Resolve, алерты только при росте счётчиков, жирный шрифт для увеличенных, кнопка "Refresh SMART Info", история измерений
- **Stalwart** интегрирован в health/updates/dashboard
- **Favicon**: анимированный SVG + PWA manifest
- **Миграции**: Alembic для схемы БД
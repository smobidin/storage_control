#!/bin/bash
# Бэкап БД и конфигов Immich + Nextcloud на /mnt/backup/backups
# Запускается systemd user-timer backup-immich-nc.timer (ежедневно 04:00)
#
# Скрипт живёт в репозитории storage_control: рабочие копии в ~/immich/tools
# (писала в несуществующий /mnt/recovery) и ~/tools просто разъезжались.
# Полная копия самого домашнего каталога идёт отдельно — backup-home-rsync.timer
# в 05:00; здесь только то, что нужно для быстрого восстановления сервисов.
set -uo pipefail

DEST=/mnt/backup/backups
KEEP_DAYS=14
STAMP=$(date +%F_%H%M)

log() { echo "[$(date '+%F %T')] $*"; }

# 1. Диск должен быть смонтирован
if ! mountpoint -q /mnt/backup; then
    log "ERROR: /mnt/backup не смонтирован — бэкап пропущен"
    exit 1
fi

mkdir -p "$DEST/immich" "$DEST/nextcloud" "$DEST/stalwart" "$DEST/logs"

# 2. Immich: дамп БД (custom format, сжимается, восстановление pg_restore)
log "pg_dump immich..."
if pg_dump -h localhost -U storage -d immich -Fc -f "$DEST/immich/immich_db_$STAMP.dump" 2>"$DEST/logs/immich_pgdump_$STAMP.err"; then
    log "OK: $(du -h "$DEST/immich/immich_db_$STAMP.dump" | cut -f1)"
else
    log "ERROR: pg_dump immich упал, см. logs"
fi

# 3. Immich: настройки (.env + systemd units + nightfix)
log "Копирую настройки immich..."
tar czf "$DEST/immich/immich_config_$STAMP.tar.gz" \
    -C /home/storage \
    immich/.env \
    .config/systemd/user/immich-server.service \
    .config/systemd/user/immich-ml.service \
    .config/systemd/user/immich-nightfix.service \
    .config/systemd/user/immich-nightfix.timer \
    dev/stoage_system_tools/tools/immich/night_fix_thumbnails.cjs \
    dev/stoage_system_tools/tools/immich/night_fix_wrapper.sh \
    2>"$DEST/logs/immich_tar_$STAMP.err" || log "WARN: tar immich вернул не 0"

# 4. Nextcloud: дамп БД
log "pg_dump nextcloud_db..."
if pg_dump -h localhost -U nextcloud_db -d nextcloud_db -Fc -f "$DEST/nextcloud/nextcloud_db_$STAMP.dump" 2>"$DEST/logs/nextcloud_pgdump_$STAMP.err"; then
    log "OK: $(du -h "$DEST/nextcloud/nextcloud_db_$STAMP.dump" | cut -f1)"
else
    log "ERROR: pg_dump nextcloud упал, см. logs"
fi

# 5. Nextcloud: настройки (config.php)
log "Копирую настройки nextcloud..."
tar czf "$DEST/nextcloud/nextcloud_config_$STAMP.tar.gz" \
    -C /home/storage/nextcloud config/config.php \
    2>"$DEST/logs/nextcloud_tar_$STAMP.err" || log "WARN: tar nextcloud вернул не 0"

# 5a. Stalwart: данные (RocksDB) + конфиг/юнит. Останавливаем сервис, чтобы дамп был консистентным.
log "Бэкап Stalwart..."
STALWART_WAS_RUNNING=0
if systemctl --user is-active --quiet stalwart; then
    STALWART_WAS_RUNNING=1
    systemctl --user stop stalwart
fi
if [ -d /mnt/data/stalwart ]; then
    if tar czf "$DEST/stalwart/stalwart_data_$STAMP.tar.gz" -C /mnt/data stalwart 2>"$DEST/logs/stalwart_data_$STAMP.err"; then
        log "OK: $(du -h "$DEST/stalwart/stalwart_data_$STAMP.tar.gz" | cut -f1)"
    else
        log "ERROR: tar stalwart data упал, см. logs"
    fi
fi
tar czf "$DEST/stalwart/stalwart_config_$STAMP.tar.gz" \
    -C /home/storage \
    .config/systemd/user/stalwart.service \
    stalwart/etc \
    2>"$DEST/logs/stalwart_config_$STAMP.err" || log "WARN: tar stalwart config вернул не 0"
if [ "$STALWART_WAS_RUNNING" = "1" ]; then
    systemctl --user start stalwart || log "WARN: не удалось поднять stalwart после бэкапа"
fi

# 6. nginx + SSL конфиги
log "Копирую nginx конфиги..."
tar czf "$DEST/nginx_config_$STAMP.tar.gz" \
    -C /etc \
    nginx/sites-available/proxy-sites \
    nginx/sites-available/nextcloud \
    nginx/conf.d/websocket.conf \
    letsencrypt/live/photo.itexperts.team/ \
    2>"$DEST/logs/nginx_tar_$STAMP.err" || log "WARN: tar nginx вернул не 0"

# 7. PostgreSQL конфиг
log "Копирую postgresql.conf..."
sudo tar czf "$DEST/postgres_config_$STAMP.tar.gz" \
    -C /etc/postgresql/18/main \
    postgresql.conf pg_hba.conf \
    2>"$DEST/logs/postgres_tar_$STAMP.err" || log "WARN: tar postgres вернул не 0"

# 8. Юниты бэкапов: без них дампы и зеркало никто не будет делать
log "Копирую юниты бэкапов..."
tar czf "$DEST/backup_units_$STAMP.tar.gz" \
    -C /home/storage/.config/systemd/user \
    backup-immich-nc.service \
    backup-immich-nc.timer \
    backup-home-rsync.service \
    backup-home-rsync.timer \
    stalwart.service \
    2>"$DEST/logs/backup_units_tar_$STAMP.err" || log "WARN: tar backup units вернул не 0"

# 9. Список установленных пакетов
dpkg --get-selections > "$DEST/pkg_list_$STAMP.txt" 2>/dev/null || true

# 10. Ротация: старше KEEP_DAYS дней — удалить
log "Ротация: удаляю бэкапы старше $KEEP_DAYS дней..."
find "$DEST/immich" "$DEST/nextcloud" "$DEST/stalwart" -name "*.dump" -mtime +$KEEP_DAYS -delete 2>/dev/null
find "$DEST/immich" "$DEST/nextcloud" "$DEST/stalwart" -name "*.tar.gz" -mtime +$KEEP_DAYS -delete 2>/dev/null
find "$DEST" -maxdepth 1 -name "*.tar.gz" -mtime +$KEEP_DAYS -delete 2>/dev/null
find "$DEST" -maxdepth 1 -name "pkg_list_*.txt" -mtime +$KEEP_DAYS -delete 2>/dev/null
find "$DEST/logs" -type f -mtime +$KEEP_DAYS -delete 2>/dev/null

# 11. Итог
log "=== Готово ==="
df -h /mnt/backup | tail -1
ls -lht "$DEST/immich" | head -3
ls -lht "$DEST/nextcloud" | head -3

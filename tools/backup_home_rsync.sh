#!/bin/bash
# Ежедневное rsync-зеркало /home/storage на резервный диск /mnt/backup.
#
# Зачем: системный диск (/dev/sde2) с проблемами, а именно на нём живёт
# домашняя директория. Медиафайлы лежат отдельно на /mnt/data (RAID) и сюда
# не попадают — копируется только сам дом: конфиги, репозитории, БД-дампы
# (их делает отдельный backup-immich-nc.timer в 04:00), ключи, скрипты.
#
# Схема назначения:
#   $DEST/current/   — зеркало /home/storage (последнее состояние)
#   $DEST/deleted/   --backup-dir: файлы, удалённые или перезаписанные
#                       в источнике, складываются сюда, а не пропадают
#   $DEST/logs/      — логи rsync и итоги прогонов
#
# Запускается user-таймером backup-home-rsync.timer (05:00). Root не нужен:
# все файлы в /home/storage принадлежат пользователю storage.
set -uo pipefail

SRC=/home/storage/
MNT=/mnt/backup
DEST=$MNT/backups/home-storage
CURRENT=$DEST/current
DELETED=$DEST/deleted
LOGDIR=$DEST/logs

# Сколько хранить удалённые/перезаписанные файлы и старые логи.
KEEP_DAYS=30
# Минимум свободного места на назначении, ниже — только предупреждение.
MIN_FREE_GB=20
STAMP=$(date +%F_%H%M%S)

log() { echo "[$(date '+%F %T')] $*"; }

# 1. Резервный диск должен быть смонтирован — иначе бэкап бессмыслен.
if ! mountpoint -q "$MNT"; then
    log "ERROR: $MNT не смонтирован — бэкап пропущен"
    exit 1
fi

mkdir -p "$CURRENT" "$DELETED" "$LOGDIR" || { log "ERROR: нет $DEST"; exit 1; }

# 2. Место: при нехватке прогон всё равно идёт, но громко предупреждаем.
free_gb=$(df -BG --output=avail "$MNT" | tail -1 | tr -dc '0-9')
if [ "${free_gb:-0}" -lt "$MIN_FREE_GB" ]; then
    log "WARN: на $MNT свободно ${free_gb}G (порог ${MIN_FREE_GB}G)"
fi

# 3. Зеркало.
#   -aHAX   : сохранение прав, жёстких ссылок, ACL, xattr
#   --delete: назначение должно отражать источник (иначе удалённые файлы
#             копились бы вечно), но удаляемое попадает в --backup-dir
#   --partial: докачка прерванных файлов
#   исключаем только регенерируемый кэш (3.1G: immich_ml, playwright, pnpm, uv).
#   Паттерн БЕЗ обратного слэша: '/\.cache/' rsync трактует как литеральный
#   backslash в пути и не исключает ничего — проверено на прогоне, где вся
#   .cache (3.6G) уехала в зеркало. Ведущий / якорит на корень переноса,
#   поэтому вложенные node_modules/.cache остаются.
log "rsync $SRC -> $CURRENT ..."
rsync -aHAX --numeric-ids --delete --partial \
    --backup --backup-dir="$DELETED/$STAMP" \
    --exclude='/.cache/' \
    --stats --human-readable \
    --log-file="$LOGDIR/rsync-$STAMP.log" \
    "$SRC" "$CURRENT/"
rc=$?

case $rc in
    0)   log "OK: зеркало обновлено" ;;
    24)  log "WARN: часть файлов исчезла во время копирования (rc=24) — это нормально при живой системе" ;;
    *)   log "ERROR: rsync вернул $rc, см. $LOGDIR/rsync-$STAMP.log" ;;
esac

# 4. Ротация удалённого и старых логов.
find "$DELETED" -mindepth 1 -maxdepth 1 -type d -mtime +$KEEP_DAYS -exec rm -rf {} + 2>/dev/null
find "$LOGDIR" -name 'rsync-*.log' -mtime +$KEEP_DAYS -delete 2>/dev/null

# 5. Итог.
log "=== Итог ==="
du -sh "$CURRENT" 2>/dev/null
df -h "$MNT" | tail -1
log "удалённых/перезаписанных за $KEEP_DAYS дней: $(du -sh "$DELETED" 2>/dev/null | cut -f1)"

exit $rc

#!/usr/bin/env node
/**
 * Оптимизированный ночной фикс превьюшек Immich.
 * Теперь sharp нативно обрабатывает HEIC/HEIF (libheif 1.23.5 + libde265).
 * Остаются только форматы, которые sharp НЕ умеет читать:
 *   - RAW: CR2, NEF, ARW, DNG, ORF, RAF, RW2, PEF, SRW
 *   - BMP (битые заголовки), PSD/PSB
 *   - Видео: MOV, MP4, MKV, WEBM, AVI, 3GP, M4V, MTS, M2TS
 *
 * ВАЖНО: скрипт трогает ТОЛЬКО ассеты, у которых превью вообще нет
 * (нет записи в asset_file с type=preview/isEdited=false либо файла на диске).
 * Уже существующие превью не перезаписываются — их теперь умеет генерировать
 * сам Immich (в том числе с EXIF-ориентацией, см. IMMICH_PROCESS_INVALID_IMAGES).
 *
 * Запуск: DB_URL=postgresql://... node night_fix_thumbnails.cjs [--dry-run] [--concurrency=N]
 */
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { createRequire } = require('node:module');
const { pathToFileURL } = require('node:url');

const IMMICH_SRC = '/home/storage/immich/src';
const IMMICH_SERVER = path.join(IMMICH_SRC, 'server');
const immichRequire = createRequire(path.join(IMMICH_SERVER, 'package.json'));

const sharp = immichRequire('sharp');
const { Client } = immichRequire('pg');

// Константы
const THUMBS_ROOT = '/mnt/data/immich/library/thumbs';
const USER_ID = '9452cd5c-87c4-47fd-a861-aad0b36a1552';
const TMP_DIR = '/tmp/nightfix';

// Форматы, которые sharp теперь умеет нативно (HEIC/HEIF убраны)
const NATIVE_FORMATS = new Set([
  'jpg', 'jpeg', 'png', 'webp', 'gif', 'tiff', 'tif', 'avif', 'heic', 'heif',
  'bmp', 'jxl', 'jp2', 'j2k', 'jpf', 'jpx', 'jpm', 'mj2'
]);

// Форматы, требующие внешние инструменты
const RAW_FORMATS = new Set(['cr2', 'nef', 'arw', 'dng', 'orf', 'raf', 'rw2', 'pef', 'srw']);
const VIDEO_FORMATS = new Set(['mov', 'mp4', 'mkv', 'webm', 'avi', '3gp', 'm4v', 'mts', 'm2ts']);
const PSD_FORMATS = new Set(['psd', 'psb']);

// EXIF-ориентация -> трансформация sharp.
// Совпадает с ORIENTATION_TO_SHARP_ROTATION в server/src/constants.ts в Immich,
// чтобы превью совпадали с тем, что генерирует сам сервер.
const ORIENTATION_TO_SHARP_ROTATION = {
  1: { angle: 0 },
  2: { angle: 0, flop: true },
  3: { angle: 180 },
  4: { angle: 180, flop: true },
  5: { angle: 270, flip: true },
  6: { angle: 90 },
  7: { angle: 90, flip: true },
  8: { angle: 270 },
};

// CLI args
const DRY_RUN = process.argv.includes('--dry-run');
const CONCURRENCY = parseInt(process.argv.find(a => a.startsWith('--concurrency='))?.split('=')[1] || '2', 10);

// Находим exiftool (лениво, только при необходимости)
let ET = null;
function getExiftool() {
  if (ET) return ET;
  const base = path.join(IMMICH_SRC, 'node_modules', '.pnpm');
  const dirs = fs.readdirSync(base)
    .filter(d => d.startsWith('exiftool-vendored.pl@'))
    .sort();
  if (dirs.length === 0) throw new Error('exiftool-vendored.pl not found in ' + base);
  const bin = path.join(base, dirs.at(-1), 'node_modules', 'exiftool-vendored.pl', 'bin', 'exiftool');
  if (!fs.existsSync(bin)) throw new Error('exiftool binary not found: ' + bin);
  ET = bin;
  return ET;
}

// Убедимся, что temp директория существует
if (!fs.existsSync(TMP_DIR)) fs.mkdirSync(TMP_DIR, { recursive: true });

/** Очистка временных файлов для одного ассета */
function cleanupTemp(baseName) {
  const exts = ['', '.ppm', '.src', '.fixed.bmp', '.jpg', '.rawpreview'];
  for (const ext of exts) {
    try { fs.unlinkSync(path.join(TMP_DIR, baseName + ext)); } catch {}
  }
}

/**
 * Применяет EXIF-ориентацию исходника к превью.
 * Встроенное превью RAW лежит в "сенсорной" ориентации и не содержит тега
 * Orientation, поэтому без этого фото в UI отображается боком.
 */
async function applyOrientation(srcPath, dstPath, orientation) {
  const { angle = 0, flip = false, flop = false } = ORIENTATION_TO_SHARP_ROTATION[orientation] ?? {};
  if (!angle && !flip && !flop) {
    fs.copyFileSync(srcPath, dstPath);
    return;
  }
  let pipeline = sharp(srcPath).rotate(angle);
  if (flip) pipeline = pipeline.flip();
  if (flop) pipeline = pipeline.flop();
  await pipeline.jpeg({ quality: 90 }).toFile(dstPath);
}

/** Один проход sharp: preview + thumbnail + thumbhash за раз */
async function processWithSharp(srcPath) {
  // Читаем метаданные один раз
  const meta = await sharp(srcPath).metadata();

  // Параллельно генерируем все три варианта
  const [previewBuf, thumbBuf, { data, info }] = await Promise.all([
    sharp(srcPath).resize(1440, 1440, { fit: 'inside', withoutEnlargement: true }).jpeg({ quality: 80, progressive: false }).toBuffer(),
    sharp(srcPath).resize(250, 250, { fit: 'inside', withoutEnlargement: true }).webp({ quality: 80 }).toBuffer(),
    sharp(srcPath).resize(100, 100, { fit: 'inside', withoutEnlargement: true }).raw().ensureAlpha().toBuffer({ resolveWithObject: true })
  ]);

  const { rgbaToThumbHash } = await import(pathToFileURL(immichRequire.resolve('thumbhash')));
  const thumbhash = Buffer.from(rgbaToThumbHash(info.width, info.height, data));

  return { previewBuf, thumbBuf, thumbhash, meta };
}

/** Декодирует один ассет во временный JPEG */
async function decodeToJpeg(asset) {
  const src = asset.originalPath;
  const ext = src.toLowerCase().split('.').pop();
  const baseName = `nightfix_${asset.id}`;
  const tmpImg = path.join(TMP_DIR, baseName + '.jpg');

  try {
    // 1. Нативные форматы — читаем sharp напрямую, конвертируем в JPEG
    if (NATIVE_FORMATS.has(ext)) {
      await sharp(src).jpeg({ quality: 90 }).toFile(tmpImg);
      return tmpImg;
    }

    // 2. RAW: пробуем встроенный PreviewImage через exiftool,
    //    иначе — полная демозаика через dcraw + ImageMagick.
    //    EXIF-ориентацию исходника применяем сами: превью встроено в
    //    "сенсорной" ориентации и без этого отображается боком.
    if (RAW_FORMATS.has(ext)) {
      const rawPreview = path.join(TMP_DIR, baseName + '.rawpreview');
      let buf = Buffer.alloc(0);
      try {
        buf = execFileSync('perl', [getExiftool(), '-b', '-PreviewImage', src], { maxBuffer: 100 * 1024 * 1024 });
      } catch { /* игнорируем ошибки exiftool */ }
      if (buf.length >= 1000) {
        fs.writeFileSync(rawPreview, buf);
      } else {
        // Нет превью — полная демозаика через dcraw + ImageMagick
        const ppm = execFileSync('dcraw', ['-c', '-w', '-q', '3', src], { maxBuffer: 200 * 1024 * 1024 });
        const ppmPath = path.join(TMP_DIR, baseName + '.ppm');
        fs.writeFileSync(ppmPath, ppm);
        try {
          execFileSync('convert', [ppmPath, '-quality', '90', rawPreview], { stdio: 'pipe' });
        } finally {
          try { fs.unlinkSync(ppmPath); } catch {}
        }
      }
      try {
        await applyOrientation(rawPreview, tmpImg, asset.orientation);
        return tmpImg;
      } finally {
        try { fs.unlinkSync(rawPreview); } catch {}
      }
    }

    // 3. BMP: пробуем ImageMagick, при битом заголовке — чиним cbSize
    if (ext === 'bmp') {
      try {
        execFileSync('convert', [src, '-quality', '90', tmpImg], { stdio: 'pipe' });
        return tmpImg;
      } catch {
        const buf = fs.readFileSync(src);
        if (buf.length >= 54 && buf.toString('ascii', 0, 2) === 'BM') {
          const fixed = Buffer.from(buf);
          fixed.writeUInt32LE(buf.length, 2);
          const fixedSrc = path.join(TMP_DIR, baseName + '.fixed.bmp');
          fs.writeFileSync(fixedSrc, fixed);
          try {
            execFileSync('convert', [fixedSrc, '-quality', '90', tmpImg], { stdio: 'pipe' });
            return tmpImg;
          } finally {
            try { fs.unlinkSync(fixedSrc); } catch {}
          }
        }
        throw new Error('BMP decode failed');
      }
    }

    // 4. PSD/PSB: композитный кадр через ImageMagick
    if (PSD_FORMATS.has(ext)) {
      execFileSync('convert', [src + '[0]', '-quality', '90', tmpImg], { stdio: 'pipe' });
      return tmpImg;
    }

    // 5. Видео: первый кадр через ffmpeg
    if (VIDEO_FORMATS.has(ext)) {
      execFileSync('ffmpeg', ['-y', '-v', 'error', '-i', src, '-frames:v', '1', '-q:v', '2', tmpImg], { stdio: 'pipe' });
      return tmpImg;
    }

    // 6. Неизвестное расширение — пробуем sharp напрямую
    await sharp(src).jpeg({ quality: 90 }).toFile(tmpImg);
    return tmpImg;
  } catch (e) {
    cleanupTemp(baseName);
    throw e;
  }
}

/** Обрабатывает один ассет */
async function fixAsset(client, asset) {
  const src = asset.originalPath;
  const ext = src.toLowerCase().split('.').pop();
  const baseName = `nightfix_${asset.id}`;

  try {
    // Декодируем во временный JPEG
    const tmpImg = await decodeToJpeg(asset);

    // Проверка что файл создался и не пустой
    const stat = fs.statSync(tmpImg);
    if (stat.size < 100) throw new Error('decode produced nothing');

    // Генерируем preview/thumbnail/thumbhash
    const { previewBuf, thumbBuf, thumbhash } = await processWithSharp(tmpImg);

    if (!DRY_RUN) {
      // Записываем файлы
      const dir = path.join(THUMBS_ROOT, USER_ID, asset.id.slice(0, 2), asset.id.slice(2, 4));
      fs.mkdirSync(dir, { recursive: true });
      const previewPath = path.join(dir, `${asset.id}_preview.jpeg`);
      const thumbPath = path.join(dir, `${asset.id}_thumbnail.webp`);
      fs.writeFileSync(previewPath, previewBuf);
      fs.writeFileSync(thumbPath, thumbBuf);

      // Обновляем БД.
      // (assetId, type, isEdited) — уникальный индекс, поэтому upsert, а не insert:
      // у ассета может уже быть строка thumbnail, но не быть preview.
      const upsertFile = (type, filePath) =>
        client.query(
          `INSERT INTO asset_file (id, "assetId", type, path, "createdAt", "updatedAt", "isEdited", "isProgressive", "isTransparent")
           VALUES (gen_random_uuid(), $1, $2, $3, now(), now(), false, false, false)
           ON CONFLICT ("assetId", type, "isEdited")
           DO UPDATE SET path = EXCLUDED.path, "updatedAt" = now()`,
          [asset.id, type, filePath]
        );
      await upsertFile('preview', previewPath);
      await upsertFile('thumbnail', thumbPath);

      await client.query(
        `UPDATE asset SET thumbhash = $1, visibility = CASE WHEN visibility = 'hidden' THEN 'timeline' ELSE visibility END WHERE id = $2`,
        [thumbhash, asset.id]
      );
    }

    // Чистим temp
    cleanupTemp(baseName);
    return true;
  } catch (e) {
    cleanupTemp(baseName);
    const msg = String(e.message).split('\n')[0];
    console.error(`FAIL ${asset.id} (${ext}): ${msg}`);
    return false;
  }
}

/** Очередь с ограничением параллелизма */
async function runQueue(assets, client) {
  let ok = 0, fail = 0;
  let index = 0;

  async function worker() {
    while (index < assets.length) {
      const i = index++;
      const asset = assets[i];
      const result = await fixAsset(client, asset);
      if (result) ok++; else fail++;
      // Прогресс каждые 10 или в конце
      if ((i + 1) % 10 === 0 || i === assets.length - 1) {
        console.log(`  progress: ${i + 1}/${assets.length} (ok: ${ok}, fail: ${fail})`);
      }
    }
  }

  const workers = Array.from({ length: Math.min(CONCURRENCY, assets.length) }, () => worker());
  await Promise.all(workers);
  return { ok, fail };
}

/**
 * Выбирает ассеты, у которых превью ВООБЩЕ нет:
 * нет записи в asset_file (type=preview, isEdited=false) либо файла на диске.
 * Всё, у чего превью уже есть, не трогаем — его генерирует сам Immich.
 */
async function selectTargets(client) {
  const { rows } = await client.query(
    `SELECT a.id, a."originalPath", e.orientation, f.path AS preview_path
       FROM asset a
       LEFT JOIN asset_exif e ON e."assetId" = a.id
       LEFT JOIN asset_file f ON f."assetId" = a.id AND f.type = 'preview' AND f."isEdited" = false
      WHERE a."deletedAt" IS NULL`
  );
  return rows.filter((a) => !a.preview_path || !fs.existsSync(a.preview_path));
}

async function main() {
  console.log(`Nightfix started (concurrency=${CONCURRENCY}, dryRun=${DRY_RUN})`);
  console.log(`Sharp: ${sharp.versions.sharp}, libvips: ${sharp.versions.vips}, heif: ${sharp.versions.heif}`);

  const client = new Client({ connectionString: process.env.DB_URL });
  await client.connect();

  const assets = await selectTargets(client);

  if (assets.length === 0) {
    console.log('nothing to fix');
    await client.end();
    process.exit(0);
  }

  console.log(`assets to fix: ${assets.length}`);

  const { ok, fail } = await runQueue(assets, client);

  const left = (await selectTargets(client)).length;
  console.log(`done: ${ok} fixed, ${fail} failed, ${left} still without preview`);
  await client.end();

  if (fail > 0 || left > 0) {
    process.exit(1);
  }
  process.exit(0);
}

main().catch(e => { console.error(e); process.exit(1); });
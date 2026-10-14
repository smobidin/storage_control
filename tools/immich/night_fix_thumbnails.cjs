#!/usr/bin/env node
/**
 * Ночной фикс превьюшек Immich для Samsung HEIC (пока sharp не обновился до libheif 1.23.4).
 * Находит ассеты без thumbhash, декодирует их libheif 1.23.4/dcraw/sharp,
 * генерирует preview/thumbnail/thumbhash и пишет в БД + файлы.
 * Выход: 0 = работы не было или всё починено; 1 = что-то не починилось.
 *
 * Файл намеренно лежит в репозитории storage_control, а не в ~/immich/src:
 * Preserve делает `git stash push -u`, который уносит untracked-файлы immich,
 * и скрипт исчезал из рабочего дерева (так и произошло 2026-10-14).
 * Зависимости (sharp/pg/thumbhash/exiftool) берутся из node_modules immich-server
 * через createRequire: NODE_PATH не действует на динамический import(),
 * а захардкоженные пути .pnpm меняются после каждого pnpm install.
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

const HEIF_DEC = '/home/storage/immich/tools/libheif-1.23.4/heif-dec';
const HEIF_LIB = '/home/storage/immich/tools/libheif-1.23.4/lib';
const THUMBS_ROOT = '/mnt/data/immich/library/thumbs';
const USER_ID = '9452cd5c-87c4-47fd-a861-aad0b36a1552';

/** exiftool-vendored.pl — транзитивная зависимость: резолвится только по .pnpm,
 *  где версия в имени каталога меняется при каждом pnpm install. Берём свежую. */
function findExiftool() {
  const base = path.join(IMMICH_SRC, 'node_modules', '.pnpm');
  const dirs = fs.readdirSync(base)
    .filter((d) => d.startsWith('exiftool-vendored.pl@'))
    .sort();
  if (dirs.length === 0) throw new Error('exiftool-vendored.pl not found in ' + base);
  const bin = path.join(base, dirs.at(-1), 'node_modules', 'exiftool-vendored.pl', 'bin', 'exiftool');
  if (!fs.existsSync(bin)) throw new Error('exiftool binary not found: ' + bin);
  return bin;
}

const ET = findExiftool();

async function fixAsset(client, asset) {
  const src = asset.originalPath;
  const ext = src.toLowerCase().split('.').pop();
  const tmpImg = `/tmp/nightfix_${asset.id}.jpg`;
  try {
    if (ext === 'heic' || ext === 'heif') {
      // Samsung HEIC: декодируем libheif 1.23.4
      execFileSync(HEIF_DEC, [src, tmpImg], {
        stdio: 'pipe',
        env: { ...process.env, LD_LIBRARY_PATH: HEIF_LIB },
      });
    } else if (ext === 'dng' || ext === 'arw' || ext === 'nef' || ext === 'cr2' || ext === 'raf' || ext === 'orf') {
      // RAW (вкл. Olympus ORF): сначала пробуем встроенный PreviewImage через exiftool
      let buf = Buffer.alloc(0);
      try {
        buf = execFileSync('perl', [ET, '-b', '-PreviewImage', src], { maxBuffer: 100 * 1024 * 1024 });
      } catch (e) { /* exiftool может упасть на битом файле — идём в dcraw */ }
      if (buf.length < 1000) {
        // Нет встроенного превью: демозаиким dcraw → PPM → ImageMagick → JPEG
        const ppm = execFileSync('dcraw', ['-c', '-w', '-q', '3', src], { maxBuffer: 200 * 1024 * 1024 });
        fs.writeFileSync(tmpImg + '.ppm', ppm);
        try {
          execFileSync('convert', [tmpImg + '.ppm', '-quality', '90', tmpImg]);
        } finally {
          fs.unlinkSync(tmpImg + '.ppm');
        }
      } else {
        fs.writeFileSync(tmpImg, buf);
      }
    } else if (ext === 'bmp') {
      // BMP: sharp его не читает — конвертируем напрямую через ImageMagick.
      // Если заголовок битый (cbSize != реальный размер), ImageMagick падает —
      // тогда пересобираем cbSize на фактический размер файла (данные не трогаем).
      try {
        execFileSync('convert', [src, '-quality', '90', tmpImg], { stdio: 'pipe' });
      } catch (e) {
        const buf = fs.readFileSync(src);
        if (buf.length >= 54 && buf.toString('ascii', 0, 2) === 'BM') {
          const fixed = Buffer.from(buf);
          fixed.writeUInt32LE(buf.length, 2); // cbSize = фактический размер
          const fixedSrc = tmpImg + '.fixed.bmp';
          fs.writeFileSync(fixedSrc, fixed);
          try {
            execFileSync('convert', [fixedSrc, '-quality', '90', tmpImg], { stdio: 'pipe' });
          } finally {
            try { fs.unlinkSync(fixedSrc); } catch {}
          }
        } else {
          throw e;
        }
      }
    } else if (ext === 'psd' || ext === 'psb') {
      // PSD: sharp его не читает — берём композитный кадр через ImageMagick ([0] = merged image)
      execFileSync('convert', [src + '[0]', '-quality', '90', tmpImg], { stdio: 'pipe' });
    } else if (['mov', 'mp4', 'mkv', 'webm', 'avi', '3gp', 'm4v', 'mts', 'm2ts'].includes(ext)) {
      // Видео: sharp видео не читает — кадр через ffmpeg (первый кадр)
      execFileSync('ffmpeg', ['-y', '-v', 'error', '-i', src, '-frames:v', '1', '-q:v', '2', tmpImg], { stdio: 'pipe' });
    } else {
      // прочее: пробуем напрямую sharp
      fs.copyFileSync(src, tmpImg + '.src');
      try {
        await sharp(tmpImg + '.src').jpeg({ quality: 90 }).toFile(tmpImg);
      } finally {
        fs.unlinkSync(tmpImg + '.src');
      }
    }
    // Маленькие картинки (иконки 16x16) дают JPEG < 1000 байт — это нормально.
    if (!fs.existsSync(tmpImg) || fs.statSync(tmpImg).size < 100) throw new Error('decode produced nothing');

    const previewBuf = await sharp(tmpImg)
      .resize(1440, 1440, { fit: 'inside', withoutEnlargement: true })
      .jpeg({ quality: 80, progressive: false })
      .toBuffer();

    const thumbBuf = await sharp(tmpImg)
      .resize(250, 250, { fit: 'inside', withoutEnlargement: true })
      .webp({ quality: 80 })
      .toBuffer();

    const { data, info } = await sharp(tmpImg)
      .resize(100, 100, { fit: 'inside', withoutEnlargement: true })
      .raw()
      .ensureAlpha()
      .toBuffer({ resolveWithObject: true });
    const { rgbaToThumbHash } = await import(pathToFileURL(immichRequire.resolve('thumbhash')));
    const thumbhash = Buffer.from(rgbaToThumbHash(info.width, info.height, data));

    const dir = path.join(THUMBS_ROOT, USER_ID, asset.id.slice(0, 2), asset.id.slice(2, 4));
    fs.mkdirSync(dir, { recursive: true });
    const previewPath = path.join(dir, `${asset.id}_preview.jpeg`);
    const thumbPath = path.join(dir, `${asset.id}_thumbnail.webp`);
    fs.writeFileSync(previewPath, previewBuf);
    fs.writeFileSync(thumbPath, thumbBuf);

    for (const [type, p] of [['preview', previewPath], ['thumbnail', thumbPath]]) {
      await client.query(
        `INSERT INTO asset_file (id, "assetId", type, path, "createdAt", "updatedAt", "isEdited", "isProgressive", "isTransparent")
         VALUES (gen_random_uuid(), $1, $2, $3, now(), now(), false, false, false)`,
        [asset.id, type, p]
      );
    }
    await client.query(`UPDATE asset SET thumbhash = $1, visibility = CASE WHEN visibility = 'hidden' THEN 'timeline' ELSE visibility END WHERE id = $2`, [thumbhash, asset.id]);

    fs.unlinkSync(tmpImg);
    return true;
  } catch (e) {
    try { fs.unlinkSync(tmpImg); } catch {}
    console.error(`FAIL ${asset.id} (${ext}): ${String(e.message).split('\n')[0]}`);
    return false;
  }
}

async function main() {
  const client = new Client({ connectionString: process.env.DB_URL });
  await client.connect();

  const { rows: assets } = await client.query(
    `SELECT id, "originalPath" FROM asset WHERE thumbhash IS NULL`
  );

  if (assets.length === 0) {
    console.log('nothing to fix');
    await client.end();
    return;
  }

  console.log(`assets to fix: ${assets.length}`);
  let ok = 0, fail = 0;
  for (const asset of assets) {
    if (await fixAsset(client, asset)) ok++; else fail++;
  }

  const { rows: left } = await client.query(`SELECT count(*)::int AS n FROM asset WHERE thumbhash IS NULL`);
  console.log(`done: ${ok} fixed, ${fail} failed, ${left[0].n} still without thumbhash`);
  await client.end();

  if (fail > 0 || left[0].n > 0) process.exit(1);
}

main().catch((e) => { console.error(e); process.exit(1); });
// Draws Ambar's own 32px tray mark and 20 progress frames without external packages.
// Run from the project root: node scripts/make-tray-icons.mjs
import { mkdir, writeFile } from 'node:fs/promises';
import { deflateSync } from 'node:zlib';

const size = 32;
const scale = 4;
const root = 'src-tauri/icons';
const themes = ['dark', 'light', 'mac'];
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let i = 0; i < 8; i++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(data) {
  let c = 0xffffffff;
  for (const byte of data) c = crcTable[(c ^ byte) & 255] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const name = Buffer.from(type);
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([name, data])));
  return Buffer.concat([length, name, data, crc]);
}

function png(pixels) {
  const raw = Buffer.alloc(size * (1 + size * 4));
  for (let y = 0; y < size; y++) {
    const start = y * (1 + size * 4);
    raw[start] = 0;
    pixels.copy(raw, start + 1, y * size * 4, (y + 1) * size * 4);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw, { level: 9 })), chunk('IEND', Buffer.alloc(0))]);
}

function polygon(x, y, points) {
  let inside = false;
  for (let i = 0, j = points.length - 1; i < points.length; j = i++) {
    const [xi, yi] = points[i];
    const [xj, yj] = points[j];
    if ((yi > y) !== (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

function sample(x, y, frame, theme) {
  const mac = theme === 'mac';
  let colour = null;
  const square = Math.max(Math.abs(x - 16), Math.abs(y - 16));
  if (!mac && square < 15 && !(square > 12.5 && Math.abs(x - 16) + Math.abs(y - 16) > 27)) colour = [16, 21, 30, 255];
  const body = polygon(x, y, [[8, 14], [16, 8], [24, 14], [24, 24], [8, 24]]);
  if (body) colour = mac ? [0, 0, 0, 255] : [231, 241, 240, 255];
  const roof = polygon(x, y, [[5.5, 14.5], [16, 6], [26.5, 14.5], [24.7, 16.6], [16, 9.5], [7.3, 16.6]]);
  if (roof) colour = mac ? [0, 0, 0, 255] : [113, 212, 202, 255];
  if (x >= 13 && x < 19 && y >= 17 && y < 24) colour = mac ? null : [16, 21, 30, 255];
  // The downward arrow remains legible at the taskbar's 16px display size.
  if (!mac && y >= 16.4 && y < 22.1 && Math.abs(x - 16) < .75) colour = [113, 212, 202, 255];
  if (!mac && polygon(x, y, [[13.5, 20], [16, 22.5], [18.5, 20], [17.5, 19.5], [16, 21], [14.5, 19.5]])) colour = [113, 212, 202, 255];
  if (frame < 20) {
    const dx = x - 16, dy = y - 16;
    const radius = Math.hypot(dx, dy);
    if (radius >= 13.5 && radius <= 15.4) {
      let angle = Math.atan2(dy, dx) + Math.PI / 2;
      if (angle < 0) angle += Math.PI * 2;
      const fraction = frame / 19;
      colour = angle <= fraction * Math.PI * 2
        ? (mac ? [0, 0, 0, 255] : [113, 212, 202, 255])
        : (mac ? [0, 0, 0, 70] : [174, 196, 198, 90]);
    }
  }
  return colour;
}

function draw(frame, theme) {
  const out = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    let r = 0, g = 0, b = 0, a = 0;
    for (let sy = 0; sy < scale; sy++) for (let sx = 0; sx < scale; sx++) {
      const pixel = sample(x + (sx + .5) / scale, y + (sy + .5) / scale, frame, theme);
      if (pixel) { r += pixel[0] * pixel[3]; g += pixel[1] * pixel[3]; b += pixel[2] * pixel[3]; a += pixel[3]; }
    }
    const n = scale * scale;
    const at = (y * size + x) * 4;
    out[at] = a ? Math.round(r / a) : 0;
    out[at + 1] = a ? Math.round(g / a) : 0;
    out[at + 2] = a ? Math.round(b / a) : 0;
    out[at + 3] = Math.round(a / n);
  }
  return png(out);
}

for (const theme of themes) {
  const folder = theme === 'mac' ? `${root}/tray` : `${root}/tray-win/${theme}`;
  await mkdir(folder, { recursive: true });
  for (let frame = 0; frame <= 20; frame++) {
    await writeFile(`${folder}/${String(frame).padStart(2, '0')}.png`, draw(frame, theme));
  }
}

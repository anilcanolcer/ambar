const number1 = new Intl.NumberFormat("tr-TR", { maximumFractionDigits: 1, minimumFractionDigits: 0 });
const number0 = new Intl.NumberFormat("tr-TR", { maximumFractionDigits: 0 });

export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit++;
  }
  const fmt = unit >= 2 ? number1 : number0;
  return `${fmt.format(value)} ${units[unit]}`;
}

export function formatSpeed(bytesPerSecond: number | null | undefined): string {
  const text = formatBytes(bytesPerSecond);
  return text ? `${text}/sn` : "";
}

export function formatDuration(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return "";
  const s = Math.round(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(sec)}` : `${m}:${pad(sec)}`;
}

export function formatEta(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return "";
  if (seconds < 60) return `${Math.max(1, Math.round(seconds))} sn kaldı`;
  const m = Math.floor(seconds / 60);
  if (m < 60) return `${m} dk ${Math.round(seconds % 60)} sn kaldı`;
  return `${Math.floor(m / 60)} sa ${m % 60} dk kaldı`;
}

export function formatPercent(fraction: number): string {
  return `%${number0.format(Math.floor(Math.min(1, Math.max(0, fraction)) * 100))}`;
}

export function resolutionLabel(res: number): string {
  if (res >= 4320) return "8K";
  if (res >= 2160) return "4K";
  if (res >= 1440) return "2K (1440p)";
  if (res >= 1080) return "1080p Full HD";
  if (res >= 720) return "720p HD";
  return `${res}p`;
}

/**
 * "C:\…", "C:/…" or "\\nas\…". Only these are split at backslashes too: on a
 * Mac a backslash is an ordinary character in a file name ("AC\DC.mp3").
 */
export function isWindowsPath(path: string): boolean {
  return /^(?:[a-z]:[\\/]|\\\\)/i.test(path);
}

/** Where the last folder separator is, -1 if there is none. */
function lastSeparator(path: string): number {
  const slash = path.lastIndexOf("/");
  return isWindowsPath(path) ? Math.max(slash, path.lastIndexOf("\\")) : slash;
}

export function fileName(path: string | null | undefined): string {
  if (!path) return "";
  return path.slice(lastSeparator(path) + 1);
}

/** "/Users/ali/Downloads/Ambar/a.mp3" -> "/Users/ali/Downloads/Ambar", "C:\a.mp3" -> "C:\" */
export function dirName(path: string | null | undefined): string {
  if (!path) return "";
  const i = lastSeparator(path);
  if (i <= 0) return "";
  const dir = path.slice(0, i);
  // A drive's top folder is "C:\"; plain "C:" means the current folder there.
  return /^[a-z]:$/i.test(dir) ? `${dir}\\` : dir;
}

/**
 * The first web address in some text. Links copied without "https://"
 * (e.g. "youtube.com/watch?v=…") get it added.
 */
export function extractUrl(text: string): string | null {
  const t = text.trim();
  const found = t.match(/https?:\/\/[^\s<>"]+/i)?.[0];
  if (found) {
    // "[text](https://…)" Markdown, then sentence punctuation or a closing
    // quote stuck to the end; ")" only when it closes nothing.
    let url = (found.split("](")[0] ?? found).replace(/[.,;:!?'"]+$/, "");
    while (url.endsWith(")") && (url.match(/\(/g)?.length ?? 0) < (url.match(/\)/g)?.length ?? 0)) url = url.slice(0, -1);
    return url;
  }
  if (/^(?:www\.|m\.|music\.)?(?:youtube\.com|youtu\.be|vimeo\.com|soundcloud\.com)\/\S+$/i.test(t)) return `https://${t}`;
  return null;
}

export function looksLikeUrl(text: string): boolean {
  const t = text.trim();
  return /^https?:\/\/\S+$/i.test(t) && t.length < 4096;
}

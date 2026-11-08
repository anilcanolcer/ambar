import "./styles.css";
import { createApi, inTauri, type Api } from "./api";
import { icon, type IconName } from "./icons";
import {
  dirName,
  extractUrl,
  formatBytes,
  formatDuration,
  formatEta,
  formatPercent,
  formatSpeed,
  isWindowsPath,
  looksLikeUrl,
  resolutionLabel,
} from "./format";
import { isAudioKind } from "./types";
import type {
  AppUpdate,
  AppUpdateProgress,
  DownloadRequest,
  EngineId,
  EngineProgress,
  EnginesState,
  JobEvent,
  JobView,
  Kind,
  MediaInfo,
  Platform,
  PlaylistInfo,
  Settings,
  VideoInfo,
} from "./types";

// ------------------------------------------------------------------ helpers

type Child = Node | string | number | null | undefined | false;

interface Props {
  class?: string;
  title?: string;
  attrs?: Record<string, string>;
  on?: Partial<{ [K in keyof HTMLElementEventMap]: (ev: HTMLElementEventMap[K]) => void }>;
}

function h<K extends keyof HTMLElementTagNameMap>(tag: K, props: Props = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  if (props.class) el.className = props.class;
  if (props.title) el.title = props.title;
  for (const [k, v] of Object.entries(props.attrs ?? {})) el.setAttribute(k, v);
  for (const [k, fn] of Object.entries(props.on ?? {})) el.addEventListener(k, fn as EventListener);
  append(el, children);
  return el;
}

function append(el: Element, children: Child[]) {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    el.append(c instanceof Node ? c : String(c));
  }
}

function button(label: Child, onClick: () => void, opts: { class?: string; icon?: IconName; title?: string; disabled?: boolean } = {}) {
  const b = h("button", { class: `btn ${opts.class ?? ""}`.trim(), title: opts.title, attrs: { type: "button" }, on: { click: onClick } });
  if (opts.icon) b.append(icon(opts.icon, 16));
  if (label !== null && label !== "") append(b, [h("span", {}, label)]);
  b.disabled = !!opts.disabled;
  return b;
}

function iconButton(name: IconName, title: string, onClick: () => void, extra = "") {
  const b = h("button", { class: `icon-btn ${extra}`.trim(), title, attrs: { type: "button", "aria-label": title }, on: { click: onClick } });
  b.append(icon(name, 18));
  return b;
}

function spinner() {
  return h("span", { class: "spinner", attrs: { "aria-hidden": "true" } });
}

function img(src: string | null, cls: string, alt = "") {
  if (!src) return h("div", { class: `${cls} img-fallback` }, icon("play", 22));
  const el = h("img", { class: cls, attrs: { src, alt, loading: "lazy", referrerpolicy: "no-referrer", draggable: "false" } });
  el.addEventListener("error", () => el.replaceWith(h("div", { class: `${cls} img-fallback` }, icon("play", 22))));
  return el;
}

/** Plays the entrance animation once; re-inserted elements stay still. */
function enter<T extends HTMLElement>(el: T): T {
  el.classList.add("enter");
  el.addEventListener("animationend", () => el.classList.remove("enter"), { once: true });
  return el;
}

/** Home folders under the names Finder shows in Turkish. */
const HOME_FOLDERS: Record<string, string> = {
  Downloads: "İndirilenler",
  Desktop: "Masaüstü",
  Documents: "Belgeler",
  Movies: "Filmler",
  Music: "Müzik",
  Pictures: "Resimler",
};

/**
 * Home folders under the names Explorer shows on a Turkish Windows; on disk
 * they keep their English names. Keys are lower case (paths ignore case).
 */
const WINDOWS_HOME_FOLDERS: Record<string, string> = {
  downloads: "İndirilenler",
  desktop: "Masaüstü",
  documents: "Belgeler",
  videos: "Videolar",
  music: "Müzik",
  pictures: "Resimler",
};

/** "/Users/ali/Downloads/Ambar" -> "İndirilenler › Ambar"; long paths keep their last three folders. */
function friendlyPath(path: string): string {
  if (isWindowsPath(path)) return friendlyWindowsPath(path);
  const home = state.homeDir.replace(/\/+$/, "");
  const icloud = `${home}/Library/Mobile Documents/com~apple~CloudDocs`;
  const under = (base: string) => path === base || path.startsWith(`${base}/`);
  let parts: string[];
  if (home && under(icloud)) {
    parts = ["iCloud Drive", ...path.slice(icloud.length).split("/").filter(Boolean)];
  } else if (home && under(home)) {
    parts = path.slice(home.length).split("/").filter(Boolean);
    const first = parts[0];
    if (first === undefined) return "Ana klasör";
    parts[0] = HOME_FOLDERS[first] ?? first;
  } else if (under("/Users/Shared")) {
    parts = ["Paylaşılan", ...path.slice("/Users/Shared".length).split("/").filter(Boolean)];
  } else {
    parts = path.split("/").filter(Boolean);
    // "/Volumes/USB/Videos" -> "USB › Videos"
    if (parts[0] === "Volumes" && parts.length > 1) parts.shift();
    if (parts.length === 0) return path;
  }
  return (parts.length > 3 ? ["…", ...parts.slice(-3)] : parts).join(" › ");
}

/**
 * "C:\Users\ali\Downloads\Ambar" -> "İndirilenler › Ambar",
 * "D:\Videolar" -> "D: › Videolar", "\\nas\paylaşım\x" -> "nas › paylaşım › x".
 */
function friendlyWindowsPath(path: string): string {
  // "\\?\C:\…" and "\\?\UNC\nas\…" are long forms of the same paths.
  const plain = path.replace(/^\\\\\?\\UNC\\/i, "\\\\").replace(/^\\\\\?\\/, "");
  let parts = plain.split(/[\\/]+/).filter(Boolean);
  const home = state.homeDir.split(/[\\/]+/).filter(Boolean);
  const underHome = home.length > 0 && home.length <= parts.length && home.every((p, i) => p.toLowerCase() === parts[i]!.toLowerCase());
  if (underHome) {
    parts = parts.slice(home.length);
    if (parts.length === 0) return "Ana klasör";
    // OneDrive can hold the known folders too ("OneDrive\Documents");
    // "OneDrive - Şirket" is a work account and keeps its name.
    const at = /^OneDrive( - .+)?$/i.test(parts[0]!) ? 1 : 0;
    const folder = parts[at];
    if (folder !== undefined) parts[at] = WINDOWS_HOME_FOLDERS[folder.toLowerCase()] ?? folder;
  } else if (/^[a-z]:$/i.test(parts[0] ?? "")) {
    parts[0] = parts[0]!.toUpperCase();
  }
  if (parts.length === 0) return path;
  return (parts.length > 3 ? ["…", ...parts.slice(-3)] : parts).join(" › ");
}

const SITE_NAMES: Record<string, string> = {
  "youtube.com": "YouTube",
  "youtu.be": "YouTube",
  "music.youtube.com": "YouTube Music",
  Youtube: "YouTube",
  "vimeo.com": "Vimeo",
  "soundcloud.com": "SoundCloud",
  "dailymotion.com": "Dailymotion",
  "twitch.tv": "Twitch",
  "x.com": "X",
  "twitter.com": "X",
  "instagram.com": "Instagram",
  "tiktok.com": "TikTok",
  "bandcamp.com": "Bandcamp",
  "archive.org": "Internet Archive",
};

/** "artist.bandcamp.com" -> "Bandcamp"; exact names win over suffixes. */
function siteLabel(site: string): string {
  const parts = site.split(".");
  for (let i = 0; i < parts.length - 1; i++) {
    const hit = SITE_NAMES[parts.slice(i).join(".")];
    if (hit) return hit;
  }
  return SITE_NAMES[site] ?? site;
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

// -------------------------------------------------------------------- state

const NOTICE_POINTS = [
  "Ambar; kendi videolarını, izin aldığın içerikleri ve Creative Commons gibi serbest lisanslı içerikleri kaydetmen için yapılmış ücretsiz bir araçtır.",
  "İndirdiğin içerikleri paylaşma, yeniden yükleme ya da ticari amaçla kullanma. Türkiye'de kişisel kopya yalnızca kazanç amacı olmadan kendi kullanımın içindir.",
  "Bazı sitelerin kullanım şartları indirmeyi kısıtlar. Bu şartlara ve yasalara uymak senin sorumluluğundadır.",
  "Ambar DRM korumalı, ücretli ya da üyelere özel içerikleri indirmez.",
];

const ENGINE_INFO: Record<EngineId, { name: string; role: string; source: string; windowsSource?: string }> = {
  "yt-dlp": { name: "yt-dlp", role: "Medyayı bulan ve indiren motor", source: "github.com/yt-dlp" },
  ffmpeg: { name: "FFmpeg", role: "Ses ve görüntüyü dönüştürür, birleştirir", source: "ffmpeg.martin-riedl.de", windowsSource: "gyan.dev" },
  deno: { name: "Deno", role: "yt-dlp'nin ihtiyaç duyduğu JavaScript çalıştırıcı", source: "deno.com" },
};
const ENGINE_ORDER: EngineId[] = ["yt-dlp", "ffmpeg", "deno"];

type Group = "audio" | "video";

/** `windowsDesc` replaces a description that names Mac-only players. */
const FORMATS: { kind: Kind; group: Group; label: string; desc: string; windowsDesc?: string }[] = [
  { kind: "mp3", group: "audio", label: "MP3", desc: "Her cihazda ve arabada çalar. Müzik için en uyumlu seçenek." },
  { kind: "m4a", group: "audio", label: "M4A", desc: "Apple cihazlarda rahat çalar. Kaynak AAC ise hiç dönüştürülmez." },
  { kind: "opus", group: "audio", label: "OPUS", desc: "Kaynağın kendi sesi, hiç dönüştürülmeden. En küçük dosya, en iyi kalite. Bazı eski cihazlar açmayabilir." },
  { kind: "flac", group: "audio", label: "FLAC", desc: "Kayıpsız arşiv biçimi. Kaynak zaten sıkıştırılmış olduğu için ses kalitesi artmaz, dosya büyür." },
  { kind: "wav", group: "audio", label: "WAV", desc: "Kurgu ve DJ programları için sıkıştırmasız ses. Kalite artmaz, dosya çok büyür, kapak resmi eklenmez." },
  {
    kind: "mp4",
    group: "video",
    label: "MP4",
    desc: "Her yerde açılır: telefon, QuickTime, kurgu programları.",
    windowsDesc: "Her yerde açılır: telefon, Windows'un kendi oynatıcısı, kurgu programları.",
  },
  {
    kind: "mkv",
    group: "video",
    label: "MKV",
    desc: "En yüksek kalite, hiç dönüştürülmeden (AV1 ya da VP9). IINA veya VLC ile açılır.",
    windowsDesc: "En yüksek kalite, hiç dönüştürülmeden (AV1 ya da VP9). VLC ile sorunsuz açılır.",
  },
  { kind: "webm", group: "video", label: "WEBM", desc: "Web için açık biçim (VP9). Tarayıcılarda doğrudan oynar, kapak resmi eklenmez." },
];

function formatDesc(f: (typeof FORMATS)[number]): string {
  return (onWindows() && f.windowsDesc) || f.desc;
}

const MP3_QUALITIES: { value: number | null; label: string }[] = [
  { value: null, label: "En iyi (önerilen)" },
  { value: 320, label: "320 kbps" },
  { value: 256, label: "256 kbps" },
  { value: 192, label: "192 kbps" },
  { value: 128, label: "128 kbps" },
];

const PLAYLIST_RESOLUTIONS = [2160, 1440, 1080, 720, 480, 360];

/** Ambar's own update; see "app update" below. */
interface AppUpdateState {
  /** The newer Ambar found by the last check. */
  found: AppUpdate | null;
  checking: boolean;
  /** A check started in Settings is running; only such checks report back. */
  manual: boolean;
  /** The last check started in Settings found nothing newer. */
  upToDate: boolean;
  checkError: string | null;
  /** "Sonra" was pressed for this version; the bar stays away for this session. */
  dismissed: string | null;
  notesOpen: boolean;
  /** Waiting for a yes: the restart would cut running downloads short. */
  confirming: boolean;
  installing: boolean;
  /** The install returned; Ambar is about to restart. */
  installed: boolean;
  progress: AppUpdateProgress | null;
  installError: string | null;
}

interface State {
  ready: boolean;
  bootError: string | null;
  appVersion: string;
  noticeVersion: number;
  settings: Settings;
  engines: EnginesState;
  install: { running: boolean; error: string | null; progress: Partial<Record<EngineId, EngineProgress>> };
  noticeChecked: boolean;
  url: string;
  fetching: boolean;
  fetchToken: number;
  fetchError: string | null;
  /** The error came from the site, so trying again can help. */
  fetchErrorRetryable: boolean;
  /** The running (or failed) lookup was for a whole playlist. */
  fetchPlaylist: boolean;
  /** The lookup has been running for a while. */
  fetchSlow: boolean;
  homeDir: string;
  info: MediaInfo | null;
  kind: Kind;
  res: number;
  splitArtist: boolean;
  selected: Set<string>;
  jobs: Map<number, JobView>;
  modal: "settings" | "about" | null;
  updatingEngine: EngineId | null;
  checkingUpdates: boolean;
  av1Hardware: boolean;
  audioQuality: number | null;
  subtitles: boolean;
  /** Last format chosen in each group, so switching Ses/Video keeps it. */
  lastAudio: Kind;
  lastVideo: Kind;
  /** The user picked the format / quality themselves (only those are remembered). */
  kindTouched: boolean;
  resTouched: boolean;
  /** Set when a modal opens so it animates in once. */
  modalFresh: boolean;
  /** Set when new media info arrives so its card animates in once. */
  freshCard: boolean;
  /** Which system Ambar runs on; labels, shortcuts and the title bar follow it. */
  platform: Platform;
  appUpdate: AppUpdateState;
}

const state: State = {
  ready: false,
  bootError: null,
  appVersion: "",
  noticeVersion: 1,
  settings: {
    downloadDir: "",
    defaultKind: "mp3",
    defaultRes: 1080,
    maxConcurrent: 2,
    ytdlpChannel: "nightly",
    acceptedNotice: 0,
    squareCover: true,
    askFolder: false,
    hideDownloads: false,
  },
  engines: { allInstalled: false, engines: [] },
  install: { running: false, error: null, progress: {} },
  noticeChecked: false,
  url: "",
  fetching: false,
  fetchToken: 0,
  fetchError: null,
  fetchErrorRetryable: false,
  fetchPlaylist: false,
  fetchSlow: false,
  info: null,
  kind: "mp3",
  res: 1080,
  splitArtist: false,
  selected: new Set(),
  jobs: new Map(),
  modal: null,
  updatingEngine: null,
  checkingUpdates: false,
  av1Hardware: false,
  homeDir: "",
  audioQuality: null,
  subtitles: false,
  lastAudio: "mp3",
  lastVideo: "mp4",
  kindTouched: false,
  resTouched: false,
  modalFresh: false,
  freshCard: false,
  // Only bootstrap says for sure. The webview's name is right in practice
  // (WebView2 reports Windows), so the first frame already fits.
  platform: navigator.userAgent.includes("Windows") ? "windows" : "macos",
  appUpdate: {
    found: null,
    checking: false,
    manual: false,
    upToDate: false,
    checkError: null,
    dismissed: null,
    notesOpen: false,
    confirming: false,
    installing: false,
    installed: false,
    progress: null,
    installError: null,
  },
};

let api: Api;
const root = document.getElementById("app")!;
const toastHost = h("div", { class: "toasts", attrs: { role: "status", "aria-live": "polite" } });
document.body.append(toastHost);
if (inTauri) document.documentElement.classList.add("is-tauri");
setPlatform(state.platform);

/** Puts "is-macos" / "is-windows" on <html>; the title bar, fonts and scrollbars follow it. */
function setPlatform(platform: Platform) {
  state.platform = platform;
  const classes = document.documentElement.classList;
  classes.remove("is-macos", "is-windows", "is-linux");
  classes.add(`is-${platform}`);
}

/** Windows has its own names for the Mac's Finder, ⌘ and players. */
function onWindows() {
  return state.platform === "windows";
}

function needsOnboarding() {
  return state.settings.acceptedNotice < state.noticeVersion || !state.engines.allInstalled;
}

// ------------------------------------------------------------------- toasts

function toast(text: string, kind: "info" | "success" | "error" = "info", action?: { label: string; onClick: () => void }) {
  const dismiss = () => {
    el.classList.remove("show");
    setTimeout(() => el.remove(), 300);
  };
  const el = h(
    "div",
    { class: `toast toast-${kind}` },
    icon(kind === "error" ? "alert" : kind === "success" ? "check" : "info", 16),
    h("span", {}, text),
    action &&
      button(action.label, () => {
        action.onClick();
        dismiss();
      }, { class: "btn-ghost btn-sm toast-action" }),
  );
  toastHost.append(el);
  requestAnimationFrame(() => el.classList.add("show"));
  setTimeout(dismiss, kind === "error" || action ? 6000 : 3500);
}

// ------------------------------------------------------------------- render

let jobListEl: HTMLElement | null = null;
let jobsHeaderEl: HTMLElement | null = null;
let downloadsEl: HTMLElement | null = null;
/** Re-labels the playlist's download button; set while a playlist card is shown. */
let playlistSync: (() => void) | null = null;
let bannerEl: HTMLElement | null = null;
/** Ambar's own update: the bar on the main screen and the block in Settings, while shown. */
let appBarEl: HTMLElement | null = null;
let appBlockEl: HTMLElement | null = null;
const jobRows = new Map<number, { el: HTMLElement; status: string }>();
const engineRows = new Map<EngineId, HTMLElement>();

let renderedModal: State["modal"] = null;

function render() {
  // Read scroll positions before anything is rebuilt: reading them later
  // forces a layout while job rows are detached, which makes the page jump.
  const listScroll = root.querySelector(".pl-list")?.scrollTop ?? 0;
  const modalScroll = state.modal !== null && state.modal === renderedModal ? root.querySelector(".modal-body")?.scrollTop ?? 0 : 0;
  renderedModal = state.modal;
  engineRows.clear();
  jobListEl = null;
  jobsHeaderEl = null;
  downloadsEl = null;
  playlistSync = null;
  bannerEl = null;
  appBarEl = null;
  appBlockEl = null;
  let content: HTMLElement;
  if (!state.ready) content = renderLoading();
  else if (needsOnboarding()) content = renderOnboarding();
  else content = renderMain();
  root.replaceChildren(renderTitlebar(), content);
  const modal = renderModal();
  if (modal) root.append(modal);
  const list = root.querySelector(".pl-list");
  if (list) list.scrollTop = listScroll;
  const body = root.querySelector(".modal-body");
  if (body) body.scrollTop = modalScroll;
}

function renderTitlebar() {
  const showActions = state.ready && !needsOnboarding();
  // On a Mac the window is moved by this bar. Windows draws a real title bar
  // above the page, so here it is only a toolbar.
  const drag: Record<string, string> = onWindows() ? {} : { "data-tauri-drag-region": "" };
  return h(
    "header",
    { class: `titlebar${window.scrollY > 4 ? " scrolled" : ""}`, attrs: drag },
    h(
      "div",
      { class: "brand", attrs: drag },
      h("img", { class: "brand-logo", attrs: { src: "/logo.svg", alt: "", draggable: "false" } }),
      h("span", { class: "brand-name", attrs: drag }, "Ambar"),
    ),
    h("div", { class: "titlebar-spacer", attrs: drag }),
    showActions && iconButton("folder", "İndirme klasörünü aç", () => run(() => api.openDownloadDir())),
    showActions && iconButton("settings", "Ayarlar", () => openModal("settings")),
  );
}

function renderLoading() {
  return h(
    "main",
    { class: "center-screen" },
    state.bootError
      ? h("div", { class: "card error-card" }, icon("alert", 20), h("p", {}, state.bootError))
      : h("div", { class: "loading" }, spinner(), h("span", {}, "Yükleniyor")),
  );
}

// ------------------------------------------------------------- onboarding

function renderOnboarding() {
  const accepted = state.settings.acceptedNotice >= state.noticeVersion;
  const canInstall = (accepted || state.noticeChecked) && !state.install.running;
  const checkbox = h("input", {
    attrs: { type: "checkbox", id: "notice-check" },
    on: {
      change: (e) => {
        state.noticeChecked = (e.target as HTMLInputElement).checked;
        render();
      },
    },
  });
  checkbox.checked = accepted || state.noticeChecked;
  checkbox.disabled = accepted || state.install.running;

  const engineList = h("div", { class: "engine-list" });
  for (const id of ENGINE_ORDER) {
    const row = renderEngineRow(id);
    engineRows.set(id, row);
    engineList.append(row);
  }

  return h(
    "main",
    { class: "onboarding" },
    h(
      "div",
      { class: "welcome" },
      h("img", { class: "welcome-logo", attrs: { src: "/logo.svg", alt: "" } }),
      h("h1", {}, "Ambar'ya hoş geldin"),
      h("p", { class: "lead" }, "Reklamsız, ücretsiz ve tamamen bilgisayarında çalışan bir medya indirici."),
    ),
    h(
      "section",
      { class: "card notice" },
      h("div", { class: "card-head" }, icon("shield", 20), h("h2", {}, "Başlamadan önce")),
      h("ul", { class: "notice-list" }, ...NOTICE_POINTS.map((p) => h("li", {}, p))),
      h("label", { class: "check-row", attrs: { for: "notice-check" } }, checkbox, h("span", {}, "Okudum, Ambar'yı bu şartlarla kullanacağım.")),
    ),
    h(
      "section",
      { class: "card" },
      h("div", { class: "card-head" }, icon("download", 20), h("h2", {}, "Gerekli bileşenler")),
      h(
        "p",
        { class: "muted" },
        // Windows' FFmpeg comes as one bigger archive (docs, ffplay inside).
        `Ambar üç açık kaynak programla çalışır. Bunları içinde taşımaz, resmi kaynaklarından indirip doğrular. Toplam indirme yaklaşık ${onWindows() ? "180" : "150"} MB.`,
      ),
      engineList,
      state.install.error && h("div", { class: "inline-error" }, icon("alert", 16), h("span", {}, state.install.error)),
    ),
    h(
      "div",
      { class: "onboarding-actions" },
      button(state.install.running ? "Kuruluyor" : state.install.error ? "Tekrar dene" : accepted ? "Bileşenleri kur" : "Kabul et ve kur", startInstall, {
        class: "btn-primary btn-lg",
        icon: state.install.running ? undefined : "download",
        disabled: !canInstall,
      }),
    ),
  );
}

function renderEngineRow(id: EngineId) {
  const info = ENGINE_INFO[id];
  const status = state.engines.engines.find((e) => e.id === id);
  const p = state.install.progress[id];
  let label = "Bekliyor";
  let cls = "waiting";
  let fraction: number | null = null;
  if (status?.installed && !p) {
    label = status.version ? `Kurulu · ${status.version}` : "Kurulu";
    cls = "done";
  }
  if (p) {
    switch (p.phase) {
      case "checking":
        label = "Sürüm kontrol ediliyor";
        cls = "busy";
        break;
      case "downloading":
        fraction = p.fraction;
        label = `İndiriliyor · ${formatBytes(p.downloaded)}`;
        cls = "busy";
        break;
      case "installing":
        label = "Doğrulanıyor";
        cls = "busy";
        break;
      case "done":
      case "up-to-date":
        label = p.version ? `Kuruldu · ${p.version}` : "Kuruldu";
        cls = "done";
        break;
      case "error":
        label = "Hata";
        cls = "failed";
        break;
    }
  }
  const bar = h("div", { class: `progress ${cls === "busy" && fraction == null ? "indeterminate" : ""}` }, h("div", { class: "bar" }));
  (bar.firstElementChild as HTMLElement).style.width = `${Math.round((cls === "done" ? 1 : fraction ?? 0) * 100)}%`;
  return h(
    "div",
    { class: `engine-row ${cls}` },
    h(
      "div",
      { class: "engine-icon" },
      cls === "done" ? icon("check", 16) : cls === "failed" ? icon("alert", 16) : cls === "busy" ? spinner() : h("span", { class: "dot" }),
    ),
    h(
      "div",
      { class: "engine-text" },
      h("div", { class: "engine-name" }, info.name, h("span", { class: "engine-source" }, (onWindows() && info.windowsSource) || info.source)),
      h("div", { class: "engine-role" }, info.role),
    ),
    h("div", { class: "engine-state" }, h("span", {}, label), (cls === "busy" || cls === "done") && bar),
  );
}

function patchEngineRow(id: EngineId) {
  const old = engineRows.get(id);
  if (!old) return;
  const fresh = renderEngineRow(id);
  old.replaceWith(fresh);
  engineRows.set(id, fresh);
}

async function startInstall() {
  state.install = { running: true, error: null, progress: {} };
  render();
  revealInOnboarding(".engine-list");
  try {
    if (state.settings.acceptedNotice < state.noticeVersion) {
      state.settings = await api.acceptNotice();
    }
    state.engines = await api.installEngines();
    state.install.running = false;
    toast("Ambar kullanıma hazır", "success");
    render();
    focusUrl();
    startUpdateTimer();
    startAppUpdateChecks();
  } catch (e) {
    state.install.running = false;
    state.install.error = errorText(e);
    state.engines = await api.enginesState().catch(() => state.engines);
    render();
    revealInOnboarding(".inline-error");
  }
}

/** The install button stays at the bottom of the window; scroll what it starts into view. */
function revealInOnboarding(selector: string) {
  requestAnimationFrame(() => document.querySelector(selector)?.scrollIntoView({ block: "nearest", behavior: reducedMotion() ? "auto" : "smooth" }));
}

// --------------------------------------------------------------- main view

function renderMain() {
  const input = h("input", {
    class: "url-input",
    attrs: {
      type: "url",
      placeholder: "Bir video ya da oynatma listesi bağlantısı yapıştır",
      spellcheck: "false",
      autocomplete: "off",
      autocapitalize: "off",
      id: "url-input",
    },
    on: {
      input: (e) => {
        state.url = (e.target as HTMLInputElement).value;
        // Typing fixes a "that's not a link" note.
        if (state.fetchError && !state.fetchErrorRetryable) {
          state.fetchError = null;
          document.querySelector(".url-note")?.remove();
        }
        // A new link is being typed: "Getir" is the next step again.
        document.getElementById("fetch-btn")?.classList.replace("btn-soft", "btn-primary");
        syncUrlBar();
      },
      paste: (e) => {
        // A pasted link replaces whatever is in the field instead of being
        // inserted at the caret next to an old link.
        const url = extractUrl(e.clipboardData?.getData("text/plain") ?? "");
        if (!url) return;
        e.preventDefault();
        input.value = state.url = url;
        syncUrlBar();
        fetchInfo(false);
      },
    },
  });
  input.value = state.url;

  const form = h(
    "form",
    {
      class: `url-bar${state.url ? "" : " is-empty"}`,
      // Our own check gives a clearer message than the browser's bubble.
      attrs: { novalidate: "" },
      on: {
        submit: (e) => {
          e.preventDefault();
          fetchInfo(false);
        },
      },
    },
    h("span", { class: "url-icon" }, icon("link", 18)),
    input,
    state.url
      ? iconButton("close", state.fetching ? "Vazgeç" : "Temizle", () => {
          state.url = "";
          state.info = null;
          state.fetchError = null;
          state.fetching = false;
          // Results of a lookup still running are ignored from now on.
          state.fetchToken++;
          render();
          focusUrl();
        }, "url-clear")
      : null,
    h("kbd", { class: "kbd-hint", title: "Kopyaladığın bağlantıyı yapıştır" }, onWindows() ? "Ctrl+V" : "⌘V"),
    h(
      "button",
      // Once a card is shown, "İndir" is the one button that should stand out.
      { class: state.info && !state.fetching ? "btn btn-soft" : "btn btn-primary", attrs: { type: "submit", id: "fetch-btn" } },
      state.fetching ? spinner() : icon("arrowRight", 16),
      h("span", {}, state.fetching ? "Getiriliyor" : "Getir"),
    ),
  );

  bannerEl = h("div", { class: "update-banner" });
  paintBanner();

  appBarEl = h("div", { class: "app-update" });
  paintAppUpdateIn(appBarEl, "bar");

  jobsHeaderEl = h("div", { class: "section-head" });
  jobListEl = h("div", { class: "job-list" });
  downloadsEl = h("section", { class: "downloads" }, jobsHeaderEl, jobListEl);
  paintJobs();

  // With a card on screen the big headline makes room for it.
  const hasCard = !!(state.info || state.fetching || (state.fetchError && state.fetchErrorRetryable));
  const urlNote =
    state.fetchError && !state.fetchErrorRetryable ? h("p", { class: "url-note", attrs: { role: "alert" } }, icon("alert", 14), h("span", {}, state.fetchError)) : null;
  return h(
    "main",
    { class: hasCard ? "main has-card" : "main" },
    appBarEl,
    bannerEl,
    h(
      "section",
      { class: "hero" },
      h("p", { class: "hero-kicker" }, "📦  MEDYA ARŞİVİN · AMBAR"),
      h("h1", {}, "Bağlantıyı yapıştır, gerisini Ambar halletsin."),
      h("p", { class: "lead" }, "Reklam yok, üyelik yok, kalite sınırı yok. Her şey senin bilgisayarında olur."),
      form,
      urlNote,
      h("p", { class: "hint" }, "YouTube, Vimeo, SoundCloud ve yüzlerce site desteklenir. Bağlantıyı pencereye sürükleyip bırakabilirsin."),
    ),
    renderPreview(),
    downloadsEl,
    h(
      "footer",
      { class: "footer" },
      h("span", {}, "Ücretsiz · Reklamsız · Açık kaynak"),
      h("button", { class: "link-btn", attrs: { type: "button" }, on: { click: () => openModal("about") } }, "Hakkında ve lisanslar"),
    ),
  );
}

/** Keeps the link field's hint and buttons in step with what is typed, without a full render. */
function syncUrlBar() {
  document.querySelector(".url-bar")?.classList.toggle("is-empty", !state.url);
}

function paintBanner() {
  if (!bannerEl) return;
  const id = state.updatingEngine;
  bannerEl.classList.toggle("show", !!id);
  bannerEl.replaceChildren(...(id ? [spinner(), h("span", {}, `${ENGINE_INFO[id].name} güncelleniyor. Bu sırada indirmeye devam edebilirsin.`)] : []));
}

function renderPreview(): HTMLElement {
  if (state.fetching) {
    return h(
      "section",
      { class: "card media skeleton" },
      h("div", { class: "thumb shimmer" }),
      h(
        "div",
        { class: "media-body" },
        h("div", { class: "line shimmer w60" }),
        h("div", { class: "line shimmer w90" }),
        h("div", { class: "line shimmer w40" }),
        h("p", { class: "skeleton-text" }, state.fetchSlow ? SLOW_TEXT : state.fetchPlaylist ? "Oynatma listesi alınıyor…" : "Video bilgileri alınıyor…"),
      ),
    );
  }
  // A mistyped link is pointed out under the field (see renderMain).
  if (state.fetchError && state.fetchErrorRetryable) {
    return h(
      "section",
      { class: "card error-card" },
      icon("alert", 20),
      h("div", {}, h("strong", {}, "Bu bağlantı açılamadı"), h("p", { class: "selectable" }, state.fetchError)),
      button("Tekrar dene", () => fetchInfo(state.fetchPlaylist), { class: "btn-ghost btn-sm", icon: "retry" }),
    );
  }
  if (!state.info) return h("div", { class: "preview-empty" });
  const card = state.info.type === "video" ? renderVideo(state.info) : renderPlaylist(state.info);
  if (state.freshCard) {
    state.freshCard = false;
    enter(card);
  }
  return card;
}

function pickKind(kind: Kind) {
  state.kind = kind;
  state.kindTouched = true;
  if (isAudioKind(kind)) state.lastAudio = kind;
  else state.lastVideo = kind;
  render();
}

/** "Ses / Video" on top, the formats of the chosen group below. */
function formatPicker(info: { hasVideo: boolean; hasAudio: boolean; webm?: boolean }) {
  const current: Group = isAudioKind(state.kind) ? "audio" : "video";
  const groups: { group: Group; label: string; hint: string; icon: IconName; enabled: boolean }[] = [
    { group: "audio", label: "Ses", hint: "Müzik, podcast, ses kaydı", icon: "music", enabled: info.hasAudio },
    { group: "video", label: "Video", hint: "Görüntü ve ses birlikte", icon: "film", enabled: info.hasVideo },
  ];
  const top = h("div", { class: "segmented kinds", attrs: { role: "radiogroup", "aria-label": "Ses ya da video" } });
  for (const g of groups) {
    const b = h(
      "button",
      {
        class: `seg ${current === g.group ? "active" : ""}`,
        attrs: { type: "button", role: "radio", "aria-checked": String(current === g.group) },
        on: { click: () => current !== g.group && pickKind(g.group === "audio" ? state.lastAudio : state.lastVideo) },
      },
      icon(g.icon, 18),
      h("span", { class: "seg-text" }, h("strong", {}, g.label), h("small", {}, g.hint)),
    );
    b.disabled = !g.enabled;
    top.append(b);
  }
  const chips = h("div", { class: "chips", attrs: { role: "radiogroup", "aria-label": "Biçim" } });
  for (const f of FORMATS.filter((x) => x.group === current)) {
    const unavailable = f.kind === "webm" && info.webm === false;
    const chip = h(
      "button",
      {
        class: `chip ${state.kind === f.kind ? "active" : ""}`,
        attrs: { type: "button", role: "radio", "aria-checked": String(state.kind === f.kind) },
        title: unavailable ? "Bu site WEBM biçiminde video sunmuyor." : formatDesc(f),
        on: { click: () => pickKind(f.kind) },
      },
      f.label,
      f.kind === "mp3" || f.kind === "mp4" ? h("span", { class: "chip-tag" }, "önerilen") : null,
    );
    chip.disabled = unavailable;
    chips.append(chip);
  }
  const picked = FORMATS.find((f) => f.kind === state.kind);
  const desc = picked ? formatDesc(picked) : "";
  return h("div", { class: "format-picker" }, top, chips, h("p", { class: "format-desc" }, desc));
}

function mp3QualitySelect() {
  const select = h("select", {
    class: "select",
    attrs: { "aria-label": "MP3 kalitesi" },
    on: {
      change: (e) => {
        const v = (e.target as HTMLSelectElement).value;
        state.audioQuality = v === "" ? null : Number(v);
        render();
      },
    },
  });
  for (const q of MP3_QUALITIES) {
    const o = h("option", { attrs: { value: q.value === null ? "" : String(q.value) } }, q.label);
    if (q.value === state.audioQuality) o.selected = true;
    select.append(o);
  }
  return h(
    "label",
    { class: "field", title: "YouTube sesi en fazla ~160 kbps'tir. 320 kbps daha büyük dosya demektir, daha iyi ses değil; bazı cihazlar sabit bit hızı istediği için var." },
    h("span", { class: "field-label" }, "Kalite"),
    select,
  );
}

function subtitlesToggle() {
  const input = h("input", {
    attrs: { type: "checkbox" },
    on: {
      change: (e) => {
        state.subtitles = (e.target as HTMLInputElement).checked;
      },
    },
  });
  input.checked = state.subtitles;
  return h(
    "label",
    { class: "switch-row", title: "Videoda Türkçe ya da İngilizce altyazı varsa dosyanın içine eklenir. Oynatıcıdan açıp kapatabilirsin." },
    h("span", { class: "switch" }, input, h("span", { class: "slider" })),
    h("span", {}, "Altyazıları ekle"),
  );
}

/** Quality, subtitle and tag controls for the chosen format. */
function formatControls(resolutions: number[], canSplitArtist: boolean): Child[] {
  if (isAudioKind(state.kind)) {
    return [state.kind === "mp3" && mp3QualitySelect(), canSplitArtist && artistToggle()];
  }
  return [resolutions.length > 0 && qualitySelect(resolutions), subtitlesToggle()];
}

function estimateFor(info: VideoInfo): number | null {
  const sizes = info.sizes;
  switch (state.kind) {
    case "mp3":
      return state.audioQuality && info.duration ? Math.round((info.duration * state.audioQuality * 1000) / 8) : sizes.mp3;
    case "m4a":
    case "opus":
    case "flac":
    case "wav":
      return sizes[state.kind];
    default:
      return sizes[state.kind][String(state.res)] ?? null;
  }
}

function qualitySelect(resolutions: number[]) {
  const select = h("select", {
    class: "select",
    attrs: { "aria-label": "Video kalitesi" },
    on: {
      change: (e) => {
        state.res = Number((e.target as HTMLSelectElement).value);
        state.resTouched = true;
        render();
      },
    },
  });
  for (const r of resolutions) {
    const o = h("option", { attrs: { value: String(r) } }, resolutionLabel(r));
    if (r === state.res) o.selected = true;
    select.append(o);
  }
  return h("label", { class: "field" }, h("span", { class: "field-label" }, "Kalite"), select);
}

function artistToggle() {
  const input = h("input", {
    attrs: { type: "checkbox" },
    on: {
      change: (e) => {
        state.splitArtist = (e.target as HTMLInputElement).checked;
      },
    },
  });
  input.checked = state.splitArtist;
  return h(
    "label",
    { class: "switch-row", title: "Örneğin \"Sezen Aksu - Firuze\" başlığından sanatçı ve şarkı etiketlerini doldurur." },
    h("span", { class: "switch" }, input, h("span", { class: "slider" })),
    h("span", {}, "Sanatçı ve şarkı adını başlıktan ayır"),
  );
}

/** Where the files will go, with a one-click way to change it. */
function saveLocationRow() {
  const s = state.settings;
  return h(
    "div",
    { class: "save-row" },
    icon("folder", 14),
    s.askFolder
      ? h("span", {}, "Kaydedilecek yer: ", h("strong", {}, "her indirmede sorulacak"))
      : h("span", {}, "Kaydedilecek yer: ", h("strong", { title: s.downloadDir }, friendlyPath(s.downloadDir))),
    h(
      "button",
      { class: "link-btn", attrs: { type: "button" }, on: { click: () => (s.askFolder ? openModal("settings") : changeDownloadDir()) } },
      s.askFolder ? "Ayarlar" : "Değiştir",
    ),
  );
}

async function changeDownloadDir() {
  const current = state.settings.downloadDir;
  const picked = await api.chooseFolder(current).catch(() => null);
  if (!picked || picked === current) return;
  if (!(await saveSettings({ downloadDir: picked }))) return;
  const where = friendlyPath(state.settings.downloadDir);
  if (state.settings.askFolder) {
    // The folder question will start here from now on.
    lastPickedDir = null;
    toast(`Klasör seçme penceresi artık burada açılacak: ${where}`, "success");
  } else {
    toast(`Yeni indirmeler buraya kaydedilecek: ${where}`, "success");
  }
}

function hiResNote() {
  // Macs with AV1 hardware (M3 and newer) get AV1 files QuickTime can play.
  const text = onWindows()
    ? "1080p üstü videolar VP9 biçiminde gelir. Bazı oynatıcılar bu biçimi açamaz, VLC ile sorunsuz oynar."
    : "1080p üstü videolar VP9 biçiminde gelir. QuickTime bu biçimi açamaz, IINA veya VLC ile sorunsuz oynar.";
  return state.kind === "mp4" && state.res > 1080 && !state.av1Hardware ? h("p", { class: "note" }, icon("info", 14), h("span", {}, text)) : null;
}

function renderVideo(info: VideoInfo) {
  const estimate = estimateFor(info);
  const playlistHint = /[?&]list=/.test(state.url)
    ? h(
        "div",
        { class: "playlist-hint" },
        icon("list", 16),
        h("span", {}, "Bu video bir oynatma listesinin parçası."),
        button("Tüm listeyi getir", () => fetchInfo(true), { class: "btn-ghost btn-sm" }),
      )
    : null;

  return h(
    "section",
    { class: "card media" },
    h(
      "div",
      { class: "media-top" },
      h("div", { class: "thumb-wrap" }, img(info.thumbnail, "thumb"), info.duration != null && h("span", { class: "duration" }, formatDuration(info.duration))),
      h(
        "div",
        { class: "media-body" },
        h(
          "div",
          { class: "badges" },
          info.creativeCommons
            ? h("span", { class: "badge badge-cc", title: info.license ?? "" }, "Creative Commons")
            : h("span", { class: "badge", title: info.license ?? "Lisans bilgisi yok" }, "Standart lisans"),
          info.site && h("span", { class: "badge badge-soft" }, siteLabel(info.site)),
        ),
        h("h3", { class: "media-title selectable" }, info.title),
        info.uploader && h("div", { class: "media-sub" }, info.uploader),
        h(
          "p",
          { class: "license-note" },
          info.creativeCommons
            ? "Serbest lisanslı içerik. Lisans koşullarına uy ve kaynak göstermeyi unutma."
            : "Yalnızca kişisel kullanım için indir. Paylaşma ya da yeniden yükleme.",
        ),
      ),
    ),
    playlistHint,
    h(
      "div",
      { class: "media-options" },
      formatPicker(info),
      h(
        "div",
        { class: "option-row" },
        ...formatControls(info.resolutions, info.titleHasSeparator),
        h("div", { class: "grow" }),
        estimate != null && h("span", { class: "estimate" }, `yaklaşık ${formatBytes(estimate)}`),
        button("İndir", startDownload, { class: "btn-primary btn-lg dl-btn", icon: "download" }),
      ),
      saveLocationRow(),
      hiResNote(),
    ),
  );
}

function renderPlaylist(list: PlaylistInfo) {
  // Ticking boxes only updates these elements; rebuilding the whole card
  // would throw away the list's scroll position.
  const rowEls = new Map<string, { row: HTMLElement; cb: HTMLInputElement }>();
  // The label is filled in by sync() below.
  const downloadBtn = button("Seçim yok", startDownload, { class: "btn-primary btn-lg dl-btn", icon: "download" });
  const selectAll = h("input", {
    attrs: { type: "checkbox", "aria-label": "Tümünü seç" },
    on: {
      change: (e) => {
        state.selected = (e.target as HTMLInputElement).checked ? new Set(list.entries.map((x) => x.id)) : new Set();
        sync();
      },
    },
  });
  const sync = () => {
    const count = state.selected.size;
    selectAll.checked = count === list.entries.length;
    selectAll.indeterminate = count > 0 && count < list.entries.length;
    for (const [id, { row, cb }] of rowEls) {
      cb.checked = state.selected.has(id);
      row.classList.toggle("off", !cb.checked);
    }
    downloadBtn.disabled = count === 0;
    const label = downloadBtn.querySelector("span");
    if (label) label.textContent = count > 0 ? `${count} öğeyi indir` : "Seçim yok";
  };
  playlistSync = sync;

  const rows = h("div", { class: "pl-list" });
  list.entries.forEach((entry, i) => {
    const cb = h("input", {
      attrs: { type: "checkbox" },
      on: {
        change: (e) => {
          if ((e.target as HTMLInputElement).checked) state.selected.add(entry.id);
          else state.selected.delete(entry.id);
          sync();
        },
      },
    });
    const row = h(
      "label",
      { class: "pl-row" },
      cb,
      h("span", { class: "pl-index" }, String(i + 1)),
      img(entry.thumbnail, "pl-thumb"),
      h("span", { class: "pl-title" }, entry.title),
      h("span", { class: "pl-duration" }, formatDuration(entry.duration)),
    );
    rowEls.set(entry.id, { row, cb });
    rows.append(row);
  });
  sync();

  return h(
    "section",
    { class: "card media playlist" },
    h(
      "div",
      { class: "pl-head" },
      h("div", { class: "pl-icon" }, icon("list", 22)),
      h(
        "div",
        { class: "media-body" },
        h("div", { class: "badges" }, h("span", { class: "badge badge-soft" }, "Oynatma listesi"), h("span", { class: "badge badge-soft" }, `${list.entries.length} video`)),
        h("h3", { class: "media-title selectable" }, list.title),
        list.uploader && h("div", { class: "media-sub" }, list.uploader),
      ),
      h("label", { class: "check-row compact" }, selectAll, h("span", {}, "Tümü")),
    ),
    rows,
    list.truncated && h("p", { class: "note" }, icon("info", 14), h("span", {}, "Liste çok uzun, ilk 1000 video gösteriliyor.")),
    h(
      "div",
      { class: "media-options" },
      formatPicker({ hasVideo: true, hasAudio: true }),
      h(
        "div",
        { class: "option-row" },
        ...formatControls(PLAYLIST_RESOLUTIONS, true),
        h("div", { class: "grow" }),
        downloadBtn,
      ),
      saveLocationRow(),
      hiResNote(),
    ),
  );
}

// -------------------------------------------------------------------- jobs

// Jobs added together (one playlist) keep their own order; newer batches
// appear above older ones. The backend numbers the batches.
function sortedJobs() {
  return [...state.jobs.values()].sort((a, b) => b.batch - a.batch || a.id - b.id);
}

/** Jobs removed in this session; a late update for one must not bring it back. */
const removedJobs = new Set<number>();

function paintJobs() {
  if (!jobListEl || !jobsHeaderEl) return;
  const jobs = sortedJobs();
  const finished = jobs.filter((j) => ["done", "error", "canceled"].includes(j.status)).length;
  const running = jobs.filter((j) => ["starting", "downloading", "processing"].includes(j.status)).length;
  const queued = jobs.filter((j) => j.status === "queued").length;
  const hidden = state.settings.hideDownloads;
  downloadsEl?.classList.toggle("is-empty", jobs.length === 0);
  downloadsEl?.classList.toggle("collapsed", hidden);
  // Rebuilt only when it changes: progress events arrive several times a
  // second, and a button replaced mid-click would swallow the click.
  const done = jobs.filter((j) => j.status === "done").length;
  const failed = jobs.filter((j) => j.status === "error").length;
  const headerKey = JSON.stringify([running, queued, finished, done, failed, hidden]);
  if (jobsHeaderEl.dataset.key !== headerKey) {
    jobsHeaderEl.dataset.key = headerKey;
    // A rebuild must not drop keyboard focus from its buttons.
    const focused = (document.activeElement as HTMLElement | null)?.closest<HTMLElement>(".section-head [data-action]")?.dataset.action;
    const clear = button("Bitenleri temizle", () => run(() => api.clearFinished()), { class: "btn-ghost btn-sm", icon: "trash" });
    clear.dataset.action = "clear";
    const toggle = button(hidden ? "Göster" : "Gizle", toggleDownloads, {
      class: "btn-ghost btn-sm",
      icon: hidden ? "chevronDown" : "chevronUp",
      title: hidden ? "İndirilenler listesini göster" : "İndirilenler listesini gizle",
    });
    toggle.dataset.action = "toggle";
    toggle.setAttribute("aria-expanded", String(!hidden));
    jobsHeaderEl.replaceChildren();
    append(jobsHeaderEl, [
      h("h2", {}, "İndirilenler"),
      (running > 0 || queued > 0) && h("span", { class: "count-pill" }, [running > 0 && `${running} iniyor`, queued > 0 && `${queued} sırada`].filter(Boolean).join(" · ")),
      // Folded away, the header still says what is in the list.
      hidden && running === 0 && queued === 0 && h("span", { class: "section-note" }, [done > 0 && `${done} dosya`, failed > 0 && `${failed} hata`].filter(Boolean).join(" · ")),
      h("div", { class: "grow" }),
      !hidden && finished > 0 && clear,
      toggle,
    ]);
    if (focused) jobsHeaderEl.querySelector<HTMLElement>(`[data-action="${focused}"]`)?.focus();
  }
  if (jobs.length === 0) {
    jobRows.clear();
    // Until the first download, show how Ambar is used.
    const step = (n: number, title: string, sub: string) =>
      h("div", { class: "step" }, h("span", { class: "step-num" }, String(n)), h("div", { class: "step-text" }, h("strong", {}, title), h("small", {}, sub)));
    jobListEl.replaceChildren(
      h(
        "div",
        { class: "starter" },
        step(1, "Bağlantıyı kopyala", "sitenin Paylaş düğmesinden ya da adres çubuğundan"),
        step(2, onWindows() ? "Ambar'da Ctrl+V'ye bas" : "Ambar'da ⌘V'ye bas", "ya da linki pencereye sürükle"),
        step(3, "Ses ya da video seç", "sonra İndir'e bas"),
      ),
    );
    return;
  }
  jobListEl.querySelector(".starter")?.remove();
  // Rebuild in order; rows are reused when their status did not change.
  const fragment: HTMLElement[] = [];
  for (const job of jobs) {
    let row = jobRows.get(job.id);
    if (!row || row.status !== job.status) {
      const el = renderJob(job);
      // Hidden rows can't animate; the animation would wait and play later.
      if (!hidden) {
        if (!row) enter(el);
        else if (job.status === "done") flash(el, "just-done");
      }
      row = { el, status: job.status };
      jobRows.set(job.id, row);
    } else {
      updateJobRow(row.el, job);
    }
    fragment.push(row.el);
  }
  for (const id of [...jobRows.keys()]) if (!state.jobs.has(id)) jobRows.delete(id);
  const current = [...jobListEl.children];
  if (current.length !== fragment.length || current.some((el, i) => el !== fragment[i])) jobListEl.replaceChildren(...fragment);
}

/** Folds the downloads list away or back, without rebuilding the page. */
async function toggleDownloads() {
  const previous = state.settings;
  state.settings = { ...previous, hideDownloads: !previous.hideDownloads };
  if (!state.settings.hideDownloads) {
    // Leftover one-off animations must not all play at once.
    for (const { el } of jobRows.values()) el.classList.remove("enter", "just-added", "just-done");
  }
  paintJobs();
  try {
    state.settings = await api.saveSettings(state.settings);
  } catch (e) {
    state.settings = previous;
    paintJobs();
    toast(errorText(e), "error");
  }
}

/** Plays a one-off highlight animation on a row. */
function flash(el: HTMLElement, cls: string) {
  el.classList.add(cls);
  const end = (e: AnimationEvent) => {
    if (e.target !== el) return;
    el.classList.remove(cls);
    el.removeEventListener("animationend", end);
  };
  el.addEventListener("animationend", end);
}

function jobMeta(job: JobView): string {
  const parts: string[] = [];
  switch (job.status) {
    case "queued":
      return "Sırada bekliyor";
    case "starting":
      return job.stage ?? "Hazırlanıyor";
    case "downloading":
      // The percentage has its own spot next to the title.
      if (job.stage) parts.push(job.stage);
      if (job.speed) parts.push(formatSpeed(job.speed));
      if (job.eta != null) parts.push(formatEta(job.eta));
      return parts.join(" · ");
    case "processing":
      return job.stage ?? "Son rötuşlar yapılıyor";
    case "done":
      return "İndirildi";
    case "error":
      return job.error ?? "Bir hata oluştu";
    case "canceled":
      return "İptal edildi";
  }
}

function kindLabel(job: JobView) {
  const name = job.kind.toUpperCase();
  if (isAudioKind(job.kind) || !job.res) return name;
  return `${name} · ${job.res >= 4320 ? "8K" : job.res >= 2160 ? "4K" : job.res >= 1440 ? "2K" : `${job.res}p`}`;
}

/** The "show the file" button, named after the file manager it opens. */
function revealLabel() {
  return onWindows() ? "Klasörde göster" : "Finder'da göster";
}

function renderJob(job: JobView): HTMLElement {
  const active = ["starting", "downloading", "processing"].includes(job.status);
  const actions = h("div", { class: "job-actions" });
  if (active || job.status === "queued") {
    actions.append(iconButton("close", "İptal et", () => run(() => api.cancelJob(job.id))));
  } else if (job.status === "done") {
    actions.append(
      button("Aç", () => run(() => api.openJobFile(job.id)), { class: "btn-ghost btn-sm", icon: "play" }),
      button(revealLabel(), () => run(() => api.revealJobFile(job.id)), {
        class: "btn-ghost btn-sm reveal-btn",
        icon: "folder",
        title: onWindows() ? "Dosyayı klasöründe göster" : "Dosyayı Finder'da göster",
      }),
      iconButton("close", "Listeden kaldır", () => run(() => api.removeJob(job.id))),
    );
  } else {
    actions.append(
      button("Tekrar dene", () => run(() => api.retryJob(job.id)), { class: "btn-ghost btn-sm", icon: "retry" }),
      iconButton("close", "Listeden kaldır", () => run(() => api.removeJob(job.id))),
    );
  }
  const statusIcon =
    job.status === "done" ? h("span", { class: "job-status ok" }, icon("check", 14)) : job.status === "error" ? h("span", { class: "job-status bad" }, icon("alert", 14)) : null;
  const el = h(
    "div",
    {
      class: `job status-${job.status}`,
      attrs: { "data-id": String(job.id) },
      on: {
        // Like Finder and Explorer: double-clicking a finished download opens it.
        dblclick: (e) => {
          if (job.status === "done" && !(e.target as Element).closest(".job-actions")) run(() => api.openJobFile(job.id));
        },
      },
    },
    h("div", { class: "job-thumb-wrap" }, img(job.thumbnail, "job-thumb"), h("span", { class: "kind-badge" }, kindLabel(job)), statusIcon),
    h(
      "div",
      { class: "job-body" },
      h("div", { class: "job-title-row" }, h("div", { class: "job-title", title: job.title }, job.title), h("span", { class: "job-pct" })),
      h("div", { class: "progress" }, h("div", { class: "bar" })),
      h("div", { class: `job-meta ${job.status === "error" ? "selectable" : ""}` }),
    ),
    actions,
  );
  updateJobRow(el, job);
  return el;
}

function updateJobRow(el: HTMLElement, job: JobView) {
  const progress = el.querySelector<HTMLElement>(".progress");
  const bar = el.querySelector<HTMLElement>(".bar");
  const meta = el.querySelector<HTMLElement>(".job-meta");
  const indeterminate = (job.status === "starting" || job.status === "processing") && job.progress == null;
  progress?.classList.toggle("indeterminate", indeterminate);
  progress?.classList.toggle("hidden", job.status === "queued" || job.status === "canceled" || job.status === "error" || job.status === "done");
  if (bar) bar.style.width = `${Math.round((job.status === "done" ? 1 : job.progress ?? 0) * 100)}%`;
  const pct = el.querySelector<HTMLElement>(".job-pct");
  if (pct) pct.textContent = job.status === "downloading" && job.progress != null ? formatPercent(job.progress) : "";
  if (!meta) return;
  const dir = job.status === "done" ? dirName(job.filePath) : "";
  if (dir) {
    // Says where the file went, which matters now that the folder can change.
    meta.replaceChildren(h("span", { class: "done-label" }, "İndirildi"), ` · ${friendlyPath(dir)}`);
    meta.title = job.filePath ?? "";
  } else {
    meta.textContent = jobMeta(job);
  }
}

let finishedSinceIdle: JobView[] = [];

function isBusy(job: JobView) {
  return !["done", "error", "canceled"].includes(job.status);
}

function onJobEvent(e: JobEvent) {
  if (e.type === "removed") {
    state.jobs.delete(e.id);
    removedJobs.add(e.id);
  } else {
    if (removedJobs.has(e.job.id)) return;
    const before = state.jobs.get(e.job.id);
    state.jobs.set(e.job.id, e.job);
    if (before && before.status !== "done" && e.job.status === "done") finishedSinceIdle.push(e.job);
    // Announce once everything in flight has settled.
    if (finishedSinceIdle.length > 0 && ![...state.jobs.values()].some(isBusy)) {
      const done = finishedSinceIdle;
      finishedSinceIdle = [];
      const only = done.length === 1 ? done[0]! : null;
      if (only) toast(`İndirildi: ${only.title}`, "success", { label: revealLabel(), onClick: () => run(() => api.revealJobFile(only.id)) });
      else toast(`${done.length} indirme tamamlandı`, "success");
    }
  }
  paintJobs();
  // The update's "n downloads running" question counts along.
  if (state.appUpdate.confirming) paintAppUpdate();
}

// ------------------------------------------------------------------ actions

async function run(fn: () => Promise<unknown>) {
  try {
    await fn();
  } catch (e) {
    toast(errorText(e), "error");
  }
}

function focusUrl() {
  requestAnimationFrame(() => (document.getElementById("url-input") as HTMLInputElement | null)?.focus());
}

/** Guess whether "A - B" in a title means "Artist - Song". */
function looksLikeArtistTitle(info: VideoInfo): boolean {
  if (!info.titleHasSeparator || info.hasMusicTags) return false;
  const before = info.title.split(/ [-–] /)[0]?.toLocaleLowerCase("tr") ?? "";
  const uploader = (info.uploader ?? "")
    .toLocaleLowerCase("tr")
    .replace(/\s*-\s*topic$/, "")
    .replace(/vevo$/, "")
    .replace(/\s*(official|resmi)\s*$/, "")
    .trim();
  return uploader.length > 1 && (before.includes(uploader) || uploader.includes(before));
}

function initOptions(info: MediaInfo) {
  state.kindTouched = false;
  state.resTouched = false;
  state.kind = state.settings.defaultKind;
  if (isAudioKind(state.kind)) state.lastAudio = state.kind;
  else state.lastVideo = state.kind;
  if (info.type === "video") {
    if (!info.hasVideo && !isAudioKind(state.kind)) state.kind = state.lastAudio;
    if (!info.hasAudio && isAudioKind(state.kind)) state.kind = state.lastVideo;
    // Not remembered as a choice: the saved default stays WEBM.
    if (state.kind === "webm" && !info.webm) state.kind = "mkv";
    const wanted = state.settings.defaultRes;
    state.res = info.resolutions.find((r) => r <= wanted) ?? info.resolutions[info.resolutions.length - 1] ?? wanted;
    state.splitArtist = looksLikeArtistTitle(info);
  } else {
    state.res = PLAYLIST_RESOLUTIONS.find((r) => r <= state.settings.defaultRes) ?? PLAYLIST_RESOLUTIONS[PLAYLIST_RESOLUTIONS.length - 1]!;
    state.splitArtist = false;
    state.selected = new Set(info.entries.map((e) => e.id));
  }
}

const SLOW_TEXT = "Site biraz yavaş yanıt veriyor, az kaldı…";

async function fetchInfo(playlist: boolean) {
  // "youtube.com/watch?v=…" without https:// or a link inside other text still works.
  const url = extractUrl(state.url) ?? state.url.trim();
  state.url = url;
  if (!looksLikeUrl(url)) {
    state.info = null;
    state.fetchErrorRetryable = false;
    state.fetchError = url ? "Bu bir bağlantıya benzemiyor. https:// ile başlayan tam adresi yapıştır." : "Önce bir bağlantı yapıştır.";
    render();
    return;
  }
  const token = ++state.fetchToken;
  state.fetching = true;
  state.fetchSlow = false;
  state.fetchPlaylist = playlist || (/[?&]list=/.test(url) && !/[?&]v=/.test(url));
  state.fetchError = null;
  state.info = null;
  render();
  setTimeout(() => {
    if (token !== state.fetchToken || !state.fetching) return;
    state.fetchSlow = true;
    const text = document.querySelector(".skeleton-text");
    if (text) text.textContent = SLOW_TEXT;
  }, 8000);
  try {
    const info = await api.fetchInfo(url, playlist);
    if (token !== state.fetchToken) return;
    initOptions(info);
    state.info = info;
    state.freshCard = true;
  } catch (e) {
    if (token !== state.fetchToken) return;
    state.fetchError = errorText(e);
    state.fetchErrorRetryable = true;
  } finally {
    if (token === state.fetchToken) {
      state.fetching = false;
      render();
    }
  }
}

// Guards against double-clicks: the same request is ignored while it is
// being added and for a moment afterwards. Different requests (e.g. MP3,
// then MP4 of the same video) always go through.
let lastEnqueue: { key: string; at: number } | null = null;
/** Folder picked last time "ask every time" was on, offered first next time. */
let lastPickedDir: string | null = null;
let choosingFolder = false;

async function startDownload() {
  const info = state.info;
  if (!info) return;
  const audio = isAudioKind(state.kind);
  const base = {
    kind: state.kind,
    res: audio ? null : state.res,
    splitArtist: audio && state.splitArtist,
    audioQuality: state.kind === "mp3" ? state.audioQuality : null,
    subtitles: !audio && state.subtitles,
  };
  const requests: DownloadRequest[] =
    info.type === "video"
      ? [{ url: info.url || state.url.trim(), title: info.title, thumbnail: info.thumbnail, ...base }]
      : info.entries.filter((e) => state.selected.has(e.id)).map((e) => ({ url: e.url, title: e.title, thumbnail: e.thumbnail, ...base }));
  if (requests.length === 0) return;
  if (state.settings.askFolder) {
    // One question per click, also for a whole playlist; a double-click
    // must not open a second one.
    if (choosingFolder) return;
    choosingFolder = true;
    const picked = await api.chooseFolder(lastPickedDir ?? state.settings.downloadDir, "Nereye kaydedilsin?").catch(() => null).finally(() => (choosingFolder = false));
    if (!picked) return;
    lastPickedDir = picked;
    for (const r of requests) r.saveDir = picked;
  }
  const key = JSON.stringify(requests.map((r) => [r.url, r.kind, r.res, r.splitArtist, r.audioQuality, r.subtitles, r.saveDir]));
  if (lastEnqueue && lastEnqueue.key === key && performance.now() - lastEnqueue.at < 1500) return;
  lastEnqueue = { key, at: performance.now() };
  try {
    const created = await api.enqueue(requests);
    paintJobs();
    showAdded(created);
    if (requests.length > 1) toast(`${requests.length} öğe indirme listesine eklendi`, "success");
    // Remember only what the user chose; values picked automatically
    // (e.g. 720p because the video has nothing higher) must not become
    // the new default.
    const next = {
      ...state.settings,
      defaultKind: state.kindTouched ? state.kind : state.settings.defaultKind,
      defaultRes: !audio && state.resTouched ? state.res : state.settings.defaultRes,
    };
    if (next.defaultKind !== state.settings.defaultKind || next.defaultRes !== state.settings.defaultRes) {
      api.saveSettings(next).then((s) => (state.settings = s)).catch(() => {});
    }
  } catch (e) {
    lastEnqueue = null; // let the user try the same thing again right away
    toast(errorText(e), "error");
  }
}

const reducedMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

/** Points at the rows just added and confirms on the button itself. */
function showAdded(created: JobView[]) {
  requestAnimationFrame(() => {
    for (const job of created) {
      const el = jobRows.get(job.id)?.el;
      if (!el || state.settings.hideDownloads) continue;
      // Removed once the ring is done; a row re-inserted later must not replay it.
      el.classList.add("just-added");
      const end = (e: AnimationEvent) => {
        if (e.target !== el || e.animationName !== "added-ring") return;
        el.classList.remove("just-added");
        el.removeEventListener("animationend", end);
      };
      el.addEventListener("animationend", end);
    }
    const first = created[0];
    if (first) jobRows.get(first.id)?.el.scrollIntoView({ block: "nearest", behavior: reducedMotion() ? "auto" : "smooth" });
  });
  const btn = document.querySelector<HTMLButtonElement>(".dl-btn");
  const label = btn?.querySelector("span");
  const glyph = btn?.querySelector("svg");
  if (!btn || !label || !glyph) return;
  const text = label.textContent;
  // Keep the width so the row around the button doesn't reflow.
  const width = btn.getBoundingClientRect().width;
  btn.style.width = `${width}px`;
  const check = icon("check", 16);
  glyph.replaceWith(check);
  label.textContent = "Eklendi";
  if (btn.scrollWidth > width + 1) check.remove();
  btn.disabled = true;
  setTimeout(() => {
    if (!btn.isConnected) return;
    if (check.isConnected) check.replaceWith(glyph);
    else btn.prepend(glyph);
    btn.style.width = "";
    btn.disabled = false;
    label.textContent = text;
    // The playlist's label and state depend on the current selection.
    playlistSync?.();
  }, 1200);
}

// ------------------------------------------------------------------- modals

function openModal(which: "settings" | "about") {
  state.modalFresh = state.modal === null;
  state.modal = which;
  render();
}

function closeModal() {
  state.modal = null;
  render();
}

function renderModal(): HTMLElement | null {
  if (!state.modal) return null;
  const body = state.modal === "settings" ? renderSettings() : renderAbout();
  const fresh = state.modalFresh ? " fresh" : "";
  state.modalFresh = false;
  return h(
    "div",
    {
      class: `modal-backdrop${fresh}`,
      on: {
        click: (e) => {
          if (e.target === e.currentTarget) closeModal();
        },
      },
    },
    h(
      "div",
      { class: `modal${fresh}`, attrs: { role: "dialog", "aria-modal": "true" } },
      h("div", { class: "modal-head" }, h("h2", {}, state.modal === "settings" ? "Ayarlar" : "Hakkında"), iconButton("close", "Kapat", closeModal)),
      body,
    ),
  );
}

async function saveSettings(patch: Partial<Settings>): Promise<boolean> {
  try {
    state.settings = await api.saveSettings({ ...state.settings, ...patch });
    render();
    return true;
  } catch (e) {
    toast(errorText(e), "error");
    // Put every control back to the value that is really in effect.
    render();
    return false;
  }
}

function settingRow(label: string, hint: string | null, control: Node) {
  return h("div", { class: "setting" }, h("div", { class: "setting-text" }, h("div", { class: "setting-label" }, label), hint && h("div", { class: "setting-hint" }, hint)), control);
}

function smallSegmented<T extends string | number>(values: { value: T; label: string }[], current: T, onPick: (v: T) => void) {
  return h(
    "div",
    { class: "segmented small" },
    ...values.map((v) =>
      h("button", { class: `seg ${v.value === current ? "active" : ""}`, attrs: { type: "button" }, on: { click: () => onPick(v.value) } }, v.label),
    ),
  );
}

function renderSettings() {
  const s = state.settings;
  const cover = h("input", { attrs: { type: "checkbox" }, on: { change: (e) => saveSettings({ squareCover: (e.target as HTMLInputElement).checked }) } });
  cover.checked = s.squareCover;
  const ask = h("input", { attrs: { type: "checkbox" }, on: { change: (e) => saveSettings({ askFolder: (e.target as HTMLInputElement).checked }) } });
  ask.checked = s.askFolder;

  const channel = (value: Settings["ytdlpChannel"], title: string, hint: string) =>
    h(
      "button",
      {
        class: `choice ${s.ytdlpChannel === value ? "active" : ""}`,
        attrs: { type: "button" },
        on: {
          click: async () => {
            if (s.ytdlpChannel === value) return;
            if (await saveSettings({ ytdlpChannel: value })) checkUpdates(false, true);
          },
        },
      },
      h("span", { class: "radio" }),
      h("span", {}, h("strong", {}, title), h("small", {}, hint)),
    );

  const engineLines = h(
    "div",
    { class: "engine-versions" },
    ...ENGINE_ORDER.map((id) => {
      const e = state.engines.engines.find((x) => x.id === id);
      return h("div", { class: "engine-version" }, h("span", {}, ENGINE_INFO[id].name), h("code", {}, e?.version ?? "kurulu değil"));
    }),
  );

  return h(
    "div",
    { class: "modal-body" },
    settingRow(
      "İndirme klasörü",
      null,
      h(
        "div",
        { class: "folder-row" },
        h("code", { class: "path", title: s.downloadDir }, friendlyPath(s.downloadDir)),
        button("Değiştir", changeDownloadDir, { class: "btn-ghost btn-sm" }),
      ),
    ),
    settingRow(
      "Her indirmede nereye kaydedileceğini sor",
      "İndir'e bastığında klasör seçme penceresi açılır. Kapalıysa her şey yukarıdaki klasöre kaydedilir.",
      h("label", { class: "switch" }, ask, h("span", { class: "slider" })),
    ),
    settingRow(
      "Aynı anda indirme",
      "Listeleri indirirken kaç dosyanın birlikte ineceği.",
      smallSegmented([1, 2, 3].map((n) => ({ value: n, label: String(n) })), s.maxConcurrent, (n) => saveSettings({ maxConcurrent: n })),
    ),
    settingRow("Kapak resmini kare kırp", "Müzik dosyalarında kapak, telefon ve müzik uygulamalarında düzgün görünür.", h("label", { class: "switch" }, cover, h("span", { class: "slider" }))),
    renderAppVersionSetting(),
    h(
      "div",
      { class: "setting column" },
      h("div", { class: "setting-label" }, "yt-dlp güncelleme kanalı"),
      h(
        "div",
        { class: "choices" },
        channel("nightly", "Gece sürümü (önerilen)", "Site değişikliklerine birkaç gün içinde uyum sağlar. yt-dlp ekibinin önerisi budur."),
        channel("stable", "Kararlı sürüm", "Ayda bir yayınlanır. Bazen site değişikliklerinin gerisinde kalır."),
      ),
    ),
    h(
      "div",
      { class: "setting column" },
      // Named apart from Ambar's own "Güncellemeleri denetle" above.
      h("div", { class: "setting-head" }, h("div", { class: "setting-label" }, "Bileşenler"), button(state.checkingUpdates ? "Denetleniyor…" : "Bileşenleri denetle", () => checkUpdates(true), { class: "btn-ghost btn-sm", icon: "retry", disabled: state.checkingUpdates })),
      engineLines,
      h("div", { class: "setting-hint" }, "Ambar, yt-dlp'yi 6 saatte bir, diğerlerini haftada bir kendiliğinden günceller."),
    ),
    h("div", { class: "modal-foot" }, button("Hakkında ve lisanslar", () => openModal("about"), { class: "btn-ghost", icon: "info" })),
  );
}

function linkButton(label: string, url: string) {
  return h("button", { class: "link-btn", attrs: { type: "button" }, on: { click: () => run(() => api.openLink(url)) } }, label, icon("external", 12));
}

function renderAbout() {
  const component = (name: string, license: string, desc: string, links: [string, string][]) =>
    h("div", { class: "component" }, h("div", { class: "component-head" }, h("strong", {}, name), h("span", { class: "badge badge-soft" }, license)), h("p", {}, desc), h("div", { class: "component-links" }, ...links.map(([l, u]) => linkButton(l, u))));
  return h(
    "div",
    { class: "modal-body about" },
    h("div", { class: "about-head" }, h("img", { class: "about-logo", attrs: { src: "/logo.svg", alt: "" } }), h("div", {}, h("h3", {}, "Ambar"), h("small", { class: "muted" }, `Sürüm ${state.appVersion}`))),
    h("p", {}, "Ambar; yt-dlp, FFmpeg ve Deno gibi açık kaynak projelerin üzerine kurulu ücretsiz bir arayüzdür. İndirmeyi bu programlar yapar, Ambar onları senin için kolaylaştırır. Bu programlar Ambar'nın içinde gelmez, ilk açılışta resmi kaynaklarından indirilip doğrulanır."),
    h("h4", {}, "Kullanım notu"),
    h("ul", { class: "notice-list" }, ...NOTICE_POINTS.map((p) => h("li", {}, p))),
    h("h4", {}, "Bileşenler ve lisanslar"),
    component("Kova", "MIT", "Ambar’ın temel aldığı açık kaynak proje.", [["Kaynak kodu", "https://github.com/alpunlu12-commits/kova"]]),
    component("yt-dlp", "Unlicense · GPLv3+", "Medyayı bulan ve indiren motor. Kaynak kodu Unlicense, hazır derlemesi GPLv3+ bileşenler içerir.", [["GitHub", "https://github.com/yt-dlp/yt-dlp"]]),
    onWindows()
      ? component("FFmpeg", "GPL", "Ses ve görüntü dönüştürme. Windows derlemesi Gyan Doshi tarafından gyan.dev'de yayınlanır.", [["ffmpeg.org", "https://ffmpeg.org/"], ["gyan.dev", "https://www.gyan.dev/ffmpeg/builds/"]])
      : component("FFmpeg", "GPL", "Ses ve görüntü dönüştürme. macOS derlemesi Martin Riedl tarafından imzalanarak yayınlanır.", [["ffmpeg.org", "https://ffmpeg.org/"], ["Derleme", "https://ffmpeg.martin-riedl.de/"]]),
    component("Deno", "MIT", "yt-dlp'nin bazı siteler için ihtiyaç duyduğu JavaScript çalıştırıcı.", [["deno.com", "https://deno.com/"]]),
  );
}

// Update checks run one at a time. A request that arrives while one is
// running is remembered and runs right after it. Automatic checks stay
// quiet; only checks the user started report errors and "all up to date".
const BUSY_TEXT = "Bileşen denetimi zaten sürüyor.";
let updateRun: Promise<void> | null = null;
let pendingCheck: { force: boolean; manual: boolean } | null = null;
let manualCheck = false;
let updateErrors = 0;
let updateTimer: number | null = null;

function checkUpdates(force: boolean, manual = force) {
  if (updateRun) {
    pendingCheck = { force: force || !!pendingCheck?.force, manual: manual || !!pendingCheck?.manual };
    return;
  }
  updateRun = runUpdateCheck(force, manual).finally(() => {
    updateRun = null;
    const next = pendingCheck;
    pendingCheck = null;
    if (next) checkUpdates(next.force, next.manual);
  });
}

async function runUpdateCheck(force: boolean, manual: boolean) {
  state.checkingUpdates = true;
  manualCheck = manual;
  updateErrors = 0;
  if (state.modal) render();
  try {
    const outcomes = await api.updateEngines(force);
    state.engines = await api.enginesState();
    for (const o of outcomes) toast(`${ENGINE_INFO[o.engine].name} güncellendi: ${o.to}`, "success");
    if (manual && outcomes.length === 0 && updateErrors === 0) toast("Tüm bileşenler güncel", "success");
  } catch (e) {
    const message = errorText(e);
    if (message === BUSY_TEXT) {
      // Something else (e.g. an install) holds the engines; try again soon.
      setTimeout(() => checkUpdates(force, manual), 10_000);
    } else if (manual) {
      toast(message, "error");
    }
  } finally {
    state.checkingUpdates = false;
    manualCheck = false;
    if (state.modal) render();
  }
}

/** Checks hourly while the app stays open; the backend decides what is due. */
function startUpdateTimer() {
  if (updateTimer !== null) return;
  updateTimer = window.setInterval(() => checkUpdates(false, false), 60 * 60 * 1000);
}

// -------------------------------------------------------------- app update

// Ambar looks for a newer version of itself a few seconds after it is ready
// and then once a day. Nothing is installed without a click: installing
// restarts Ambar, and that stops running downloads.

type AppUpdateView = "hidden" | "current" | "check-error" | "found" | "confirm" | "installing" | "install-error";
type AppUpdateWhere = "bar" | "settings";

const APP_CHECK_DELAY = 5000;
const APP_CHECK_EVERY = 24 * 60 * 60 * 1000;
let appCheckTimer: number | null = null;
/** When a check last succeeded; a failed one is tried again on the next hourly tick. */
let lastAppCheck = 0;
let appProgressFrame = 0;

function startAppUpdateChecks() {
  if (appCheckTimer !== null) return;
  // A little later, so start-up and the engine check go first.
  setTimeout(() => checkAppUpdate(false), APP_CHECK_DELAY);
  // Hourly ticks against the clock: a 24-hour timer stalls while the computer sleeps.
  appCheckTimer = window.setInterval(() => {
    if (Date.now() - lastAppCheck >= APP_CHECK_EVERY) checkAppUpdate(false);
  }, 60 * 60 * 1000);
}

/** Automatic checks stay quiet; a check started in Settings also reports "up to date" and errors. */
async function checkAppUpdate(manual: boolean) {
  const u = state.appUpdate;
  if (u.installing || needsOnboarding()) return;
  if (manual) {
    u.manual = true;
    u.upToDate = false;
    u.checkError = null;
  }
  if (u.checking) {
    // The check already running answers this one too.
    paintAppUpdate();
    return;
  }
  u.checking = true;
  paintAppUpdate();
  try {
    const found = await api.checkAppUpdate();
    lastAppCheck = Date.now();
    // An install started meanwhile keeps the version it is installing.
    if (!u.installing) u.found = found;
    if (u.manual) u.upToDate = !found;
  } catch (e) {
    if (u.manual) u.checkError = errorText(e);
  } finally {
    u.checking = false;
    u.manual = false;
    paintAppUpdate();
  }
}

function runningDownloads() {
  return [...state.jobs.values()].filter(isBusy).length;
}

/** Downloads and installs the update, then Ambar restarts. Asks first while downloads would be cut short. */
async function installAppUpdate(confirmed: boolean) {
  const u = state.appUpdate;
  if (!u.found || u.installing) return;
  if (!confirmed && runningDownloads() > 0) {
    u.confirming = true;
    paintAppUpdate();
    return;
  }
  Object.assign(u, { confirming: false, installing: true, installed: false, installError: null, progress: null });
  paintAppUpdate();
  try {
    await api.installAppUpdate();
    // Ambar normally restarts (on Windows the installer closes it) before this returns.
    u.installed = true;
  } catch (e) {
    u.installing = false;
    u.installError = errorText(e);
  }
  paintAppUpdate();
}

function onAppUpdateProgress(p: AppUpdateProgress) {
  const u = state.appUpdate;
  if (!u.installing) return;
  u.progress = p;
  // Chunks arrive many times a second; one paint per frame is plenty.
  if (!appProgressFrame) {
    appProgressFrame = requestAnimationFrame(() => {
      appProgressFrame = 0;
      paintAppUpdate();
    });
  }
}

/** "Sonra": the bar goes away for this version until Ambar is opened again. */
function dismissAppUpdate() {
  const u = state.appUpdate;
  u.dismissed = u.found?.version ?? null;
  u.confirming = false;
  u.installError = null;
  paintAppUpdate();
}

function appUpdateView(where: AppUpdateWhere): AppUpdateView {
  const u = state.appUpdate;
  if (u.found) {
    if (u.installing) return "installing";
    if (u.confirming) return "confirm";
    if (u.installError) return "install-error";
    // "Sonra" only quiets the bar; Settings still offers the update.
    return where === "bar" && u.dismissed === u.found.version ? "hidden" : "found";
  }
  if (where === "bar") return "hidden";
  if (u.checkError) return "check-error";
  return u.upToDate ? "current" : "hidden";
}

/** Downloading until every byte is in, then installing; restarting once the install returned. */
function installStage(): "downloading" | "installing" | "restarting" {
  const { installed, progress: p } = state.appUpdate;
  if (installed) return "restarting";
  return p && p.total && p.downloaded >= p.total ? "installing" : "downloading";
}

/** "4,2 MB / 9,8 MB · %43", or only the bytes when the size is unknown. */
function appProgressText(p: AppUpdateProgress | null): string {
  if (!p) return "";
  if (!p.total) return formatBytes(p.downloaded);
  return `${formatBytes(p.downloaded)} / ${formatBytes(p.total)} · ${formatPercent(p.downloaded / p.total)}`;
}

function appUpdateClass(view: AppUpdateView, base: string) {
  const tone = view === "current" ? " ok" : view === "check-error" || view === "install-error" ? " bad" : "";
  return `${base}${view === "hidden" ? "" : " show"}${tone}`;
}

function paintAppUpdate() {
  if (appBarEl) paintAppUpdateIn(appBarEl, "bar");
  if (appBlockEl) paintAppUpdateIn(appBlockEl, "settings");
}

function paintAppUpdateIn(host: HTMLElement, where: AppUpdateWhere) {
  const u = state.appUpdate;
  const view = appUpdateView(where);
  const stage = view === "installing" ? installStage() : null;
  // Rebuilt only when the content changes. Progress is written in place,
  // so the spinner keeps turning and no button is replaced mid-click.
  const key = JSON.stringify([
    view,
    stage,
    u.found?.version,
    u.notesOpen,
    u.progress?.total != null,
    view === "confirm" && runningDownloads(),
    u.installError,
    u.checkError,
    where === "settings" && u.checking,
  ]);
  if (host.dataset.key !== key) {
    const active = document.activeElement as HTMLElement | null;
    const hadFocus = !!active && host.contains(active);
    const focused = active?.dataset.action;
    // Animates when it shows up on screen, not on every page render.
    const appeared = host.dataset.view === "hidden" && view !== "hidden";
    host.dataset.key = key;
    host.dataset.view = view;
    host.replaceChildren();
    if (where === "bar") {
      host.className = appUpdateClass(view, "app-update");
      append(host, appUpdateBody(view, where));
      if (appeared) enter(host);
    } else {
      append(host, appVersionBlock(view));
    }
    if (hadFocus) {
      const same = focused ? host.querySelector<HTMLElement>(`[data-action="${focused}"]:not(:disabled)`) : null;
      (same ?? host.querySelector<HTMLElement>("[data-focus]"))?.focus();
    }
  }
  const p = u.progress;
  const bar = host.querySelector<HTMLElement>(".app-update-progress .bar");
  if (bar) bar.style.width = `${Math.round((stage === "downloading" ? (p?.total ? Math.min(1, p.downloaded / p.total) : 0) : 1) * 100)}%`;
  const amount = host.querySelector(".app-update-amount");
  if (amount) amount.textContent = stage === "downloading" ? appProgressText(p) : "";
}

/** The "Ambar" block in Settings: version, the check button and what the check found. */
function renderAppVersionSetting() {
  appBlockEl = h("div", { class: "setting column" });
  paintAppUpdateIn(appBlockEl, "settings");
  return appBlockEl;
}

function appVersionBlock(view: AppUpdateView): Child[] {
  const u = state.appUpdate;
  const check = button(u.checking ? "Denetleniyor…" : "Güncellemeleri denetle", () => checkAppUpdate(true), { class: "btn-ghost btn-sm", icon: "retry" });
  check.dataset.action = "check";
  // Dimmed but still focusable while busy, so keyboard focus survives the
  // rebuilds; checkAppUpdate ignores the extra clicks.
  if (u.checking || u.installing) check.setAttribute("aria-disabled", "true");
  return [
    h("div", { class: "setting-head" }, h("div", {}, h("div", { class: "setting-label" }, "Ambar"), h("div", { class: "setting-hint" }, `Sürüm ${state.appVersion}`)), check),
    view !== "hidden" && h("div", { class: appUpdateClass(view, "app-update in-settings") }, ...appUpdateBody(view, "settings")),
  ];
}

/** What the bar and the Settings block say; only the bar has "Sonra". */
function appUpdateBody(view: AppUpdateView, where: AppUpdateWhere): Child[] {
  const u = state.appUpdate;
  const version = u.found?.version ?? "";
  /** `focus` marks the button that takes over keyboard focus when the content changes. */
  const act = (b: HTMLButtonElement, name: string, focus = false) => {
    b.dataset.action = name;
    if (focus) b.dataset.focus = "";
    return b;
  };
  const later = where === "bar" && act(button("Sonra", dismissAppUpdate, { class: "btn-ghost btn-sm" }), "later");
  // The icon sits with the text, so it stays on the first line when the text wraps.
  const line = (glyph: Node, text: Child[], actions: Child[] = []) =>
    h(
      "div",
      { class: "app-update-row" },
      h("div", { class: "app-update-lead" }, glyph, h("div", { class: "app-update-text" }, ...text)),
      actions.some(Boolean) && h("div", { class: "app-update-actions" }, ...actions),
    );
  switch (view) {
    case "hidden":
      return [];
    case "current":
      return [line(icon("check", 16), [h("span", {}, "Ambar güncel.")])];
    case "check-error":
      return [line(icon("alert", 16), [h("span", { class: "selectable" }, u.checkError)])];
    case "found": {
      const notes = u.found?.notes?.trim() || null;
      const toggle =
        notes &&
        h(
          "button",
          {
            class: "link-btn",
            attrs: { type: "button", "aria-expanded": String(u.notesOpen), "data-action": "notes" },
            on: {
              click: () => {
                u.notesOpen = !u.notesOpen;
                paintAppUpdate();
              },
            },
          },
          "Neler değişti",
          icon(u.notesOpen ? "chevronUp" : "chevronDown", 14),
        );
      return [
        line(icon("upgrade", 16), [h("span", {}, `Ambar ${version} hazır.`), toggle], [act(button("Güncelle", () => installAppUpdate(false), { class: "btn-soft btn-sm" }), "install", true), later]),
        u.notesOpen && notes && h("div", { class: "app-update-notes selectable" }, notes),
      ];
    }
    case "confirm": {
      const n = runningDownloads();
      const text = n > 0 ? `${n} indirme sürüyor. Güncellemek için Ambar yeniden başlayacak, indirmeler yarıda kalacak.` : "Güncellemek için Ambar yeniden başlayacak.";
      const cancel = () => {
        u.confirming = false;
        paintAppUpdate();
      };
      return [
        line(icon("alert", 16), [h("span", {}, text)], [
          act(button("Güncelle ve yeniden başlat", () => installAppUpdate(true), { class: "btn-primary btn-sm" }), "confirm"),
          // The safe answer gets the focus, so Enter twice does not cut downloads short.
          act(button("Vazgeç", cancel, { class: "btn-ghost btn-sm" }), "cancel", true),
        ]),
      ];
    }
    case "installing": {
      const stage = installStage();
      const title = stage === "restarting" ? "Güncelleme kuruldu, Ambar yeniden başlatılıyor…" : stage === "installing" ? `Ambar ${version} kuruluyor…` : `Ambar ${version} indiriliyor`;
      const unknownSize = stage === "downloading" && !u.progress?.total;
      return [
        line(spinner(), [h("span", {}, title)], [h("span", { class: "app-update-amount" })]),
        h("div", { class: `progress app-update-progress${unknownSize ? " indeterminate" : ""}` }, h("div", { class: "bar" })),
        // The NSIS installer shows its own small window while it works.
        h("p", { class: "app-update-hint" }, onWindows() ? "Kurulum penceresi kısa bir süre görünecek." : "Bitince Ambar yeniden açılacak."),
      ];
    }
    case "install-error":
      return [
        line(icon("alert", 16), [h("strong", {}, "Güncelleme kurulamadı."), h("span", { class: "app-update-detail selectable" }, u.installError)], [
          act(button("Tekrar dene", () => installAppUpdate(false), { class: "btn-ghost btn-sm", icon: "retry" }), "retry", true),
          later,
        ]),
      ];
  }
}

// --------------------------------------------------------------- listeners

/** True when an install started before this page loaded (webview reload). */
let adoptedInstall = false;

function onEngineProgress(p: EngineProgress) {
  if (state.install.running || needsOnboarding()) {
    state.install.progress[p.engine] = p;
    if (engineRows.size) patchEngineRow(p.engine);
    const last = ENGINE_ORDER[ENGINE_ORDER.length - 1];
    if (adoptedInstall && (p.phase === "error" || (p.engine === last && p.phase === "done"))) {
      adoptedInstall = false;
      api.enginesState().then((engines) => {
        state.engines = engines;
        state.install.running = false;
        if (p.phase === "error") state.install.error = p.message;
        render();
        if (!needsOnboarding()) {
          focusUrl();
          startUpdateTimer();
          startAppUpdateChecks();
        }
      });
    }
    return;
  }
  if (p.phase === "downloading" || p.phase === "installing") state.updatingEngine = p.engine;
  else if (state.updatingEngine === p.engine) state.updatingEngine = null;
  paintBanner();
  if (p.phase === "error") {
    updateErrors++;
    if (manualCheck) toast(`${ENGINE_INFO[p.engine].name}: ${p.message ?? "güncellenemedi"}`, "error");
  }
}

/**
 * ⌘ or Ctrl on a Mac. On Windows only Ctrl: the Windows key is metaKey there
 * (Win+V is the clipboard history), and AltGr arrives as Ctrl+Alt.
 */
function commandKey(e: KeyboardEvent) {
  return onWindows() ? e.ctrlKey && !e.metaKey && !e.altKey : e.metaKey || e.ctrlKey;
}

/** WebView2's browser shortcuts (reload, print, find, caret browsing); WebKit has none of these. */
function isBrowserShortcut(e: KeyboardEvent) {
  if (["F3", "F5", "F7"].includes(e.key)) return true;
  return commandKey(e) && ["r", "p", "f", "g", "s"].includes(e.key.toLowerCase());
}

/** Whether the click lands on selected text, which is when Chromium shows its Copy menu. */
function onSelection(e: MouseEvent) {
  const sel = getSelection();
  if (!sel || sel.isCollapsed || !sel.toString().trim()) return false;
  for (let i = 0; i < sel.rangeCount; i++) {
    for (const r of sel.getRangeAt(i).getClientRects()) {
      if (e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom) return true;
    }
  }
  return false;
}

function setupGlobalEvents() {
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && state.modal) closeModal();
    if (commandKey(e) && e.key === ",") {
      e.preventDefault();
      if (state.ready && !needsOnboarding()) openModal("settings");
    }
    // ⌘Q comes from the app menu on a Mac; a Windows window has no such menu.
    if (onWindows() && commandKey(e) && e.key.toLowerCase() === "q") {
      e.preventDefault();
      if (!e.repeat) run(() => api.quitApp());
    }
    if (onWindows() && !import.meta.env.DEV && isBrowserShortcut(e)) e.preventDefault();
    const target = e.target as HTMLElement | null;
    const typing = target && (target.tagName === "INPUT" || target.tagName === "SELECT" || target.tagName === "TEXTAREA");
    if (commandKey(e) && e.key.toLowerCase() === "v" && !typing && !state.modal) {
      const input = document.getElementById("url-input") as HTMLInputElement | null;
      input?.focus();
      input?.select();
    }
  });
  window.addEventListener("scroll", () => document.querySelector(".titlebar")?.classList.toggle("scrolled", window.scrollY > 4), { passive: true });
  // A desktop app has no "Reload / Inspect" menu (WebKit's or WebView2's);
  // text fields keep Cut/Copy/Paste. WebView2 doesn't select a word on
  // right-click, and off a selection it shows its page menu (Reload, Save
  // as, Print), so on Windows selectable text gets a menu only over a selection.
  if (!import.meta.env.DEV) {
    document.addEventListener("contextmenu", (e) => {
      const target = e.target as Element;
      if (target.closest("input, textarea")) return;
      if (target.closest(".selectable") && (!onWindows() || onSelection(e))) return;
      e.preventDefault();
    });
  }
  let depth = 0;
  document.addEventListener("dragenter", (e) => {
    e.preventDefault();
    depth++;
    document.body.classList.add("dropping");
  });
  document.addEventListener("dragleave", () => {
    depth = Math.max(0, depth - 1);
    if (depth === 0) document.body.classList.remove("dropping");
  });
  document.addEventListener("dragover", (e) => e.preventDefault());
  document.addEventListener("drop", (e) => {
    e.preventDefault();
    depth = 0;
    document.body.classList.remove("dropping");
    if (!state.ready || needsOnboarding()) return;
    const text = (e.dataTransfer?.getData("text/uri-list") || e.dataTransfer?.getData("text/plain") || "").split(/\r?\n/).find((l) => looksLikeUrl(l));
    if (text) {
      state.url = text.trim();
      fetchInfo(false);
    } else {
      toast("Bırakılan şey bir bağlantı değil.", "error");
    }
  });
}

/** A link handed over from the menu bar (taskbar on Windows) icon: show it as if it had been pasted. */
function openUrl(url: string) {
  if (!state.ready || needsOnboarding()) return;
  state.modal = null;
  state.url = url;
  window.scrollTo(0, 0);
  fetchInfo(false);
}

// --------------------------------------------------------------------- boot

async function boot() {
  render();
  try {
    api = await createApi();
    await api.onJobEvent(onJobEvent);
    await api.onEngineProgress(onEngineProgress);
    // Only the Mac title bar changes in full screen (no traffic lights).
    if (!onWindows()) await api.onFullscreenChange((full) => document.documentElement.classList.toggle("is-fullscreen", full));
    await api.onOpenUrl(openUrl);
    await api.onNotice((n) => toast(n.text, n.kind));
    await api.onAppUpdateProgress(onAppUpdateProgress);
    const b = await api.bootstrap();
    setPlatform(b.platform);
    state.appVersion = b.appVersion;
    state.noticeVersion = b.noticeVersion;
    state.settings = b.settings;
    state.engines = b.engines;
    state.av1Hardware = b.av1Hardware;
    state.homeDir = b.homeDir;
    state.kind = b.settings.defaultKind;
    state.res = b.settings.defaultRes;
    for (const job of b.jobs) state.jobs.set(job.id, job);
    if (b.engineBusy && needsOnboarding()) {
      // The first-run install is still going (the window was reloaded).
      state.install.running = true;
      adoptedInstall = true;
    }
    state.ready = true;
    render();
    setupGlobalEvents();
    if (!needsOnboarding()) {
      focusUrl();
      checkUpdates(false, false);
      startUpdateTimer();
      startAppUpdateChecks();
    }
  } catch (e) {
    state.bootError = `Ambar başlatılamadı: ${errorText(e)}`;
    render();
  }
}

// Hook for the development self-test (src-tauri/selftest.js). Not in release builds.
if (import.meta.env.DEV) {
  Object.assign(window, {
    __kova: {
      state,
      needsOnboarding,
      startInstall,
      fetchInfo,
      startDownload,
      get api() {
        return api;
      },
    },
  });
}

boot();

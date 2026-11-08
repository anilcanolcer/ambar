// Mirrors the Rust types serialized by the backend (camelCase JSON).

export type Kind = "mp3" | "m4a" | "opus" | "flac" | "wav" | "mp4" | "mkv" | "webm";

export const AUDIO_KINDS: readonly Kind[] = ["mp3", "m4a", "opus", "flac", "wav"];

export function isAudioKind(kind: Kind): boolean {
  return AUDIO_KINDS.includes(kind);
}
export type Channel = "nightly" | "stable";

export interface Settings {
  downloadDir: string;
  defaultKind: Kind;
  defaultRes: number;
  maxConcurrent: number;
  ytdlpChannel: Channel;
  acceptedNotice: number;
  squareCover: boolean;
  /** Ask for a folder every time a download is started. */
  askFolder: boolean;
  /** The downloads list on the main screen is folded away. */
  hideDownloads: boolean;
}

export type EngineId = "yt-dlp" | "ffmpeg" | "deno";

export interface EngineStatus {
  id: EngineId;
  name: string;
  installed: boolean;
  version: string | null;
}

export interface EnginesState {
  allInstalled: boolean;
  engines: EngineStatus[];
}

export type EnginePhase = "checking" | "downloading" | "installing" | "done" | "up-to-date" | "error";

export interface EngineProgress {
  engine: EngineId;
  phase: EnginePhase;
  downloaded: number;
  fraction: number | null;
  message: string | null;
  version: string | null;
}

export interface UpdateOutcome {
  engine: EngineId;
  from: string | null;
  to: string;
}

export interface SizeEstimates {
  /** MP3 at the default V0 quality. */
  mp3: number | null;
  m4a: number | null;
  opus: number | null;
  flac: number | null;
  wav: number | null;
  mp4: Record<string, number>;
  mkv: Record<string, number>;
  webm: Record<string, number>;
}

export interface VideoInfo {
  type: "video";
  id: string;
  url: string;
  title: string;
  uploader: string | null;
  duration: number | null;
  thumbnail: string | null;
  site: string | null;
  license: string | null;
  creativeCommons: boolean;
  resolutions: number[];
  hasVideo: boolean;
  hasAudio: boolean;
  /** The site offers VP8/VP9/AV1 video, so WEBM is possible. */
  webm: boolean;
  hasMusicTags: boolean;
  titleHasSeparator: boolean;
  sizes: SizeEstimates;
}

export interface PlaylistEntry {
  id: string;
  url: string;
  title: string;
  duration: number | null;
  thumbnail: string | null;
}

export interface PlaylistInfo {
  type: "playlist";
  id: string;
  url: string;
  title: string;
  uploader: string | null;
  entries: PlaylistEntry[];
  truncated: boolean;
}

export type MediaInfo = VideoInfo | PlaylistInfo;

export type JobStatus = "queued" | "starting" | "downloading" | "processing" | "done" | "error" | "canceled";

export interface JobView {
  id: number;
  url: string;
  title: string;
  thumbnail: string | null;
  kind: Kind;
  res: number | null;
  status: JobStatus;
  progress: number | null;
  speed: number | null;
  eta: number | null;
  stage: string | null;
  filePath: string | null;
  error: string | null;
  attempt: number;
  /** Jobs added together share a batch; newer batches are higher. */
  batch: number;
}

export type JobEvent = { type: "update"; job: JobView } | { type: "removed"; id: number };

export interface DownloadRequest {
  url: string;
  title: string;
  thumbnail: string | null;
  kind: Kind;
  res: number | null;
  splitArtist: boolean;
  /** MP3 bitrate in kbit/s, null for the best variable quality. */
  audioQuality: number | null;
  subtitles: boolean;
  /** Folder picked for this download; null uses the download folder. */
  saveDir?: string | null;
}

export interface Bootstrap {
  appVersion: string;
  noticeVersion: number;
  settings: Settings;
  engines: EnginesState;
  engineBusy: boolean;
  /** This computer plays AV1 natively (on a Mac: M3 and newer), so 2K/4K downloads use AV1. */
  av1Hardware: boolean;
  jobs: JobView[];
  /** The user's home folder, for showing paths the way Finder and Explorer name them. */
  homeDir: string;
  /** Which system Ambar runs on; labels, shortcuts and the title bar follow it. */
  platform: Platform;
}

export type Platform = "macos" | "windows" | "linux";

/** A newer Ambar on GitHub, found by checkAppUpdate. */
export interface AppUpdate {
  version: string;
  /** Release notes, plain text (may be empty). */
  notes: string | null;
  /** RFC 3339 publish date, if the release has one. */
  date: string | null;
}

/** Download progress of an app update, in bytes. */
export interface AppUpdateProgress {
  downloaded: number;
  total: number | null;
}

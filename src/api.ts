import type {
  AppUpdate,
  AppUpdateProgress,
  Bootstrap,
  DownloadRequest,
  EngineProgress,
  EnginesState,
  JobEvent,
  JobView,
  MediaInfo,
  Settings,
  UpdateOutcome,
} from "./types";

export interface Api {
  bootstrap(): Promise<Bootstrap>;
  acceptNotice(): Promise<Settings>;
  installEngines(): Promise<EnginesState>;
  updateEngines(force: boolean): Promise<UpdateOutcome[]>;
  enginesState(): Promise<EnginesState>;
  fetchInfo(url: string, playlist: boolean): Promise<MediaInfo>;
  enqueue(requests: DownloadRequest[]): Promise<JobView[]>;
  cancelJob(id: number): Promise<void>;
  retryJob(id: number): Promise<void>;
  removeJob(id: number): Promise<void>;
  clearFinished(): Promise<void>;
  saveSettings(settings: Settings): Promise<Settings>;
  revealJobFile(id: number): Promise<void>;
  openJobFile(id: number): Promise<void>;
  openDownloadDir(): Promise<void>;
  openLink(url: string): Promise<void>;
  /** Quits Ambar, asking first while downloads are running (Ctrl+Q on Windows). */
  quitApp(): Promise<void>;
  /** Asks GitHub whether a newer Ambar exists; null when this one is current. */
  checkAppUpdate(): Promise<AppUpdate | null>;
  /** Downloads and installs the update found by checkAppUpdate, then restarts Ambar. */
  installAppUpdate(): Promise<void>;
  onAppUpdateProgress(cb: (p: AppUpdateProgress) => void): Promise<() => void>;
  chooseFolder(current: string, title?: string): Promise<string | null>;
  onEngineProgress(cb: (p: EngineProgress) => void): Promise<() => void>;
  onJobEvent(cb: (e: JobEvent) => void): Promise<() => void>;
  /** Called now and whenever the window enters or leaves full screen (used on macOS). */
  onFullscreenChange(cb: (full: boolean) => void): Promise<void>;
  /** Links sent from the menu bar icon ("Ambar'da seç"), or the taskbar icon on Windows. */
  onOpenUrl(cb: (url: string) => void): Promise<() => void>;
  /** Messages from the menu bar / taskbar icon, e.g. why a page could not be read. */
  onNotice(cb: (n: { text: string; kind: "info" | "success" | "error" }) => void): Promise<() => void>;
}

/** Errors from Rust arrive as plain strings; normalise them to Error. */
function asError(e: unknown): Error {
  if (e instanceof Error) return e;
  return new Error(typeof e === "string" ? e : "Beklenmeyen bir hata oluştu.");
}

async function tauriApi(): Promise<Api> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  const { open } = await import("@tauri-apps/plugin-dialog");
  const call = async <T>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
    try {
      return await invoke<T>(cmd, args);
    } catch (e) {
      throw asError(e);
    }
  };
  return {
    bootstrap: () => call("bootstrap"),
    acceptNotice: () => call("accept_notice"),
    installEngines: () => call("install_engines"),
    updateEngines: (force) => call("update_engines", { force }),
    enginesState: () => call("engines_state"),
    fetchInfo: (url, playlist) => call("fetch_info", { url, playlist }),
    enqueue: (requests) => call("enqueue", { requests }),
    cancelJob: (id) => call("cancel_job", { id }),
    retryJob: (id) => call("retry_job", { id }),
    removeJob: (id) => call("remove_job", { id }),
    clearFinished: () => call("clear_finished"),
    saveSettings: (settings) => call("save_settings", { settings }),
    revealJobFile: (id) => call("reveal_job_file", { id }),
    openJobFile: (id) => call("open_job_file", { id }),
    openDownloadDir: () => call("open_download_dir"),
    openLink: (url) => call("open_link", { url }),
    quitApp: () => call("quit_app"),
    checkAppUpdate: () => call("check_app_update"),
    installAppUpdate: () => call("install_app_update"),
    onAppUpdateProgress: (cb) => listen<AppUpdateProgress>("app-update-progress", (e) => cb(e.payload)),
    chooseFolder: async (current, title = "İndirme klasörünü seç") => {
      const picked = await open({ directory: true, multiple: false, defaultPath: current, title });
      return typeof picked === "string" ? picked : null;
    },
    onEngineProgress: (cb) => listen<EngineProgress>("engine-progress", (e) => cb(e.payload)),
    onJobEvent: (cb) => listen<JobEvent>("job-event", (e) => cb(e.payload)),
    onFullscreenChange: async (cb) => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const win = getCurrentWindow();
      const check = () => win.isFullscreen().then(cb, () => {});
      await check();
      await win.onResized(() => void check());
    },
    onOpenUrl: (cb) => listen<string>("open-url", (e) => cb(e.payload)),
    onNotice: (cb) => listen<{ text: string; kind: "info" | "success" | "error" }>("notice", (e) => cb(e.payload)),
  };
}

export const inTauri = "__TAURI_INTERNALS__" in window;

/** Real backend inside the app, a simulated one in a plain browser. */
export async function createApi(): Promise<Api> {
  if (inTauri) return tauriApi();
  const { createMockApi } = await import("./mock");
  return createMockApi();
}

// Simulated backend used when the UI runs in a normal browser (npm run dev).
// It lets the interface be designed and demoed without the desktop app.
// Add "#ready" to the URL to skip the first-run screens, "#ready,windows"
// to see the Windows labels, paths and title bar. "#update" pretends a newer
// Ambar is out ("#update-error" also makes its first install fail), and
// "#check-error" makes the update check itself fail.

import type { Api } from "./api";
import type {
  AppUpdateProgress,
  Bootstrap,
  DownloadRequest,
  EngineId,
  EngineProgress,
  EnginesState,
  JobEvent,
  JobView,
  MediaInfo,
  Settings,
} from "./types";

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

const SAMPLE_VIDEO: MediaInfo = {
  type: "video",
  id: "aqz-KE-bpKQ",
  url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ",
  title: "Big Buck Bunny 60fps 4K - Official Blender Foundation Short Film",
  uploader: "Blender",
  duration: 635,
  thumbnail: "https://i.ytimg.com/vi/aqz-KE-bpKQ/maxresdefault.jpg",
  site: "Youtube",
  license: "Creative Commons Attribution license (reuse allowed)",
  creativeCommons: true,
  resolutions: [2160, 1440, 1080, 720, 480, 360, 240, 144],
  hasVideo: true,
  hasAudio: true,
  webm: true,
  hasMusicTags: false,
  titleHasSeparator: true,
  sizes: {
    mp3: 18_650_000,
    m4a: 10_271_496,
    opus: 10_202_210,
    flac: 107_950_000,
    wav: 121_920_000,
    mp4: { "144": 14_595_389, "240": 19_594_979, "360": 28_565_606, "480": 38_478_640, "720": 160_796_363, "1080": 267_891_149, "1440": 322_825_809, "2160": 722_716_776 },
    mkv: { "144": 14_841_943, "240": 17_778_997, "360": 25_501_018, "480": 36_200_784, "720": 81_486_037, "1080": 134_589_086, "1440": 322_756_523, "2160": 722_647_490 },
    webm: { "144": 15_492_022, "240": 20_400_491, "360": 34_311_746, "480": 43_340_272, "720": 122_878_532, "1080": 178_938_399, "1440": 483_565_914, "2160": 1_372_471_691 },
  },
};

const SAMPLE_PLAYLIST: MediaInfo = {
  type: "playlist",
  id: "PL-blender",
  url: "https://www.youtube.com/playlist?list=PL-blender",
  title: "Blender Open Movies",
  uploader: "Blender",
  truncated: false,
  entries: [
    ["eRsGyueVLvQ", "Sintel - Open Movie by Blender Foundation", 888],
    ["aqz-KE-bpKQ", "Big Buck Bunny 60fps 4K - Official Blender Foundation Short Film", 635],
    ["R6MlUcmOul8", "Tears of Steel - Blender VFX Open Movie", 734],
    ["WhWc3b3KhnY", "Spring - Blender Open Movie", 464],
    ["mN0zPOpADL4", "Caminandes 3: Llamigos", 150],
  ].map(([id, title, duration]) => ({
    id: id as string,
    url: `https://www.youtube.com/watch?v=${id}`,
    title: title as string,
    duration: duration as number,
    thumbnail: `https://i.ytimg.com/vi/${id}/hqdefault.jpg`,
  })),
};

export function createMockApi(): Api {
  const ready = location.hash.includes("ready");
  // "#ready,windows" previews the Windows look in a plain browser.
  const mockWindows = location.hash.includes("windows");
  let settings: Settings = {
    downloadDir: mockWindows ? "C:\\Users\\sen\\Downloads\\Ambar" : "/Users/sen/Downloads/Ambar",
    defaultKind: "mp3",
    defaultRes: 1080,
    maxConcurrent: 2,
    ytdlpChannel: "nightly",
    acceptedNotice: ready ? 1 : 0,
    squareCover: true,
    askFolder: false,
    hideDownloads: false,
  };
  const versions: Record<EngineId, string> = { "yt-dlp": "2026.10.08.232931", ffmpeg: "9.0.2", deno: "2.9.7" };
  let installed = ready;
  const engines = (): EnginesState => ({
    allInstalled: installed,
    engines: (["yt-dlp", "ffmpeg", "deno"] as EngineId[]).map((id) => ({
      id,
      name: id === "yt-dlp" ? "yt-dlp" : id === "ffmpeg" ? "FFmpeg" : "Deno",
      installed,
      version: installed ? versions[id] : null,
    })),
  });

  const engineListeners = new Set<(p: EngineProgress) => void>();
  const jobListeners = new Set<(e: JobEvent) => void>();
  const jobs = new Map<number, JobView>();
  const cancelled = new Set<number>();
  let nextId = 1;
  const emitJob = (job: JobView) => {
    jobs.set(job.id, { ...job });
    jobListeners.forEach((cb) => cb({ type: "update", job: { ...job } }));
  };

  const saveDirs = new Map<number, string>();
  let nextBatch = 1;

  const updateListeners = new Set<(p: AppUpdateProgress) => void>();
  let updateFails = location.hash.includes("update-error");
  // Set just before the simulated restart, read once: the page that comes
  // back is the new version, a further reload is the old one again.
  let updated = false;
  try {
    updated = sessionStorage.getItem("kova-mock-updated") === "1";
    sessionStorage.removeItem("kova-mock-updated");
  } catch {
    // Storage can be blocked; the mock then simply never "updates".
  }

  async function simulate(job: JobView) {
    const failing = job.url.includes("hata");
    const total = ["mp4", "mkv", "webm"].includes(job.kind) ? 2 : 1;
    const step = (patch: Partial<JobView>) => {
      if (cancelled.has(job.id)) throw new Error("cancel");
      Object.assign(job, patch);
      emitJob(job);
    };
    try {
      step({ status: "starting", stage: "Hazırlanıyor", progress: null, attempt: 1 });
      await wait(700);
      for (let i = 0; i <= 100; i += 4) {
        const part = total === 2 && i > 85 ? "Ses indiriliyor" : total === 2 ? "Görüntü indiriliyor" : "İndiriliyor";
        step({ status: "downloading", stage: part, progress: Math.min(0.99, i / 100), speed: 4_200_000 + Math.random() * 1_500_000, eta: Math.round((100 - i) / 8) });
        if (failing && i > 40) {
          step({ status: "error", stage: null, speed: null, eta: null, error: "Video kullanılamıyor. Silinmiş ya da bulunduğun ülkede erişime kapalı olabilir." });
          return;
        }
        await wait(160);
      }
      const steps = [total === 2 ? "Görüntü ve ses birleştiriliyor" : "Ses dönüştürülüyor"];
      if (job.kind !== "wav" && job.kind !== "webm") steps.push("Kapak resmi ekleniyor");
      for (const label of steps) {
        step({ status: "processing", stage: label, progress: null, speed: null, eta: null });
        await wait(900);
      }
      step({ status: "done", stage: null, progress: 1, filePath: `${saveDirs.get(job.id) ?? settings.downloadDir}${mockWindows ? "\\" : "/"}${job.title}.${job.kind}` });
    } catch {
      Object.assign(job, { status: "canceled", stage: null, progress: null, speed: null, eta: null });
      emitJob(job);
    }
  }

  let running = 0;
  const queue: JobView[] = [];
  const pump = () => {
    while (running < settings.maxConcurrent && queue.length) {
      const job = queue.shift()!;
      if (job.status !== "queued") continue;
      running++;
      simulate(job).finally(() => {
        running--;
        pump();
      });
    }
  };

  return {
    async bootstrap(): Promise<Bootstrap> {
      await wait(150);
      return { appVersion: updated ? "0.5.0" : "0.1.0", noticeVersion: 1, settings: { ...settings }, engines: engines(), engineBusy: false, av1Hardware: false, jobs: [], homeDir: mockWindows ? "C:\\Users\\sen" : "/Users/sen", platform: mockWindows ? "windows" : "macos" };
    },
    async acceptNotice() {
      settings = { ...settings, acceptedNotice: 1 };
      return { ...settings };
    },
    async installEngines() {
      const sizes: Record<EngineId, number> = mockWindows
        ? { "yt-dlp": 23_700_000, ffmpeg: 114_800_000, deno: 42_900_000 }
        : { "yt-dlp": 53_900_000, ffmpeg: 56_700_000, deno: 38_400_000 };
      for (const id of ["yt-dlp", "ffmpeg", "deno"] as EngineId[]) {
        const emit = (p: Partial<EngineProgress>) =>
          engineListeners.forEach((cb) => cb({ engine: id, phase: "checking", downloaded: 0, fraction: null, message: null, version: null, ...p }));
        emit({ phase: "checking" });
        await wait(400);
        for (let f = 0; f <= 1.0001; f += 0.05) {
          emit({ phase: "downloading", fraction: f, downloaded: Math.round(sizes[id] * f) });
          await wait(70);
        }
        emit({ phase: "installing" });
        await wait(700);
        emit({ phase: "done", version: versions[id] });
      }
      installed = true;
      return engines();
    },
    async updateEngines() {
      await wait(300);
      return [];
    },
    async enginesState() {
      return engines();
    },
    async fetchInfo(url, playlist) {
      await wait(900);
      if (url.includes("hata")) throw new Error("Video kullanılamıyor. Silinmiş ya da bulunduğun ülkede erişime kapalı olabilir.");
      if (url.includes("playlist") || (playlist && url.includes("list="))) return structuredClone(SAMPLE_PLAYLIST);
      return structuredClone(SAMPLE_VIDEO);
    },
    async enqueue(requests: DownloadRequest[]) {
      const batch = nextBatch++;
      const created = requests.map((r) => {
        if (r.saveDir) saveDirs.set(nextId, r.saveDir);
        const job: JobView = {
          id: nextId++,
          url: r.url,
          title: r.title,
          thumbnail: r.thumbnail,
          kind: r.kind,
          res: ["mp4", "mkv", "webm"].includes(r.kind) ? r.res : null,
          status: "queued",
          progress: null,
          speed: null,
          eta: null,
          stage: "Sırada",
          filePath: null,
          error: null,
          attempt: 0,
          batch,
        };
        emitJob(job);
        queue.push(job);
        return job;
      });
      pump();
      return created.map((j) => ({ ...j }));
    },
    async cancelJob(id) {
      const job = jobs.get(id);
      if (!job) return;
      if (job.status === "queued") {
        const q = queue.find((j) => j.id === id);
        if (q) q.status = "canceled";
        emitJob({ ...job, status: "canceled", stage: null });
      } else cancelled.add(id);
    },
    async retryJob(id) {
      const job = jobs.get(id);
      if (!job) return;
      cancelled.delete(id);
      const fresh: JobView = { ...job, status: "queued", stage: "Sırada", error: null, progress: null };
      emitJob(fresh);
      queue.push(fresh);
      pump();
    },
    async removeJob(id) {
      jobs.delete(id);
      jobListeners.forEach((cb) => cb({ type: "removed", id }));
    },
    async clearFinished() {
      for (const [id, job] of jobs) {
        if (["done", "error", "canceled"].includes(job.status)) {
          jobs.delete(id);
          jobListeners.forEach((cb) => cb({ type: "removed", id }));
        }
      }
    },
    async saveSettings(next) {
      settings = { ...next, acceptedNotice: settings.acceptedNotice };
      pump();
      return { ...settings };
    },
    async revealJobFile() {},
    async openJobFile() {},
    async openDownloadDir() {},
    async openLink(url) {
      window.open(url, "_blank", "noopener");
    },
    async quitApp() {},
    async checkAppUpdate() {
      await wait(700);
      if (location.hash.includes("check-error")) throw new Error("Güncelleme bilgisi alınamadı. İnternet bağlantını kontrol et.");
      if (updated || !location.hash.includes("update")) return null;
      return {
        version: "0.5.0",
        notes: "- Ambar artık kendini tek tıkla güncelliyor.\n- Uzun oynatma listeleri daha hızlı açılıyor.\n- Küçük hata düzeltmeleri.",
        date: "2026-11-02T10:00:00Z",
      };
    },
    // Downloads in uneven chunks like a real connection, then "restarts" by reloading the page.
    async installAppUpdate() {
      const total = mockWindows ? 6_140_000 : 9_830_000;
      await wait(400);
      for (let got = 0; got < total; ) {
        got = Math.min(total, got + Math.round(total / 45 + (Math.random() * total) / 60));
        updateListeners.forEach((cb) => cb({ downloaded: got, total }));
        if (updateFails && got > total * 0.6) {
          updateFails = false;
          throw new Error("İndirilen dosya doğrulanamadı. İnternet bağlantını kontrol edip tekrar dene.");
        }
        await wait(90);
      }
      await wait(1500);
      try {
        sessionStorage.setItem("kova-mock-updated", "1");
      } catch {
        // See above.
      }
      setTimeout(() => location.reload(), 900);
    },
    async onAppUpdateProgress(cb) {
      updateListeners.add(cb);
      return () => updateListeners.delete(cb);
    },
    async chooseFolder() {
      return mockWindows ? "C:\\Users\\sen\\Music\\Ambar" : "/Users/sen/Music/Ambar";
    },
    async onEngineProgress(cb) {
      engineListeners.add(cb);
      return () => engineListeners.delete(cb);
    },
    async onFullscreenChange() {},
    async onOpenUrl() {
      return () => {};
    },
    async onNotice() {
      return () => {};
    },
    async onJobEvent(cb) {
      jobListeners.add(cb);
      return () => jobListeners.delete(cb);
    },
  };
}

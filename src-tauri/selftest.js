// Development-only end-to-end check, injected when KOVA_SELFTEST=1.
// It drives the real UI code through the real Tauri bridge and reports to
// the terminal through the debug_log command.
(async () => {
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const log = (line) => invoke("debug_log", { line: String(line) });
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const done = (s) => ["done", "error", "canceled"].includes(s);
  try {
    for (let i = 0; i < 100 && !(window.__kova && window.__kova.state.ready); i++) await sleep(200);
    const k = window.__kova;
    if (!k) throw new Error("__kova hook missing");
    await log(`boot ready=${k.state.ready} onboarding=${k.needsOnboarding()} av1=${k.state.av1Hardware} restored=${k.state.jobs.size}`);
    await log(`onboarding DOM: ${document.querySelector(".welcome h1")?.textContent ?? "-"}`);
    if (k.needsOnboarding()) {
      const phases = new Set();
      const unlisten = await k.api.onEngineProgress((p) => phases.add(`${p.engine}:${p.phase}`));
      k.state.noticeChecked = true;
      await k.startInstall();
      unlisten();
      await log(`install error=${k.state.install.error} engines=${JSON.stringify(k.state.engines)}`);
      await log(`engine events seen: ${[...phases].join(",")}`);
    }
    await log(`main DOM: ${document.querySelector(".hero h1")?.textContent ?? "-"}`);

    k.state.url = "https://www.youtube.com/watch?v=aqz-KE-bpKQ";
    await k.fetchInfo(false);
    const info = k.state.info;
    await log(`info type=${info?.type} title=${info?.title} cc=${info?.creativeCommons} res=${info?.resolutions} sizes=${JSON.stringify(info?.sizes)} err=${k.state.fetchError}`);
    await log(`card DOM: ${document.querySelector(".media-title")?.textContent ?? "-"} | estimate ${document.querySelector(".estimate")?.textContent ?? "-"}`);

    k.state.kind = "mp3";
    await k.startDownload();
    k.state.kind = "mp4";
    k.state.res = 360;
    await k.startDownload();
    let sawProgress = false;
    for (let i = 0; i < 480; i++) {
      const jobs = [...k.state.jobs.values()];
      if (jobs.some((j) => j.status === "downloading" && (j.progress ?? 0) > 0)) sawProgress = true;
      if (jobs.length >= 2 && jobs.every((j) => done(j.status))) break;
      await sleep(250);
    }
    const jobs = [...k.state.jobs.values()];
    await log(`progress events seen=${sawProgress}`);
    await log(`jobs ${JSON.stringify(jobs.map((j) => ({ kind: j.kind, res: j.res, status: j.status, file: j.filePath, error: j.error })))}`);
    await log(`job DOM rows=${document.querySelectorAll(".job").length} metas=${[...document.querySelectorAll(".job-meta")].map((m) => m.textContent).join(" | ")}`);

    // Playlist listing through the real bridge (first 3 entries of a channel).
    const list = await k.api.fetchInfo("https://www.youtube.com/@BlenderOfficial/videos", true);
    await log(`playlist type=${list.type} entries=${list.entries?.length} first=${list.entries?.[0]?.title}`);

    // Error path.
    try {
      await k.api.fetchInfo("https://www.youtube.com/watch?v=xxxxxxxxxxx", false);
      await log("error path: no error?!");
    } catch (e) {
      await log(`error path message: ${e.message ?? e}`);
    }
    // Rejected input never reaches yt-dlp.
    try {
      await k.api.fetchInfo("--exec=touch /tmp/pwned", false);
    } catch (e) {
      await log(`option injection rejected: ${e.message ?? e}`);
    }
    // An update check must not block downloads or link lookups anymore,
    // and a second check while one runs must say so instead of "up to date".
    k.state.kind = "mp4";
    k.state.res = 144;
    await sleep(800);
    await k.startDownload();
    const first = k.api.updateEngines(true);
    const second = k.api.updateEngines(true).then(() => "no error", (e) => e.message ?? String(e));
    const t0 = performance.now();
    const again = await k.api.fetchInfo("https://www.youtube.com/watch?v=aqz-KE-bpKQ", false);
    const lookupMs = Math.round(performance.now() - t0);
    const outcomes = await first;
    await log(`concurrent: lookup ${again.type} in ${lookupMs} ms during update check; second check -> ${await second}; outcomes=${JSON.stringify(outcomes)}`);
    for (let i = 0; i < 240 && [...k.state.jobs.values()].some((j) => !done(j.status)); i++) await sleep(250);
    await log(`after concurrent: ${JSON.stringify([...k.state.jobs.values()].map((j) => j.status))}`);
    // New formats through the real bridge: MP3 with a fixed bitrate, OPUS,
    // and MKV with embedded subtitles (Sintel, CC BY, has tr/en subtitles).
    const fresh = await k.api.enqueue([
      { url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ", title: "Bunny 128", thumbnail: null, kind: "mp3", res: null, splitArtist: false, audioQuality: 128, subtitles: false },
      { url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ", title: "Bunny opus", thumbnail: null, kind: "opus", res: null, splitArtist: false, audioQuality: null, subtitles: false },
      { url: "https://www.youtube.com/watch?v=eRsGyueVLvQ", title: "Sintel", thumbnail: null, kind: "mkv", res: 144, splitArtist: false, audioQuality: null, subtitles: true },
    ]);
    const ids = new Set(fresh.map((j) => j.id));
    for (let i = 0; i < 480 && [...k.state.jobs.values()].filter((j) => ids.has(j.id)).some((j) => !done(j.status)); i++) await sleep(250);
    await log(`formats ${JSON.stringify([...k.state.jobs.values()].filter((j) => ids.has(j.id)).map((j) => ({ kind: j.kind, res: j.res, status: j.status, file: j.filePath?.split("/").pop(), error: j.error })))}`);
    await log(`badges ${[...document.querySelectorAll(".kind-badge")].map((b) => b.textContent).join(", ")}`);
    await log("SELFTEST DONE");
  } catch (e) {
    await log(`SELFTEST ERROR ${e && e.stack ? e.stack : e}`);
  }
})();

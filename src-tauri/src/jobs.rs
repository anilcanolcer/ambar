//! Download queue: runs yt-dlp for each job, at most `max_concurrent` at a
//! time, and streams progress to the UI through `JobEnv::emit`.

use crate::error::{msg, AppResult};
use crate::proc::ProcessTree;
use crate::settings::Kind;
use crate::ytdlp::{self, DownloadRequest, Line, ProgressTracker, Runner};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Runs in a row that brought no new bytes before a job gives up. A run
/// that moved the download forward does not count.
const MAX_ATTEMPTS: u32 = 3;
/// Upper bound on runs per job, however far each of them got.
const MAX_RUNS: u32 = 8;
/// Pause before the next run, by how many runs in a row got nothing done.
const RETRY_WAIT: [u64; 2] = [10, 30];
const EMIT_INTERVAL: Duration = Duration::from_millis(150);
/// How often a running job is looked at between lines.
const TICK: Duration = Duration::from_secs(1);
/// No progress for this long replaces the last speed with "İnternet bağlantısı bekleniyor".
const QUIET_HINT: Duration = Duration::from_secs(8);
/// yt-dlp printed nothing and its files did not grow for this long, so it
/// is stuck and is started again. While it reads the page and solves the
/// site's challenges it can be quiet for a while. While downloading, its
/// read size follows the speed, so it reports progress about every second
/// and each retry, and one retry (read timeout, pause, connect timeout)
/// stays well under this.
/// FFmpeg's work after the download prints nothing and is never cut off.
const STALL_STARTING: Duration = Duration::from_secs(300);
const STALL_DOWNLOADING: Duration = Duration::from_secs(180);
/// After yt-dlp exits, how long a program it started may keep its output
/// open before Ambar stops waiting for the rest.
const DRAIN: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    Queued,
    Starting,
    Downloading,
    Processing,
    Done,
    Error,
    Canceled,
}

impl JobStatus {
    pub fn is_active(self) -> bool {
        matches!(self, JobStatus::Starting | JobStatus::Downloading | JobStatus::Processing)
    }

    pub fn is_finished(self) -> bool {
        matches!(self, JobStatus::Done | JobStatus::Error | JobStatus::Canceled)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: u64,
    pub url: String,
    pub title: String,
    pub thumbnail: Option<String>,
    pub kind: Kind,
    pub res: Option<u32>,
    pub status: JobStatus,
    /// 0.0 to 1.0, or None while the stage has no measurable progress.
    pub progress: Option<f64>,
    pub speed: Option<f64>,
    pub eta: Option<u64>,
    pub stage: Option<String>,
    pub file_path: Option<String>,
    pub error: Option<String>,
    pub attempt: u32,
    /// Jobs added together (one playlist) share a batch; newer batches are
    /// higher. The window lists newer batches first, each in id order.
    pub batch: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum JobEvent {
    Update { job: JobView },
    Removed { id: u64 },
}

pub struct JobOptions {
    pub download_dir: PathBuf,
    pub square_cover: bool,
    pub max_concurrent: usize,
    /// Pick AV1 above 1080p (only when this Mac can play it natively).
    pub prefer_av1: bool,
}

/// Everything the queue needs from the outside world, passed in as
/// closures so the queue can be tested without a Tauri window.
pub struct JobEnv {
    pub emit: Box<dyn Fn(JobEvent) + Send + Sync>,
    pub runner: Box<dyn Fn() -> Option<Runner> + Send + Sync>,
    pub options: Box<dyn Fn() -> JobOptions + Send + Sync>,
    pub temp_root: PathBuf,
}

struct Job {
    view: JobView,
    req: DownloadRequest,
    /// The folder came from the settings, not from a per-download choice;
    /// "Tekrar dene" then uses the folder that is set at that moment.
    dir_from_settings: bool,
    /// The running yt-dlp and everything it started.
    tree: Option<Arc<ProcessTree>>,
    cancel_requested: bool,
    last_emit: Instant,
    /// A change the throttle held back; `flush` sends it.
    dirty: bool,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    next_batch: u64,
    jobs: Vec<Job>,
    running: usize,
    /// Ambar is quitting: start nothing new, stop anything just started.
    closing: bool,
}

/// Called with a job the moment it is done, under the queue's lock.
type DoneHook = Box<dyn Fn(&JobView) + Send + Sync>;

pub struct JobManager {
    env: JobEnv,
    inner: Mutex<Inner>,
    on_done: std::sync::OnceLock<DoneHook>,
}

enum Outcome {
    Done(String),
    Canceled,
    Failed(String),
}

/// Serialises the final "pick a free name and move" step so two jobs that
/// finish at the same moment cannot claim the same file name.
static MOVE_LOCK: Mutex<()> = Mutex::new(());

/// Hidden part files currently being copied to another disk, so they can be
/// removed if the app quits mid-copy.
static PARTS_IN_FLIGHT: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Deletes half-copied part files: the ones still being written when the
/// app quits, and (at start-up) leftovers in the download folder.
pub fn remove_part_files(download_dir: Option<&Path>) {
    let parts: Vec<PathBuf> = std::mem::take(&mut *PARTS_IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner()));
    for part in parts {
        let _ = std::fs::remove_file(part);
    }
    if let Some(dir) = download_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && name.ends_with(".kova-part") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// "Song.mp3" -> "Song (2).mp3" -> "Song (3).mp3" ... like a browser does.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem} ({}){ext}", crate::util::now_secs()))
}

/// ".Song.mp3.job-3.kova-part" -> "job-3"
fn part_tag(file: &str) -> Option<&str> {
    let tag = file.strip_suffix(".kova-part")?.rsplit_once('.')?.1;
    tag.starts_with("job-").then_some(tag)
}

/// The rename failed only because source and target are on different
/// disks. ErrorKind::CrossesDevices needs a newer Rust than this crate's
/// minimum, so the raw code is compared: errno on macOS, a Win32 error code
/// on Windows.
fn crosses_disks(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    let code = libc::EXDEV;
    #[cfg(windows)]
    let code = windows_sys::Win32::Foundation::ERROR_NOT_SAME_DEVICE as i32;
    e.raw_os_error() == Some(code)
}

/// Moves a finished file from the job's private folder into the download
/// folder without ever overwriting an existing file.
fn finalize_file(src: &Path, job_out: &Path, dest_dir: &Path) -> std::io::Result<PathBuf> {
    // Must be a direct child of the job folder; a lexical prefix check
    // would let "<job_out>/../x" through.
    if src.parent() != Some(job_out) || !src.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "indirilen dosya bulunamadı"));
    }
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "dosya adı yok"))?;
    std::fs::create_dir_all(dest_dir)?;
    // Same disk: a rename is instant. The name is picked and claimed under
    // the lock so two jobs can never take the same one.
    let renamed = {
        let _guard = MOVE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let target = unique_path(dest_dir, &name);
        match crate::util::rename_retrying(src, &target) {
            Ok(()) => Some(target),
            Err(e) if !crosses_disks(&e) => return Err(e),
            Err(_) => None,
        }
    };
    if let Some(target) = renamed {
        // Otherwise it keeps the work folder's "only this user" permissions.
        #[cfg(windows)]
        crate::util::inherit_folder_permissions(&target);
        return Ok(target);
    }
    // Different disk (external drive, network share): copy to a hidden
    // file first so a failed copy never leaves a broken file under the real
    // name, then rename it into place. (Windows does not hide dot files, so
    // the part file shows up there while it is being copied.)
    // The job folder name ("job-12") makes the part unique per job, so two
    // jobs producing the same file name never write into one part file.
    let tag = job_out
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let part = dest_dir.join(format!(".{name}.{tag}.kova-part"));
    // Leftovers from a crash or power cut while copying here earlier; start-up
    // only cleans the default folder, and this one may have been picked once.
    // Parts are matched by their ASCII "job-N" tag, not the full name: HFS+
    // disks hand names back decomposed (e.g. "ş" as "s" + accent).
    {
        let in_flight = PARTS_IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner());
        let busy: Vec<String> = in_flight.iter().filter_map(|p| part_tag(&p.file_name()?.to_string_lossy()).map(str::to_string)).collect();
        if let Ok(entries) = std::fs::read_dir(dest_dir) {
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if file.starts_with('.') && part_tag(&file).is_some_and(|t| !busy.iter().any(|b| b == t)) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
    PARTS_IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner()).push(part.clone());
    let forget = |part: &Path| PARTS_IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner()).retain(|p| p != part);
    if let Err(e) = std::fs::copy(src, &part) {
        let _ = std::fs::remove_file(&part);
        forget(&part);
        return Err(e);
    }
    let target = {
        let _guard = MOVE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let target = unique_path(dest_dir, &name);
        let renamed = crate::util::rename_retrying(&part, &target);
        forget(&part);
        if let Err(e) = renamed {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        target
    };
    let _ = std::fs::remove_file(src);
    Ok(target)
}


impl JobManager {
    pub fn new(env: JobEnv) -> Self {
        JobManager { env, inner: Mutex::new(Inner { next_id: 1, next_batch: 1, ..Inner::default() }), on_done: std::sync::OnceLock::new() }
    }

    /// Records finished downloads (the history). It runs under the same lock
    /// as clear_finished, so a clear either sees a job as done or not at all.
    pub fn set_on_done(&self, hook: impl Fn(&JobView) + Send + Sync + 'static) {
        let _ = self.on_done.set(Box::new(hook));
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn emit(&self, event: JobEvent) {
        (self.env.emit)(event);
    }

    fn temp_dir(&self, id: u64) -> PathBuf {
        self.env.temp_root.join(format!("job-{id}"))
    }

    pub fn list(&self) -> Vec<JobView> {
        self.lock().jobs.iter().map(|j| j.view.clone()).collect()
    }

    /// Path of a finished download, if the file is still there.
    pub fn file_of(&self, id: u64) -> Option<PathBuf> {
        let inner = self.lock();
        let job = inner.jobs.iter().find(|j| j.view.id == id)?;
        if job.view.status != JobStatus::Done {
            return None;
        }
        job.view.file_path.as_ref().map(PathBuf::from).filter(|p| p.exists())
    }

    /// Applies `f` to a job and emits the new state. Progress-only changes
    /// are throttled; status changes always go out.
    fn update(&self, id: u64, force: bool, f: impl FnOnce(&mut Job)) {
        let view = {
            let mut inner = self.lock();
            let Some(job) = inner.jobs.iter_mut().find(|j| j.view.id == id) else { return };
            let before = job.view.status;
            f(job);
            if before != JobStatus::Done && job.view.status == JobStatus::Done {
                if let Some(hook) = self.on_done.get() {
                    hook(&job.view);
                }
            }
            if force || job.view.status != before || job.last_emit.elapsed() >= EMIT_INTERVAL {
                job.last_emit = Instant::now();
                job.dirty = false;
                Some(job.view.clone())
            } else {
                job.dirty = true;
                None
            }
        };
        if let Some(job) = view {
            self.emit(JobEvent::Update { job });
        }
    }

    /// Sends the progress change `update` held back, so a download that goes
    /// quiet does not show an older percentage than it reached.
    fn flush(&self, id: u64) {
        let view = {
            let mut inner = self.lock();
            let Some(job) = inner.jobs.iter_mut().find(|j| j.view.id == id) else { return };
            if !job.dirty || job.last_emit.elapsed() < EMIT_INTERVAL {
                return;
            }
            job.dirty = false;
            job.last_emit = Instant::now();
            job.view.clone()
        };
        self.emit(JobEvent::Update { job: view });
    }

    fn status_of(&self, id: u64) -> Option<JobStatus> {
        self.lock().jobs.iter().find(|j| j.view.id == id).map(|j| j.view.status)
    }

    pub fn enqueue(self: &Arc<Self>, requests: Vec<DownloadRequest>) -> AppResult<Vec<JobView>> {
        if requests.is_empty() {
            return Err(msg("İndirilecek bir şey seçilmedi."));
        }
        if requests.len() > ytdlp::PLAYLIST_LIMIT {
            return Err(msg("Tek seferde en fazla 1000 öğe eklenebilir."));
        }
        // The folder is fixed now, so changing it later doesn't split a
        // playlist that is still downloading across two folders.
        let default_dir = (self.env.options)().download_dir.to_string_lossy().into_owned();
        let mut cleaned = Vec::with_capacity(requests.len());
        for mut req in requests {
            req.url = ytdlp::validate_url(&req.url)?;
            req.title = req.title.trim().chars().take(300).collect();
            if req.title.is_empty() {
                req.title = req.url.clone();
            }
            req.thumbnail = req.thumbnail.filter(|t| t.starts_with("https://") && t.len() < 4096);
            if req.kind.is_audio() {
                req.res = None;
                req.subtitles = false;
            } else {
                req.res = Some(req.res.unwrap_or(1080).clamp(144, 4320));
                req.audio_quality = None;
                req.split_artist = false;
            }
            if req.kind != Kind::Mp3 {
                req.audio_quality = None;
            }
            req.save_dir = match req.save_dir.take().map(|d| d.trim().to_string()).filter(|d| !d.is_empty()) {
                Some(d) if Path::new(&d).is_absolute() && d.len() < 4096 => Some(d),
                Some(_) => return Err(msg("Kaydedilecek klasör geçerli değil.")),
                None => None,
            };
            let dir_from_settings = req.save_dir.is_none();
            if dir_from_settings {
                req.save_dir = Some(default_dir.clone());
            }
            cleaned.push((req, dir_from_settings));
        }
        let views: Vec<JobView> = {
            let mut inner = self.lock();
            let batch = inner.next_batch;
            inner.next_batch += 1;
            cleaned
                .into_iter()
                .map(|(req, dir_from_settings)| {
                    let id = inner.next_id;
                    inner.next_id += 1;
                    let view = JobView {
                        id,
                        url: req.url.clone(),
                        title: req.title.clone(),
                        thumbnail: req.thumbnail.clone(),
                        kind: req.kind,
                        res: req.res,
                        status: JobStatus::Queued,
                        progress: None,
                        speed: None,
                        eta: None,
                        stage: Some("Sırada".into()),
                        file_path: None,
                        error: None,
                        attempt: 0,
                        batch,
                    };
                    inner.jobs.push(Job { view: view.clone(), req, dir_from_settings, tree: None, cancel_requested: false, last_emit: Instant::now(), dirty: false });
                    view
                })
                .collect()
        };
        for job in &views {
            self.emit(JobEvent::Update { job: job.clone() });
        }
        self.kick();
        Ok(views)
    }

    /// Starts queued jobs while there are free slots.
    pub fn kick(self: &Arc<Self>) {
        let max = (self.env.options)().max_concurrent.max(1);
        loop {
            let started = {
                let mut inner = self.lock();
                if inner.running >= max || inner.closing {
                    None
                } else if let Some(job) = inner.jobs.iter_mut().find(|j| j.view.status == JobStatus::Queued) {
                    job.view.status = JobStatus::Starting;
                    job.view.stage = Some("Hazırlanıyor".into());
                    job.cancel_requested = false;
                    job.last_emit = Instant::now();
                    let view = job.view.clone();
                    inner.running += 1;
                    Some(view)
                } else {
                    None
                }
            };
            let Some(view) = started else { break };
            let id = view.id;
            self.emit(JobEvent::Update { job: view });
            let me = Arc::clone(self);
            tauri::async_runtime::spawn(async move { me.run(id).await });
        }
        // run() calls kick() when a job ends, so this also sees the last one go.
        crate::platform::keep_awake(self.lock().running > 0);
    }

    pub fn cancel(self: &Arc<Self>, id: u64) {
        let tree = {
            let mut inner = self.lock();
            let Some(job) = inner.jobs.iter_mut().find(|j| j.view.id == id) else { return };
            match job.view.status {
                JobStatus::Queued => {
                    job.view.status = JobStatus::Canceled;
                    job.view.stage = None;
                    None
                }
                s if s.is_active() => {
                    job.cancel_requested = true;
                    job.view.stage = Some("İptal ediliyor".into());
                    job.tree.clone()
                }
                _ => return,
            }
        };
        self.update(id, true, |_| {});
        if let Some(tree) = tree {
            tree.stop();
            let me = Arc::clone(self);
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(4)).await;
                let still_running =
                    me.lock().jobs.iter().any(|j| j.view.id == id && j.tree.as_ref().is_some_and(|t| Arc::ptr_eq(t, &tree)));
                if still_running {
                    tree.kill();
                }
            });
        }
    }

    pub fn retry(self: &Arc<Self>, id: u64) {
        let default_dir = (self.env.options)().download_dir.to_string_lossy().into_owned();
        let reset = {
            let mut inner = self.lock();
            match inner.jobs.iter_mut().find(|j| j.view.id == id) {
                Some(job) if matches!(job.view.status, JobStatus::Error | JobStatus::Canceled) => {
                    if job.dir_from_settings {
                        job.req.save_dir = Some(default_dir.clone());
                    }
                    job.view.status = JobStatus::Queued;
                    job.view.stage = Some("Sırada".into());
                    job.view.error = None;
                    job.view.progress = None;
                    job.view.attempt = 0;
                    true
                }
                _ => false,
            }
        };
        if reset {
            self.update(id, true, |_| {});
            self.kick();
        }
    }

    /// Removes finished or still-queued jobs from the list.
    pub fn remove(&self, id: u64) {
        let removed = {
            let mut inner = self.lock();
            let before = inner.jobs.len();
            inner.jobs.retain(|j| !(j.view.id == id && (j.view.status.is_finished() || j.view.status == JobStatus::Queued)));
            inner.jobs.len() != before
        };
        if removed {
            crate::util::remove_dir_quiet(&self.temp_dir(id));
            self.emit(JobEvent::Removed { id });
        }
    }

    /// Removes every finished job and returns them.
    pub fn clear_finished(&self) -> Vec<JobView> {
        let removed: Vec<JobView> = {
            let mut inner = self.lock();
            let removed = inner.jobs.iter().filter(|j| j.view.status.is_finished()).map(|j| j.view.clone()).collect();
            inner.jobs.retain(|j| !j.view.status.is_finished());
            removed
        };
        for job in &removed {
            crate::util::remove_dir_quiet(&self.temp_dir(job.id));
            self.emit(JobEvent::Removed { id: job.id });
        }
        removed
    }

    /// Puts downloads finished in earlier sessions back in the list. Ids
    /// follow the finish order, like jobs added later, so newer means higher.
    pub fn restore(&self, entries: Vec<crate::history::Entry>) {
        let mut inner = self.lock();
        for e in entries {
            let id = inner.next_id;
            inner.next_id += 1;
            let batch = inner.next_batch;
            inner.next_batch += 1;
            let save_dir = Path::new(&e.file_path).parent().map(|p| p.to_string_lossy().into_owned());
            let req = DownloadRequest {
                url: e.url.clone(),
                title: e.title.clone(),
                thumbnail: e.thumbnail.clone(),
                kind: e.kind,
                res: e.res,
                split_artist: false,
                audio_quality: None,
                subtitles: false,
                save_dir,
            };
            let view = JobView {
                id,
                url: e.url,
                title: e.title,
                thumbnail: e.thumbnail,
                kind: e.kind,
                res: e.res,
                status: JobStatus::Done,
                progress: Some(1.0),
                speed: None,
                eta: None,
                stage: None,
                file_path: Some(e.file_path),
                error: None,
                attempt: 0,
                batch,
            };
            inner.jobs.push(Job { view, req, dir_from_settings: false, tree: None, cancel_requested: false, last_emit: Instant::now(), dirty: false });
        }
    }

    /// Called when the app quits so no yt-dlp or ffmpeg is left running.
    pub fn kill_all(&self) {
        let trees: Vec<Arc<ProcessTree>> = {
            let mut inner = self.lock();
            inner.closing = true;
            inner.jobs.iter().filter_map(|j| j.tree.clone()).collect()
        };
        for tree in trees {
            tree.kill();
        }
    }

    async fn run(self: Arc<Self>, id: u64) {
        let outcome = self.run_job(id).await;
        {
            let mut inner = self.lock();
            inner.running = inner.running.saturating_sub(1);
            if let Some(job) = inner.jobs.iter_mut().find(|j| j.view.id == id) {
                job.tree = None;
            }
        }
        // Partial files are kept after an error so "try again" can resume.
        if !matches!(outcome, Outcome::Failed(_)) {
            crate::util::remove_dir_quiet(&self.temp_dir(id));
        }
        self.update(id, true, |job| {
            let v = &mut job.view;
            v.speed = None;
            v.eta = None;
            v.stage = None;
            match outcome {
                Outcome::Done(path) => {
                    v.status = JobStatus::Done;
                    v.progress = Some(1.0);
                    v.file_path = Some(path);
                }
                Outcome::Canceled => {
                    v.status = JobStatus::Canceled;
                    v.progress = None;
                }
                Outcome::Failed(error) => {
                    eprintln!("[kova] download {id} failed: {error}");
                    v.status = JobStatus::Error;
                    v.error = Some(error);
                }
            }
        });
        self.kick();
    }

    fn is_cancel_requested(&self, id: u64) -> bool {
        self.lock().jobs.iter().any(|j| j.view.id == id && j.cancel_requested)
    }

    async fn run_job(&self, id: u64) -> Outcome {
        let Some(req) = self.lock().jobs.iter().find(|j| j.view.id == id).map(|j| j.req.clone()) else {
            return Outcome::Canceled;
        };
        // The runner is a snapshot of the current engine versions. An update
        // installs into a new folder, so these paths stay valid for the
        // whole download.
        let Some(runner) = (self.env.runner)() else {
            return Outcome::Failed(crate::commands::MISSING_ENGINES.into());
        };
        let options = (self.env.options)();
        let dest_dir = req.save_dir.as_ref().map(PathBuf::from).unwrap_or_else(|| options.download_dir.clone());
        if let Err(e) = std::fs::create_dir_all(&dest_dir) {
            return Outcome::Failed(format!("İndirme klasörü oluşturulamadı. {}", crate::util::dir_error(&dest_dir, &e)));
        }
        // yt-dlp works entirely inside the job's private folder; the result
        // is moved into the download folder at the end (see finalize_file).
        let temp = self.temp_dir(id);
        let job_out = temp.join("out");
        let job_parts = temp.join("parts");
        if let Err(e) = std::fs::create_dir_all(&job_out).and_then(|_| std::fs::create_dir_all(&job_parts)) {
            return Outcome::Failed(format!("Geçici klasör oluşturulamadı. {}", crate::util::io_text(&e)));
        }

        let mut last_error;
        let mut stalled;
        // The furthest each stream got over all runs of this job, to tell a
        // run that moved the download forward from one that did nothing.
        let mut best: HashMap<String, u64> = HashMap::new();
        let mut fruitless = 0;
        let mut attempt = 0;
        loop {
            attempt += 1;
            if self.is_cancel_requested(id) {
                return Outcome::Canceled;
            }
            self.update(id, true, |job| {
                job.view.attempt = attempt;
                job.view.status = JobStatus::Starting;
                job.view.progress = None;
                job.view.speed = None;
                job.view.eta = None;
                job.view.stage = Some(if attempt > 1 { "Yeniden deneniyor" } else { "Hazırlanıyor" }.into());
            });
            log(id, &format!("run {attempt} started"));

            let mut cmd = runner.command();
            cmd.args(ytdlp::download_args(&req, &job_out, &job_parts, options.square_cover, options.prefer_av1));
            let (mut child, tree) = match ProcessTree::spawn(&mut cmd) {
                Ok((child, tree)) => (child, tree.map(Arc::new)),
                Err(e) => return Outcome::Failed(format!("yt-dlp başlatılamadı. {}", crate::util::io_text(&e))),
            };
            let cancel_now = {
                let mut inner = self.lock();
                let closing = inner.closing;
                let job = inner.jobs.iter_mut().find(|j| j.view.id == id);
                match job {
                    Some(job) => {
                        job.tree = tree.clone();
                        job.cancel_requested || closing
                    }
                    None => true,
                }
            };
            if cancel_now {
                if let Some(tree) = &tree {
                    tree.stop();
                }
            }

            let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
                if let Some(tree) = &tree {
                    tree.kill();
                }
                return Outcome::Failed("yt-dlp çıktısı okunamadı.".into());
            };
            // next_segment() is cancel-safe, so a line split across two
            // pipe reads is never lost when the other stream wins select!.
            let mut out = BufReader::new(stdout).split(b'\n');
            let mut err = BufReader::new(stderr).split(b'\n');
            let (mut out_open, mut err_open) = (true, true);
            let mut tracker = ProgressTracker::default();
            let mut files: Vec<String> = Vec::new();
            let mut ignored: Vec<String> = Vec::new();
            let mut tail: VecDeque<String> = VecDeque::new();
            let mut silence = Silence::new([job_out.clone(), job_parts.clone()]);
            let mut last_progress = Instant::now();
            let mut hinted = false;
            let mut advanced = false;
            stalled = false;
            let mut exited = None;
            let mut drain_until: Option<Instant> = None;
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            while out_open || err_open {
                let heard = tokio::select! {
                    segment = out.next_segment(), if out_open => match segment {
                        Ok(Some(bytes)) => self.handle_line(id, &String::from_utf8_lossy(&bytes), &mut tracker, &mut files),
                        _ => {
                            out_open = false;
                            continue;
                        }
                    },
                    segment = err.next_segment(), if err_open => match segment {
                        Ok(Some(bytes)) => {
                            let l = String::from_utf8_lossy(&bytes).into_owned();
                            if !l.starts_with(ytdlp::MARK_POST) {
                                if tail.len() >= 40 {
                                    tail.pop_front();
                                }
                                tail.push_back(l.trim_end().to_string());
                            }
                            // "--print" output only ever goes to stdout; a file
                            // marker on stderr is never trusted.
                            self.handle_line(id, &l, &mut tracker, &mut ignored)
                        }
                        _ => {
                            err_open = false;
                            continue;
                        }
                    },
                    // yt-dlp ended, but FFmpeg or Deno may still hold the
                    // pipes; the rest of the output gets a moment to arrive.
                    status = child.wait(), if exited.is_none() => {
                        exited = Some(status);
                        drain_until = Some(Instant::now() + DRAIN);
                        continue;
                    }
                    _ = tick.tick() => {
                        self.flush(id);
                        if drain_until.is_some_and(|until| Instant::now() >= until) {
                            if let Some(tree) = &tree {
                                tree.kill();
                            }
                            break;
                        }
                        if self.is_cancel_requested(id) || stalled {
                            continue;
                        }
                        let status = self.status_of(id);
                        if status == Some(JobStatus::Downloading) && !hinted && last_progress.elapsed() >= QUIET_HINT {
                            hinted = true;
                            self.update(id, true, |job| {
                                if job.cancel_requested || job.view.status != JobStatus::Downloading {
                                    return;
                                }
                                let v = &mut job.view;
                                v.speed = None;
                                v.eta = None;
                                v.stage = Some("İnternet bağlantısı bekleniyor".into());
                            });
                        }
                        let limit = match status {
                            Some(JobStatus::Starting) => STALL_STARTING,
                            Some(JobStatus::Downloading) => STALL_DOWNLOADING,
                            _ => continue,
                        };
                        let quiet = silence.quiet_for();
                        if quiet >= limit {
                            stalled = true;
                            log(id, &format!("no output and no new bytes for {} s, stopping yt-dlp", quiet.as_secs()));
                            if let Some(tree) = &tree {
                                tree.kill();
                            }
                        }
                        continue;
                    }
                };
                silence.heard();
                match heard {
                    Heard::Progress { format_id, downloaded } => {
                        last_progress = Instant::now();
                        hinted = false;
                        let furthest = best.entry(format_id).or_insert(0);
                        if downloaded > *furthest {
                            *furthest = downloaded;
                            advanced = true;
                        }
                    }
                    Heard::Retry(line) => {
                        // The retry notice stays up until progress resumes.
                        hinted = true;
                        log(id, &line);
                    }
                    Heard::Other => {}
                }
            }
            let status = match exited {
                Some(status) => status,
                None => child.wait().await,
            };
            if let Some(job) = self.lock().jobs.iter_mut().find(|j| j.view.id == id) {
                job.tree = None;
            }
            if self.is_cancel_requested(id) {
                return Outcome::Canceled;
            }
            let stderr_text = tail.into_iter().collect::<Vec<_>>().join("\n");
            if matches!(status, Ok(s) if s.success()) {
                if files.is_empty() {
                    return Outcome::Failed("İndirme bitti ama dosya bulunamadı.".into());
                }
                // Normally one file; if a link unexpectedly produced several,
                // every one of them is kept rather than only the last. A copy
                // to another disk can take a while, so it runs off the async
                // workers.
                let (out_dir, dest) = (job_out.clone(), dest_dir.clone());
                let moved = tokio::task::spawn_blocking(move || {
                    files.iter().map(|p| finalize_file(Path::new(p), &out_dir, &dest)).collect::<Result<Vec<_>, _>>()
                })
                .await;
                return match moved {
                    Ok(Ok(targets)) => match targets.into_iter().next() {
                        Some(first) => Outcome::Done(first.to_string_lossy().into_owned()),
                        None => Outcome::Failed("İndirme bitti ama dosya bulunamadı.".into()),
                    },
                    Ok(Err(e)) => {
                        eprintln!("[kova] download {id}: moving the file failed: {e}");
                        Outcome::Failed(format!("Dosya indirme klasörüne taşınamadı. {}", crate::util::io_text(&e)))
                    }
                    Err(_) => Outcome::Failed("Dosya indirme klasörüne taşınamadı.".into()),
                };
            }
            last_error = stderr_text;
            // The last lines yt-dlp wrote, for kova.log (the window shows them in plain Turkish).
            let last_lines: Vec<&str> = last_error.lines().rev().take(12).collect();
            let exit = status.map_or_else(|e| e.to_string(), |s| s.to_string());
            log(id, &format!("run {attempt} failed ({exit}), yt-dlp said:\n{}", last_lines.into_iter().rev().collect::<Vec<_>>().join("\n")));
            fruitless = if advanced { 0 } else { fruitless + 1 };
            if !(stalled || ytdlp::is_retryable(&last_error)) || fruitless >= MAX_ATTEMPTS || attempt >= MAX_RUNS {
                break;
            }
            // A run that got further is resumed at once; one that got
            // nothing waits longer each time for the network to come back.
            let wait = if advanced { 2 } else { RETRY_WAIT[(fruitless as usize - 1).min(RETRY_WAIT.len() - 1)] };
            let network = stalled || ytdlp::is_network_error(&last_error);
            // The status stays as it was, so the bar, the menu bar bucket and
            // the Dock keep the percentage until the next run starts.
            self.update(id, true, |job| {
                if job.cancel_requested {
                    return;
                }
                let v = &mut job.view;
                v.speed = None;
                v.eta = None;
                v.stage = Some(if network { "İnternet bağlantısı bekleniyor" } else { "Birazdan yeniden denenecek" }.into());
            });
            if !self.pause(id, Duration::from_secs(wait)).await {
                return Outcome::Canceled;
            }
        }
        if stalled {
            return Outcome::Failed("İndirme ilerlemiyor; bağlantı çok yavaş ya da kopmuş olabilir. Tekrar dene, kaldığı yerden devam eder.".into());
        }
        if req.kind == Kind::Webm && last_error.to_lowercase().contains("requested format is not available") {
            return Outcome::Failed("Bu site WEBM sunmuyor. MKV ya da MP4 seç.".into());
        }
        Outcome::Failed(ytdlp::humanize_error(&last_error, true))
    }

    /// Waits before the next run. False when the job was canceled (or Ambar
    /// is quitting) in the meantime.
    async fn pause(&self, id: u64, wait: Duration) -> bool {
        let until = Instant::now() + wait;
        loop {
            if self.is_cancel_requested(id) || self.lock().closing {
                return false;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            tokio::time::sleep(left.min(Duration::from_millis(250))).await;
        }
    }

    fn handle_line(&self, id: u64, line: &str, tracker: &mut ProgressTracker, files: &mut Vec<String>) -> Heard {
        match ytdlp::parse_line(line) {
            Line::Meta { parts } => tracker.set_parts(parts),
            // Subtitle files are fetched before the media and report no
            // format id; they would flash the bar to 99% and back.
            Line::Progress { format_id, .. } if format_id.is_empty() && tracker.part_count() == 0 => {}
            Line::Retry { attempt, of, network } => {
                let what = if network { "Bağlantı koptu" } else { "Site hata verdi" };
                self.update(id, true, |job| {
                    if job.cancel_requested || !job.view.status.is_active() {
                        return;
                    }
                    let v = &mut job.view;
                    v.speed = None;
                    v.eta = None;
                    v.stage = Some(format!("{what}, yeniden deneniyor ({attempt}/{of})"));
                });
                return Heard::Retry(line.trim().to_string());
            }
            Line::Progress { finished, downloaded, total, speed, eta, format_id } => {
                let heard = Heard::Progress { format_id: format_id.clone(), downloaded: downloaded.unwrap_or(0) };
                let fraction = tracker.update(finished, &format_id, downloaded, total);
                let stage = if tracker.part_count() >= 2 {
                    match tracker.part_index(&format_id) {
                        Some(0) => "Görüntü indiriliyor",
                        Some(_) => "Ses indiriliyor",
                        None => "İndiriliyor",
                    }
                } else {
                    "İndiriliyor"
                };
                self.update(id, false, |job| {
                    if job.cancel_requested {
                        return;
                    }
                    let v = &mut job.view;
                    v.status = JobStatus::Downloading;
                    v.progress = fraction;
                    v.speed = speed;
                    v.eta = eta;
                    v.stage = Some(stage.into());
                });
                return heard;
            }
            Line::Post { started: true, name } => {
                self.update(id, true, |job| {
                    // Some processors (metadata parsing) run before the
                    // download starts; keep showing "preparing" for those.
                    if job.cancel_requested || job.view.status == JobStatus::Starting {
                        return;
                    }
                    let v = &mut job.view;
                    v.status = JobStatus::Processing;
                    v.progress = None;
                    v.speed = None;
                    v.eta = None;
                    v.stage = Some(ytdlp::postprocessor_label(&name, v.kind).into());
                });
            }
            Line::File(path) => {
                let path = path.trim_end().to_string();
                if !files.contains(&path) {
                    files.push(path);
                }
            }
            _ => {}
        }
        Heard::Other
    }
}

/// What a line of yt-dlp output meant for the read loop.
enum Heard {
    Progress { format_id: String, downloaded: u64 },
    Retry(String),
    Other,
}

/// Notices when yt-dlp has gone quiet: no line of output and no growth of
/// the files in the job's folders (FFmpeg as a downloader writes without
/// printing lines).
struct Silence {
    dirs: [PathBuf; 2],
    last_activity: Instant,
    bytes: u64,
    checked: Instant,
}

impl Silence {
    fn new(dirs: [PathBuf; 2]) -> Self {
        let bytes = folder_bytes(&dirs);
        Silence { dirs, last_activity: Instant::now(), bytes, checked: Instant::now() }
    }

    fn heard(&mut self) {
        self.last_activity = Instant::now();
    }

    /// How long nothing has happened. The folders are only looked at once
    /// the output has been quiet for a few seconds.
    fn quiet_for(&mut self) -> Duration {
        const LOOK_EVERY: Duration = Duration::from_secs(5);
        if self.last_activity.elapsed() >= LOOK_EVERY && self.checked.elapsed() >= LOOK_EVERY {
            self.checked = Instant::now();
            let bytes = folder_bytes(&self.dirs);
            if bytes != self.bytes {
                self.bytes = bytes;
                self.last_activity = Instant::now();
            }
        }
        self.last_activity.elapsed()
    }
}

fn folder_bytes(dirs: &[PathBuf]) -> u64 {
    dirs.iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .filter_map(|e| e.ok()?.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// A line for kova.log with the time, so a stuck download can be traced.
fn log(id: u64, text: &str) {
    eprintln!("[kova] {} download {id}: {text}", crate::util::log_time());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::{self, EngineDirs, Net};
    use crate::settings::Channel;

    /// Creative Commons (CC BY) film from the Blender Foundation's channel.
    const CC_VIDEO: &str = "https://www.youtube.com/watch?v=aqz-KE-bpKQ";

    fn request(kind: Kind, res: Option<u32>) -> DownloadRequest {
        DownloadRequest {
            url: CC_VIDEO.into(),
            title: "Big Buck Bunny".into(),
            thumbnail: None,
            kind,
            res,
            split_artist: false,
            audio_quality: None,
            subtitles: false,
            save_dir: None,
        }
    }

    #[test]
    fn restores_history_in_finish_order() {
        let temp_root = tempfile::tempdir().unwrap();
        let env = JobEnv {
            emit: Box::new(|_| {}),
            runner: Box::new(|| None),
            options: Box::new(|| JobOptions { download_dir: PathBuf::from("/tmp"), square_cover: true, max_concurrent: 1, prefer_av1: false }),
            temp_root: temp_root.path().to_path_buf(),
        };
        let jobs = JobManager::new(env);
        let entry = |title: &str, at: u64| crate::history::Entry {
            url: "https://example.com/v".into(),
            title: title.into(),
            thumbnail: None,
            kind: Kind::Mp3,
            res: None,
            file_path: format!("/Users/x/Downloads/Ambar/{title}.mp3"),
            finished_at: at,
        };
        // History is stored oldest first; newer must get the higher id.
        jobs.restore(vec![entry("eski", 1), entry("yeni", 2)]);
        let list = jobs.list();
        assert_eq!(list.iter().map(|j| j.title.as_str()).collect::<Vec<_>>(), ["eski", "yeni"]);
        assert!(list.iter().all(|j| j.status == JobStatus::Done && j.file_path.is_some()));
        assert!(list[0].id < list[1].id, "ids follow the finish order");
    }

    #[test]
    fn part_tags() {
        assert_eq!(part_tag(".Barış Manço - Gülpembe.mp4.job-3.kova-part"), Some("job-3"));
        assert_eq!(part_tag(".x.kova-part"), None);
        assert_eq!(part_tag("song.mp3"), None);
    }

    #[test]
    fn rejects_relative_save_dir() {
        let temp_root = tempfile::tempdir().unwrap();
        let env = JobEnv {
            emit: Box::new(|_| {}),
            runner: Box::new(|| None),
            options: Box::new(|| JobOptions { download_dir: PathBuf::from("/tmp"), square_cover: true, max_concurrent: 1, prefer_av1: false }),
            temp_root: temp_root.path().to_path_buf(),
        };
        let jobs = Arc::new(JobManager::new(env));
        let mut bad = request(Kind::Mp3, None);
        bad.save_dir = Some("Downloads/Ambar".into());
        assert!(jobs.enqueue(vec![bad]).is_err());
        assert!(jobs.list().is_empty());
    }

    #[test]
    fn unique_names() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_path(dir.path(), "a.mp3"), dir.path().join("a.mp3"));
        std::fs::write(dir.path().join("a.mp3"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "a.mp3"), dir.path().join("a (2).mp3"));
        std::fs::write(dir.path().join("a (2).mp3"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), "a.mp3"), dir.path().join("a (3).mp3"));
        std::fs::write(dir.path().join(".hidden"), b"x").unwrap();
        assert_eq!(unique_path(dir.path(), ".hidden"), dir.path().join(".hidden (2)"));
    }

    #[test]
    fn part_files_are_unique_per_job_and_cleaned() {
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(dest.path().join(".a.mp4.job-1.kova-part"), b"stale").unwrap();
        std::fs::write(dest.path().join("keep.mp4"), b"x").unwrap();
        remove_part_files(Some(dest.path()));
        assert!(!dest.path().join(".a.mp4.job-1.kova-part").exists());
        assert!(dest.path().join("keep.mp4").exists());
    }

    #[test]
    fn finalize_refuses_paths_outside_job() {
        let job = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let outside = other.path().join("x.mp3");
        std::fs::write(&outside, b"x").unwrap();
        assert!(finalize_file(&outside, job.path(), dest.path()).is_err());
        let inside = job.path().join("x.mp3");
        std::fs::write(&inside, b"y").unwrap();
        let sneaky = job.path().join("..").join(other.path().file_name().unwrap()).join("x.mp3");
        assert!(finalize_file(&sneaky, job.path(), dest.path()).is_err(), "'..' must not escape the job folder");
        let moved = finalize_file(&inside, job.path(), dest.path()).unwrap();
        assert_eq!(moved, dest.path().join("x.mp3"));
        assert!(!inside.exists());
    }

    /// Every format through the real queue (WAV/WEBM must not fail on cover
    /// art, subtitles get embedded):
    /// cargo test -- --ignored all_formats --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn all_formats_download() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test engines");
        let dirs = EngineDirs::new(root.join("engines"));
        engines::install_missing(&dirs, &Net::new().unwrap(), Channel::Nightly, &|_| {}).await.unwrap();
        let out = tempfile::tempdir().unwrap();
        let temp_root = tempfile::tempdir().unwrap();
        let env = JobEnv {
            emit: Box::new(|_| {}),
            runner: {
                let dirs = dirs.clone();
                let cache = root.join("cache");
                Box::new(move || crate::commands::AppState::runner(&dirs, &cache))
            },
            options: {
                let dir = out.path().to_path_buf();
                Box::new(move || JobOptions { download_dir: dir.clone(), square_cover: true, max_concurrent: 3, prefer_av1: false })
            },
            temp_root: temp_root.path().to_path_buf(),
        };
        let jobs = Arc::new(JobManager::new(env));
        let mut reqs: Vec<DownloadRequest> = [Kind::Opus, Kind::Flac, Kind::Wav, Kind::Mkv, Kind::Webm]
            .into_iter()
            .map(|k| request(k, if k.is_audio() { None } else { Some(144) }))
            .collect();
        let mut mp3 = request(Kind::Mp3, None);
        mp3.audio_quality = Some(128);
        reqs.push(mp3);
        // Sintel (CC BY) has Turkish and English subtitles.
        let mut subs = request(Kind::Mp4, Some(144));
        subs.url = "https://www.youtube.com/watch?v=eRsGyueVLvQ".into();
        subs.subtitles = true;
        reqs.push(subs);
        let views = jobs.enqueue(reqs).unwrap();
        let ffprobe = dirs.programs().unwrap().ffmpeg_dir.join(engines::bin::FFPROBE);
        for view in &views {
            let done = wait_for(&jobs, view.id, 600, |j| j.status.is_finished()).await;
            assert_eq!(done.status, JobStatus::Done, "{:?}: {:?}", view.kind, done.error);
            let path = done.file_path.unwrap();
            let probe = std::process::Command::new(&ffprobe)
                .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name", "-of", "compact"])
                .arg(&path)
                .output()
                .unwrap();
            let streams = String::from_utf8_lossy(&probe.stdout).replace('\n', " ");
            println!("{:?} -> {} | {streams}", view.kind, Path::new(&path).file_name().unwrap().to_string_lossy());
            if view.url.contains("eRsGyueVLvQ") {
                assert!(streams.contains("subtitle"), "subtitles were not embedded: {streams}");
            }
        }
    }

    /// Sites that report no codecs (direct file links, archive.org) must be
    /// accepted, and WEBM must not be offered where it cannot work:
    /// cargo test -- --ignored generic_sites --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn generic_sites() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test engines");
        let dirs = EngineDirs::new(root.join("engines"));
        engines::install_missing(&dirs, &Net::new().unwrap(), Channel::Nightly, &|_| {}).await.unwrap();
        let runner = crate::commands::AppState::runner(&dirs, &root.join("cache")).unwrap();
        // Big Buck Bunny (CC BY) as a plain file and on archive.org.
        let direct = "https://test-videos.co.uk/vids/bigbuckbunny/mp4/h264/360/Big_Buck_Bunny_360_10s_1MB.mp4";
        for url in [direct, "https://archive.org/details/BigBuckBunny_124"] {
            let info = ytdlp::fetch_info(&runner, url, false, false).await.unwrap_or_else(|e| panic!("{url}: {e}"));
            let ytdlp::MediaInfo::Video(v) = info else { panic!("{url}: expected a video") };
            println!("{url}: video={} audio={} webm={} res={:?}", v.has_video, v.has_audio, v.webm, v.resolutions);
            assert!(v.has_video, "{url}");
        }

        let out = tempfile::tempdir().unwrap();
        let temp_root = tempfile::tempdir().unwrap();
        let env = JobEnv {
            emit: Box::new(|_| {}),
            runner: {
                let dirs = dirs.clone();
                let cache = root.join("cache");
                Box::new(move || crate::commands::AppState::runner(&dirs, &cache))
            },
            options: {
                let dir = out.path().to_path_buf();
                Box::new(move || JobOptions { download_dir: dir.clone(), square_cover: true, max_concurrent: 2, prefer_av1: false })
            },
            temp_root: temp_root.path().to_path_buf(),
        };
        let jobs = Arc::new(JobManager::new(env));
        // Subtitles requested on a file that has none must still download.
        let mut mkv = request(Kind::Mkv, Some(360));
        mkv.url = direct.into();
        mkv.subtitles = true;
        // A folder picked for this download only, created on demand.
        let picked = out.path().join("Seçilen klasör");
        mkv.save_dir = Some(picked.to_string_lossy().into_owned());
        let mut webm = request(Kind::Webm, Some(360));
        webm.url = direct.into();
        let views = jobs.enqueue(vec![mkv, webm]).unwrap();
        let mkv_done = wait_for(&jobs, views[0].id, 120, |j| j.status.is_finished()).await;
        assert_eq!(mkv_done.status, JobStatus::Done, "{:?}", mkv_done.error);
        let saved = PathBuf::from(mkv_done.file_path.expect("file path"));
        assert_eq!(saved.parent(), Some(picked.as_path()));
        let webm_done = wait_for(&jobs, views[1].id, 120, |j| j.status.is_finished()).await;
        assert_eq!(webm_done.status, JobStatus::Error);
        assert_eq!(webm_done.error.as_deref(), Some("Bu site WEBM sunmuyor. MKV ya da MP4 seç."));
    }

    async fn wait_for(jobs: &JobManager, id: u64, secs: u64, done: impl Fn(&JobView) -> bool) -> JobView {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let view = jobs.list().into_iter().find(|j| j.id == id).expect("job exists");
            if done(&view) || Instant::now() > deadline {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Full pipeline against the real network:
    /// cargo test -- --ignored end_to_end --nocapture
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn end_to_end_downloads_and_cancel() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test engines");
        let dirs = EngineDirs::new(root.join("engines"));
        let net = Net::new().unwrap();
        engines::install_missing(&dirs, &net, Channel::Nightly, &|_| {}).await.unwrap();

        let out = tempfile::tempdir().unwrap();
        let temp_root = tempfile::tempdir().unwrap();
        let events: Arc<Mutex<Vec<JobEvent>>> = Arc::default();
        let env = JobEnv {
            emit: {
                let events = Arc::clone(&events);
                Box::new(move |e| events.lock().unwrap().push(e))
            },
            runner: {
                let dirs = dirs.clone();
                let cache = root.join("cache");
                Box::new(move || crate::commands::AppState::runner(&dirs, &cache))
            },
            options: {
                let dir = out.path().join("İndirilenler klasörü");
                Box::new(move || JobOptions { download_dir: dir.clone(), square_cover: true, max_concurrent: 2, prefer_av1: false })
            },
            temp_root: temp_root.path().to_path_buf(),
        };
        let jobs = Arc::new(JobManager::new(env));

        let views = jobs.enqueue(vec![request(Kind::Mp3, None), request(Kind::Mp4, Some(144))]).unwrap();
        for view in &views {
            let done = wait_for(&jobs, view.id, 300, |j| j.status.is_finished()).await;
            assert_eq!(done.status, JobStatus::Done, "{:?}: {:?}", view.kind, done.error);
            let path = PathBuf::from(done.file_path.unwrap());
            assert!(path.is_file(), "{path:?} missing");
            println!("{:?} -> {} ({} bytes)", view.kind, path.display(), path.metadata().unwrap().len());
        }
        let saw_progress = events.lock().unwrap().iter().any(|e| {
            matches!(e, JobEvent::Update { job } if job.status == JobStatus::Downloading && job.progress.unwrap_or(0.0) > 0.0)
        });
        assert!(saw_progress, "no download progress events were emitted");

        // Same request again must never overwrite or reuse the first file.
        let first_mp3 = jobs.list().into_iter().find(|j| j.id == views[0].id).unwrap().file_path.unwrap();
        let again = jobs.enqueue(vec![request(Kind::Mp3, None)]).unwrap();
        let again = wait_for(&jobs, again[0].id, 120, |j| j.status.is_finished()).await;
        assert_eq!(again.status, JobStatus::Done, "repeat download: {:?}", again.error);
        let second_mp3 = again.file_path.unwrap();
        assert_ne!(first_mp3, second_mp3);
        assert!(second_mp3.ends_with("Short Film (2).mp3"), "{second_mp3}");
        assert!(Path::new(&first_mp3).is_file() && Path::new(&second_mp3).is_file());

        // Cancel a large download midway.
        let big = jobs.enqueue(vec![request(Kind::Mp4, Some(2160))]).unwrap();
        let id = big[0].id;
        let running = wait_for(&jobs, id, 120, |j| j.status == JobStatus::Downloading && j.progress.unwrap_or(0.0) > 0.0).await;
        assert_eq!(running.status, JobStatus::Downloading, "{:?}", running.error);
        jobs.cancel(id);
        let canceled = wait_for(&jobs, id, 15, |j| j.status.is_finished()).await;
        assert_eq!(canceled.status, JobStatus::Canceled);
        assert!(!temp_root.path().join(format!("job-{id}")).exists(), "temp dir should be cleaned");
        // Only this job's processes: other tests share the engines and may still be downloading.
        #[cfg(unix)]
        {
            let job_dir = temp_root.path().join(format!("job-{id}"));
            let leftovers = std::process::Command::new("pgrep").arg("-f").arg(&job_dir).output().unwrap();
            assert!(String::from_utf8_lossy(&leftovers.stdout).trim().is_empty(), "yt-dlp/ffmpeg still running after cancel");
        }
    }
}

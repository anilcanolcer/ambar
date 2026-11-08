//! Commands the web UI can call. Every input is validated here; the UI is
//! treated as untrusted.

use crate::engines::{self, EngineDirs, EngineProgress, EnginesState, Net, UpdateOutcome};
use crate::error::{msg, AppResult};
use crate::jobs::{JobManager, JobView};
use crate::settings::{Settings, NOTICE_VERSION};
use crate::ytdlp::{self, DownloadRequest, MediaInfo, Runner};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

pub struct AppState {
    pub dirs: EngineDirs,
    pub net: Net,
    pub ytdlp_cache: PathBuf,
    pub settings_path: PathBuf,
    pub default_download_dir: PathBuf,
    pub settings: Arc<Mutex<Settings>>,
    /// Set while an install or update runs; only one at a time.
    pub engine_busy: AtomicBool,
    pub jobs: Arc<JobManager>,
    pub history: Arc<crate::history::History>,
}

pub const MISSING_ENGINES: &str = "Gerekli bileşenler eksik. Ambar'yı yeniden başlat, kurulum ekranı açılacak.";

impl AppState {
    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn runner(dirs: &EngineDirs, cache: &Path) -> Option<Runner> {
        dirs.programs().map(|p| Runner {
            ytdlp: p.ytdlp,
            deno: p.deno,
            ffmpeg_dir: p.ffmpeg_dir,
            cache_dir: cache.to_path_buf(),
        })
    }
}

/// Clears a "busy" flag even if the operation fails or panics.
pub struct BusyGuard<'a>(&'a AtomicBool);

impl<'a> BusyGuard<'a> {
    pub fn acquire(flag: &'a AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).ok().map(|_| BusyGuard(flag))
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn progress_emitter(app: &AppHandle) -> impl Fn(EngineProgress) + Send + Sync {
    let app = app.clone();
    move |p| {
        let _ = app.emit("engine-progress", p);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    app_version: &'static str,
    notice_version: u32,
    settings: Settings,
    engines: EnginesState,
    engine_busy: bool,
    av1_hardware: bool,
    jobs: Vec<JobView>,
    /// For showing paths the way Finder names them.
    home_dir: String,
    /// "macos" or "windows": the UI adapts labels, shortcuts and the title bar.
    platform: &'static str,
}

#[tauri::command]
pub fn bootstrap(app: AppHandle, state: State<'_, AppState>) -> Bootstrap {
    Bootstrap {
        app_version: env!("CARGO_PKG_VERSION"),
        notice_version: NOTICE_VERSION,
        settings: state.settings(),
        engines: state.dirs.state(),
        engine_busy: state.engine_busy.load(Ordering::SeqCst),
        av1_hardware: crate::platform::av1_hardware_decode(),
        jobs: state.jobs.list(),
        home_dir: app.path().home_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        platform: std::env::consts::OS,
    }
}

/// Ctrl+Q in the window on Windows, which has no app menu to carry it.
/// Asks first while downloads are running, like the menu's quit item.
#[tauri::command]
pub fn quit_app(app: AppHandle) {
    crate::tray::request_quit(&app);
}

#[tauri::command]
pub fn accept_notice(state: State<'_, AppState>) -> AppResult<Settings> {
    let mut settings = state.settings.lock().unwrap_or_else(|p| p.into_inner());
    settings.accepted_notice = NOTICE_VERSION;
    settings.save(&state.settings_path)?;
    Ok(settings.clone())
}

#[tauri::command]
pub async fn install_engines(app: AppHandle, state: State<'_, AppState>) -> AppResult<EnginesState> {
    if state.settings().accepted_notice < NOTICE_VERSION {
        return Err(msg("Önce kullanım notunu onaylaman gerekiyor."));
    }
    let Some(_busy) = BusyGuard::acquire(&state.engine_busy) else {
        return Err(msg(BUSY));
    };
    let channel = state.settings().ytdlp_channel;
    let progress = progress_emitter(&app);
    engines::install_missing(&state.dirs, &state.net, channel, &progress).await?;
    Ok(state.dirs.state())
}

#[tauri::command]
pub async fn update_engines(app: AppHandle, state: State<'_, AppState>, force: bool) -> AppResult<Vec<UpdateOutcome>> {
    let Some(_busy) = BusyGuard::acquire(&state.engine_busy) else {
        // Never report "nothing to update" when nothing was checked.
        return Err(msg(BUSY));
    };
    let progress = progress_emitter(&app);
    let mut outcomes = Vec::new();
    // If the user switches the yt-dlp channel while a check runs, check
    // again with the new channel before returning.
    for _ in 0..3 {
        let channel = state.settings().ytdlp_channel;
        outcomes.extend(engines::check_updates(&state.dirs, &state.net, channel, force, &progress).await);
        if state.settings().ytdlp_channel == channel {
            break;
        }
    }
    Ok(outcomes)
}

/// Error text the UI recognises as "another check is already running".
pub const BUSY: &str = "Bileşen denetimi zaten sürüyor.";

#[tauri::command]
pub fn engines_state(state: State<'_, AppState>) -> EnginesState {
    state.dirs.state()
}

#[tauri::command]
pub async fn fetch_info(state: State<'_, AppState>, url: String, playlist: bool) -> AppResult<MediaInfo> {
    let runner = AppState::runner(&state.dirs, &state.ytdlp_cache).ok_or_else(|| msg(MISSING_ENGINES))?;
    ytdlp::fetch_info(&runner, &url, playlist, crate::platform::av1_hardware_decode()).await
}

#[tauri::command]
pub fn enqueue(state: State<'_, AppState>, requests: Vec<DownloadRequest>) -> AppResult<Vec<JobView>> {
    if !state.dirs.all_installed() {
        return Err(msg(MISSING_ENGINES));
    }
    state.jobs.enqueue(requests)
}

#[tauri::command]
pub fn cancel_job(state: State<'_, AppState>, id: u64) {
    state.jobs.cancel(id);
}

#[tauri::command]
pub fn retry_job(state: State<'_, AppState>, id: u64) {
    state.jobs.retry(id);
}

#[tauri::command]
pub fn remove_job(state: State<'_, AppState>, id: u64) {
    let file = state.jobs.list().into_iter().find(|j| j.id == id).and_then(|j| j.file_path);
    state.jobs.remove(id);
    // Another row may stand for the same file name (downloaded again after
    // the first file was deleted); its entry stays.
    if let Some(file) = file {
        if !state.jobs.list().iter().any(|j| j.file_path.as_deref() == Some(file.as_str())) {
            state.history.remove(&file);
        }
    }
}

#[tauri::command]
pub fn clear_finished(state: State<'_, AppState>) {
    state.jobs.clear_finished();
    // Keep only what is still listed: a download that finished just now
    // stays, everything else is forgotten (also files on unplugged disks).
    let listed: Vec<String> = state.jobs.list().into_iter().filter_map(|j| j.file_path).collect();
    state.history.retain_only(&listed);
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: Settings) -> AppResult<Settings> {
    let dir = PathBuf::from(settings.download_dir.trim());
    if !dir.is_absolute() {
        return Err(msg("İndirme klasörü geçerli değil."));
    }
    // Only a newly chosen folder is checked, so other settings can still be
    // saved while the disk of the current one is unplugged.
    if dir != PathBuf::from(state.settings().download_dir) {
        std::fs::create_dir_all(&dir)
            .map_err(|e| msg(format!("İndirme klasörü oluşturulamadı. {}", crate::util::dir_error(&dir, &e))))?;
    }
    let saved = {
        let mut current = state.settings.lock().unwrap_or_else(|p| p.into_inner());
        let mut next = settings.normalized(&state.default_download_dir);
        next.download_dir = dir.to_string_lossy().into_owned();
        // Only accept_notice may change this.
        next.accepted_notice = current.accepted_notice;
        // The UI does not know this one and sends settings without it.
        next.tray_note_shown = current.tray_note_shown;
        next.save(&state.settings_path)?;
        *current = next.clone();
        next
    };
    state.jobs.kick();
    Ok(saved)
}

#[tauri::command]
pub fn reveal_job_file(app: AppHandle, state: State<'_, AppState>, id: u64) -> AppResult<()> {
    let path = state.jobs.file_of(id).ok_or_else(|| msg("Dosya bulunamadı. Taşınmış ya da silinmiş olabilir."))?;
    app.opener().reveal_item_in_dir(&path).map_err(|e| msg(e.to_string()))
}

#[tauri::command]
pub fn open_job_file(app: AppHandle, state: State<'_, AppState>, id: u64) -> AppResult<()> {
    let path = state.jobs.file_of(id).ok_or_else(|| msg("Dosya bulunamadı. Taşınmış ya da silinmiş olabilir."))?;
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| msg(e.to_string()))
}

#[tauri::command]
pub fn open_download_dir(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    let dir = PathBuf::from(state.settings().download_dir);
    std::fs::create_dir_all(&dir)?;
    app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(|e| msg(e.to_string()))
}

/// Opens project and license pages in the browser. Only https links to a
/// short list of known sites are allowed.
#[tauri::command]
pub fn open_link(app: AppHandle, url: String) -> AppResult<()> {
    const ALLOWED: [&str; 8] = [
        "https://github.com/",
        "https://ffmpeg.org/",
        "https://ffmpeg.martin-riedl.de/",
        "https://www.gyan.dev/",
        "https://deno.com/",
        "https://www.gnu.org/",
        "https://creativecommons.org/",
        "https://unlicense.org/",
    ];
    if !ALLOWED.iter().any(|prefix| url.starts_with(prefix)) {
        return Err(msg("Bu bağlantı açılamaz."));
    }
    app.opener().open_url(url, None::<&str>).map_err(|e| msg(e.to_string()))
}

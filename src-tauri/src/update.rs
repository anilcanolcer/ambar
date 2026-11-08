//! Ambar's own updates ("Güncelle"), from the latest GitHub release.
//!
//! The release lists every build in `latest.json`; tauri-plugin-updater picks
//! the one for this computer, checks its minisign signature against the key
//! in tauri.conf.json, and also that the signature was made for the version
//! `latest.json` announces (`requireSignedVersion`), so an old release can't
//! be passed off as a new one. Nothing is replaced before both checks pass.
//!
//! - macOS: the new Ambar.app is swapped in place of the running one, then
//!   Ambar restarts. A release that needs a newer macOS than this Mac has
//!   (`macos_minimum` in latest.json) is refused before that.
//! - Windows: the NSIS setup is started with `/P /UPDATE /R` (progress bar
//!   only, keeps the install, starts Ambar again) and Ambar exits right away.
//!
//! The UI asks first while downloads are running; they stop just before Ambar
//! is replaced, like on quitting.

use crate::commands::{AppState, BusyGuard};
use crate::error::{msg, AppError, AppResult};
use reqwest::header::ACCEPT;
use reqwest::StatusCode;
use serde::Serialize;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Error as UpdaterError, Update, Updater, UpdaterExt};

/// Download progress for the "Güncelle" button.
pub const PROGRESS_EVENT: &str = "app-update-progress";
/// At most this often (about 10 a second).
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

// A check's error is shown on its own; the install's ones (from BUSY down)
// sit under the UI's "Güncelleme kurulamadı." heading, so they don't say it again.
const CHECK_OFFLINE: &str = "Güncelleme denetlenemedi. İnternet bağlantını kontrol et.";
const CHECK_FAILED: &str = "Güncelleme denetlenemedi. Biraz sonra tekrar dene.";
const BUSY: &str = "Başka bir kurulum sürüyor, bitmesini bekle.";
const UP_TO_DATE: &str = "Kurulacak yeni sürüm yok, Ambar güncel.";
const NOT_VERIFIED: &str = "Güncelleme doğrulanamadı, bu yüzden kurulmadı. Ambar'da hiçbir şey değişmedi.";
const DOWNLOAD_OFFLINE: &str = "Güncelleme indirilemedi. İnternet bağlantını kontrol et.";
const DOWNLOAD_FAILED: &str = "Güncelleme indirilemedi. Biraz sonra tekrar dene.";
const INSTALL_FAILED: &str = "Biraz sonra tekrar dene.";
#[cfg(any(target_os = "macos", test))]
const NO_PERMISSION: &str = "Gereken izin verilmedi.";
#[cfg(target_os = "macos")]
const MOVE_TO_APPLICATIONS: &str = "Ambar'yı önce Uygulamalar klasörüne taşı, oradan açıp tekrar dene.";
#[cfg(target_os = "macos")]
const NOT_INSTALLED: &str = "Bu Ambar kopyası güncellenemez. Yeni sürümü GitHub'dan indir.";

#[derive(Default)]
pub struct UpdateState {
    /// The update the last check found; installing uses it.
    found: Mutex<Option<Update>>,
    /// Only one download and install at a time.
    installing: AtomicBool,
    /// Windows: downloads were stopped and the window and tray icon removed
    /// for the setup to take over.
    #[cfg(windows)]
    handed_over: AtomicBool,
}

/// A newer Ambar, as the UI shows it.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdate {
    version: String,
    /// Release notes, plain text.
    notes: Option<String>,
    /// RFC 3339 publish date.
    date: Option<String>,
}

impl AppUpdate {
    /// `raw` is the release's `latest.json`; the plugin has already parsed
    /// its `pub_date` as RFC 3339, so the text is passed on as it is.
    fn new(version: &str, notes: Option<&str>, raw: &serde_json::Value) -> Self {
        AppUpdate {
            version: version.to_string(),
            notes: notes.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
            date: raw.get("pub_date").and_then(|d| d.as_str()).map(str::to_string),
        }
    }

    fn of(update: &Update) -> Self {
        AppUpdate::new(&update.version, update.body.as_deref(), &update.raw_json)
    }
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq)]
pub struct AppUpdateProgress {
    downloaded: u64,
    total: Option<u64>,
}

/// Adds up downloaded bytes and says when the next progress event is due.
#[derive(Default)]
struct Progress {
    downloaded: u64,
    total: Option<u64>,
    last_emit: Option<Instant>,
}

impl Progress {
    /// True when an event should go out now.
    fn add(&mut self, chunk: usize, total: Option<u64>, now: Instant) -> bool {
        self.downloaded += chunk as u64;
        self.total = total;
        let due = match self.last_emit {
            Some(last) => now.duration_since(last) >= PROGRESS_EVERY,
            None => true,
        };
        if due {
            self.last_emit = Some(now);
        }
        due
    }

    fn view(&self) -> AppUpdateProgress {
        AppUpdateProgress { downloaded: self.downloaded, total: self.total }
    }
}

/// Sadece Ambar'a ait anahtarı ve adresi içeren sürümler güncellenebilir.
fn require_update_config(app: &AppHandle) -> AppResult<()> {
    let configured = app.config().plugins.0.get("updater")
        .and_then(|config| config.get("pubkey"))
        .and_then(|key| key.as_str())
        .is_some_and(|key| !key.trim().is_empty());
    if configured { Ok(()) } else { Err(msg("Ambar güncellemeleri henüz yapılandırılmadı.")) }
}

#[tauri::command]
pub async fn check_app_update(app: AppHandle, state: State<'_, UpdateState>) -> AppResult<Option<AppUpdate>> {
    require_update_config(&app)?;
    let found = check(&app).await?;
    let view = found.as_ref().map(AppUpdate::of);
    *state.found.lock().unwrap_or_else(|p| p.into_inner()) = found;
    Ok(view)
}

/// Downloads, verifies and installs the update, then restarts Ambar. Only
/// returns on failure: on success Ambar exits.
#[tauri::command]
pub async fn install_app_update(app: AppHandle, state: State<'_, UpdateState>) -> AppResult<()> {
    require_update_config(&app)?;
    let Some(_busy) = BusyGuard::acquire(&state.installing) else {
        return Err(msg(BUSY));
    };
    #[cfg(target_os = "macos")]
    check_install_location()?;
    let found = state.found.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let update = match found {
        Some(update) => update,
        None => check(&app).await?.ok_or_else(|| msg(UP_TO_DATE))?,
    };
    #[cfg(target_os = "macos")]
    runs_on_this_mac(&update)?;
    let bytes = download(&app, &update).await?;
    install(&app, &state, update, bytes).await
}

fn updater(app: &AppHandle) -> tauri_plugin_updater::Result<Updater> {
    let builder = app
        .updater_builder()
        // Only the check: latest.json is tiny. The download has no overall
        // limit, so it can finish on a slow line.
        .timeout(Duration::from_secs(30))
        // The same limits as the engine downloads.
        .configure_client(|client| client.connect_timeout(Duration::from_secs(20)).read_timeout(Duration::from_secs(60)));
    // Runs once the setup is written to disk, right before it is started and
    // Ambar exits without RunEvent::Exit.
    #[cfg(windows)]
    let builder = {
        let app = app.clone();
        builder.on_before_exit(move || hand_over(&app))
    };
    builder.build()
}

async fn check(app: &AppHandle) -> AppResult<Option<Update>> {
    let result = match updater(app) {
        Ok(updater) => updater.check().await,
        Err(e) => Err(e),
    };
    let found = match result {
        // latest.json did not answer with 2xx, and the plugin doesn't say
        // with what: asking once more tells "nothing released" from GitHub
        // trouble.
        Err(UpdaterError::ReleaseNotFound) => no_release(endpoint_status(app).await?)?,
        result => found_or_none(result)?,
    };
    #[cfg(target_os = "macos")]
    if let Some(update) = &found {
        runs_on_this_mac(update)?;
    }
    Ok(found)
}

/// The status latest.json answers with now, through Ambar's own client.
async fn endpoint_status(app: &AppHandle) -> AppResult<StatusCode> {
    let state = app.try_state::<AppState>().ok_or_else(|| msg(CHECK_FAILED))?;
    let url = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u["endpoints"][0].as_str())
        .ok_or_else(|| msg(CHECK_FAILED))?;
    let request = state.net.client().get(url).header(ACCEPT, "application/json").timeout(Duration::from_secs(15));
    match request.send().await {
        Ok(response) => Ok(response.status()),
        Err(e) => {
            eprintln!("[kova] update check failed: {e}");
            Err(msg(CHECK_OFFLINE))
        }
    }
}

/// Only 404 means nothing is released yet, or the latest release has no
/// latest.json. GitHub being down (5xx) or limiting requests (403, 429)
/// must not look like "Ambar güncel".
fn no_release<T>(status: StatusCode) -> AppResult<Option<T>> {
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    eprintln!("[kova] update check failed: latest.json answered {status}");
    Err(msg(CHECK_FAILED))
}

/// A release without an update for this computer is not an error.
/// `ReleaseNotFound` is decided by [`no_release`] instead; here it is one.
fn found_or_none<T>(result: Result<Option<T>, UpdaterError>) -> AppResult<Option<T>> {
    match result {
        Ok(found) => Ok(found),
        // The newer release has no build for this system (yet).
        Err(e @ (UpdaterError::TargetNotFound(_) | UpdaterError::TargetsNotFound(_))) => {
            eprintln!("[kova] update check: {e}");
            Ok(None)
        }
        Err(e) => {
            eprintln!("[kova] update check failed: {e}");
            Err(check_error(&e))
        }
    }
}

fn check_error(e: &UpdaterError) -> AppError {
    match e {
        UpdaterError::Reqwest(e) if !e.is_decode() => msg(CHECK_OFFLINE),
        _ => msg(CHECK_FAILED),
    }
}

/// The plugin checks the signature before it hands the bytes over.
async fn download(app: &AppHandle, update: &Update) -> AppResult<Vec<u8>> {
    let mut progress = Progress::default();
    let result = update
        .download(
            |chunk, total| {
                if progress.add(chunk, total, Instant::now()) {
                    let _ = app.emit(PROGRESS_EVENT, progress.view());
                }
            },
            || {},
        )
        .await;
    match result {
        Ok(bytes) => {
            // The last chunks may have fallen inside the throttle window.
            let _ = app.emit(PROGRESS_EVENT, progress.view());
            Ok(bytes)
        }
        Err(e) => {
            eprintln!("[kova] update download failed: {e}");
            Err(install_error(&e))
        }
    }
}

/// Runs the plugin's install (signature checked again first) off the async
/// threads: it unpacks or writes tens of MB.
async fn run_install(update: Update, bytes: Vec<u8>) -> AppResult<Result<(), UpdaterError>> {
    Ok(tauri::async_runtime::spawn_blocking(move || update.install(bytes)).await?)
}

#[cfg(target_os = "macos")]
async fn install(app: &AppHandle, _state: &UpdateState, update: Update, bytes: Vec<u8>) -> AppResult<()> {
    if let Err(e) = run_install(update, bytes).await? {
        eprintln!("[kova] update install failed: {e}");
        return Err(install_error(&e));
    }
    // The new Ambar.app is in place; the running one keeps working until
    // the restart, so downloads only stop now.
    stop_downloads(app);
    // From this thread it goes through RunEvent::Exit, then opens the new app.
    app.restart();
}

#[cfg(windows)]
async fn install(app: &AppHandle, state: &UpdateState, update: Update, bytes: Vec<u8>) -> AppResult<()> {
    use std::sync::atomic::Ordering;
    // Starts the setup and exits on success.
    let result = run_install(update, bytes).await?;
    let error = result.err().map(|e| {
        eprintln!("[kova] update install failed: {e}");
        install_error(&e)
    });
    if state.handed_over.load(Ordering::SeqCst) {
        // The setup didn't start, but downloads are already stopped and the
        // window and tray icon are gone. Start Ambar again instead of leaving
        // it running unseen.
        use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
        app.dialog()
            .message("Güncelleme başlatılamadı. Ambar yeniden açılacak, biraz sonra tekrar dene.")
            .title("Ambar")
            .kind(MessageDialogKind::Error)
            .blocking_show();
        app.restart();
    }
    Err(error.unwrap_or_else(|| msg(INSTALL_FAILED)))
}

/// Windows, right before the setup starts: what quitting does, since the
/// plugin then ends the process without RunEvent::Exit.
#[cfg(windows)]
fn hand_over(app: &AppHandle) {
    use std::sync::atomic::Ordering;
    if let Some(state) = app.try_state::<UpdateState>() {
        state.handed_over.store(true, Ordering::SeqCst);
    }
    stop_downloads(app);
    // Removes the tray icon (else a dead one stays until hovered) and hides
    // the window. Tray icons belong to the main thread, so it runs there;
    // if that thread is stuck, Ambar exits without it.
    let (done, wait) = std::sync::mpsc::channel();
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        handle.cleanup_before_exit();
        let _ = done.send(());
    });
    if queued.is_ok() {
        let _ = wait.recv_timeout(Duration::from_secs(3));
    }
}

/// The same as RunEvent::Exit in main.rs.
fn stop_downloads(app: &AppHandle) {
    if let Some(state) = app.try_state::<AppState>() {
        state.jobs.kill_all();
    }
    crate::jobs::remove_part_files(None);
}

fn install_error(e: &UpdaterError) -> AppError {
    use UpdaterError as E;
    match e {
        E::Minisign(_)
        | E::Base64(_)
        | E::SignatureUtf8(_)
        | E::SignedVersionMismatch { .. }
        | E::MissingSignedVersion => msg(NOT_VERIFIED),
        E::Reqwest(e) if !e.is_decode() => msg(DOWNLOAD_OFFLINE),
        E::Reqwest(_) | E::Network(_) => msg(DOWNLOAD_FAILED),
        E::PreviousAppNotRestored(kept) => msg(format!("Önceki Ambar şurada duruyor: {}", kept.display())),
        // Declined password prompt, or the app's folder is not writable.
        #[cfg(target_os = "macos")]
        E::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => msg(NO_PERMISSION),
        #[cfg(target_os = "macos")]
        E::TempDirNotOnSameMountPoint => msg(MOVE_TO_APPLICATIONS),
        _ => msg(INSTALL_FAILED),
    }
}

/// macOS: refuses before downloading when the update can't be put in place.
#[cfg(target_os = "macos")]
fn check_install_location() -> AppResult<()> {
    let exe = tauri::utils::platform::current_exe()?;
    let bundle = app_bundle(&exe).ok_or_else(|| msg(NOT_INSTALLED))?;
    // Opened from the DMG, or from Downloads without moving it (macOS then
    // runs it from a read-only copy).
    if on_read_only_volume(bundle) {
        return Err(msg(MOVE_TO_APPLICATIONS));
    }
    Ok(())
}

/// macOS: refuses a release this Mac is too old for. The plugin would swap
/// it in anyway, and the old Ambar is gone by the time the new one fails to
/// open. Releases put their minimumSystemVersion in latest.json as
/// `macos_minimum`; one without it is let through.
#[cfg(target_os = "macos")]
fn runs_on_this_mac(update: &Update) -> AppResult<()> {
    let Some(minimum) = update.raw_json.get("macos_minimum").and_then(|m| m.as_str()) else {
        return Ok(());
    };
    let Some(os) = macos_version() else {
        return Ok(());
    };
    if older_than(&os, minimum) {
        eprintln!("[kova] update {} needs macOS {minimum}, this Mac has {os}", update.version);
        return Err(msg(format!("Ambar {} macOS {minimum} veya üstünü istiyor, bu Mac'te macOS {os} var.", update.version)));
    }
    Ok(())
}

/// "12.7.4" is older than "13.0"; "12" and "12.0" are the same. False when
/// either can't be read, so a typo doesn't block every update.
#[cfg(any(target_os = "macos", test))]
fn older_than(version: &str, minimum: &str) -> bool {
    let parts = |s: &str| s.trim().split('.').map(|p| p.parse::<u32>().ok()).collect::<Option<Vec<_>>>();
    let (Some(mut a), Some(mut b)) = (parts(version), parts(minimum)) else {
        return false;
    };
    let len = a.len().max(b.len());
    a.resize(len, 0);
    b.resize(len, 0);
    a < b
}

/// "12.7.4". Ambar is built with a current SDK, so macOS 11+ never reports
/// the "10.16" stand-in here.
#[cfg(target_os = "macos")]
fn macos_version() -> Option<String> {
    let mut buf = [0u8; 32];
    let mut len = buf.len();
    // SAFETY: the name is NUL-terminated, and len tells sysctl how much of buf it may fill.
    let status = unsafe {
        libc::sysctlbyname(c"kern.osproductversion".as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
    };
    if status != 0 {
        return None;
    }
    let text = std::str::from_utf8(buf.get(..len)?).ok()?.trim_end_matches('\0');
    (!text.is_empty()).then(|| text.to_string())
}

/// `/Applications/Ambar.app` for `/Applications/Ambar.app/Contents/MacOS/kova`.
#[cfg(target_os = "macos")]
fn app_bundle(exe: &std::path::Path) -> Option<&std::path::Path> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let named = |p: &std::path::Path, name: &str| p.file_name().is_some_and(|n| n == name);
    let is_app = bundle.extension().is_some_and(|e| e == "app");
    (named(macos, "MacOS") && named(contents, "Contents") && is_app).then_some(bundle)
}

#[cfg(target_os = "macos")]
fn on_read_only_volume(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // The volume, not the folder: one the user can't write to (an admin's
    // /Applications) still works, the plugin asks for the password then.
    let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: c_path is NUL-terminated and fs is only read after statfs filled it.
    unsafe {
        libc::statfs(c_path.as_ptr(), fs.as_mut_ptr()) == 0 && (fs.assume_init_ref().f_flags & libc::MNT_RDONLY as u32) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn update_serializes_for_the_ui() {
        let raw = json!({ "version": "0.5.0", "pub_date": "2026-11-02T10:00:00Z", "platforms": {} });
        let update = AppUpdate::new("0.5.0", Some("  Hata düzeltmeleri.\n"), &raw);
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            json!({ "version": "0.5.0", "notes": "Hata düzeltmeleri.", "date": "2026-11-02T10:00:00Z" })
        );
        let bare = AppUpdate::new("0.5.0", Some("   "), &json!({ "version": "0.5.0" }));
        assert_eq!(serde_json::to_value(&bare).unwrap(), json!({ "version": "0.5.0", "notes": null, "date": null }));
    }

    #[test]
    fn progress_serializes_for_the_ui() {
        let p = AppUpdateProgress { downloaded: 512, total: None };
        assert_eq!(serde_json::to_value(p).unwrap(), json!({ "downloaded": 512, "total": null }));
    }

    #[test]
    fn progress_is_throttled() {
        let start = Instant::now();
        let mut p = Progress::default();
        assert!(p.add(100, Some(1000), start), "the first chunk shows at once");
        assert!(!p.add(100, Some(1000), start + Duration::from_millis(50)));
        assert!(p.add(100, Some(1000), start + Duration::from_millis(100)));
        assert!(!p.add(100, Some(1000), start + Duration::from_millis(150)));
        assert_eq!(p.view(), AppUpdateProgress { downloaded: 400, total: Some(1000) });
    }

    /// The plugin reads this at start-up; a typo there would stop Ambar from opening.
    #[test]
    fn shipped_updater_config_parses() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let config: tauri_plugin_updater::Config = serde_json::from_value(conf["plugins"]["updater"].clone()).unwrap();
        assert!(config.require_signed_version);
        assert!(!config.allow_downgrades);
        assert_eq!(
            config.endpoints.iter().map(|u| u.as_str()).collect::<Vec<_>>(),
            ["https://github.com/anilcanolcer/ambar/releases/latest/download/latest.json"]
        );
        assert_eq!(config.windows.unwrap().install_mode.to_string(), "passive");
        // The CI release step injects the public key; source builds leave it empty.
    }

    /// The shape latest.json must have: one universal .app.tar.gz under both
    /// Mac keys, the NSIS setup under the Windows one.
    #[test]
    fn release_manifest_covers_this_system() {
        let mac = json!({ "url": "https://github.com/anilcanolcer/ambar/releases/download/v0.5.0/Ambar.app.tar.gz", "signature": "c2ln" });
        let manifest = json!({
            "version": "0.5.0",
            "notes": "Hata düzeltmeleri.",
            "pub_date": "2026-11-02T10:00:00Z",
            "platforms": {
                "darwin-aarch64": mac,
                "darwin-x86_64": mac,
                "windows-x86_64": { "url": "https://github.com/anilcanolcer/ambar/releases/download/v0.5.0/Ambar_0.5.0_x64-setup.exe", "signature": "c2ln" },
            },
        });
        let release: tauri_plugin_updater::RemoteRelease = serde_json::from_value(manifest).unwrap();
        let target = tauri_plugin_updater::target().unwrap();
        assert!(release.download_url(&target).is_ok(), "{target} missing");
        assert_eq!(release.version.to_string(), "0.5.0");
        // A date that is not RFC 3339 makes the whole check fail.
        let bad_date = json!({ "version": "0.5.0", "pub_date": "2 Kasım 2026", "platforms": {} });
        assert!(serde_json::from_value::<tauri_plugin_updater::RemoteRelease>(bad_date).is_err());
    }

    fn message(e: AppError) -> String {
        e.to_string()
    }

    #[test]
    fn nothing_released_is_not_an_error() {
        assert_eq!(no_release::<u8>(StatusCode::NOT_FOUND).unwrap(), None);
        let other_systems = UpdaterError::TargetsNotFound(vec!["windows-x86_64-nsis".into(), "windows-x86_64".into()]);
        assert_eq!(found_or_none::<u8>(Err(other_systems)).unwrap(), None);
        assert_eq!(found_or_none::<u8>(Err(UpdaterError::TargetNotFound("darwin-aarch64".into()))).unwrap(), None);
        assert_eq!(found_or_none(Ok(Some(7u8))).unwrap(), Some(7));
        assert_eq!(found_or_none::<u8>(Ok(None)).unwrap(), None);
    }

    #[test]
    fn other_check_failures_are_errors() {
        let bad_json = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let e = found_or_none::<u8>(Err(UpdaterError::Serialization(bad_json))).unwrap_err();
        assert_eq!(message(e), CHECK_FAILED);
        // GitHub trouble is not "Ambar güncel".
        for status in [500, 502, 503, 429, 403, 200] {
            let e = no_release::<u8>(StatusCode::from_u16(status).unwrap()).unwrap_err();
            assert_eq!(message(e), CHECK_FAILED, "{status}");
        }
        // check() asks again instead of passing it here.
        assert_eq!(message(found_or_none::<u8>(Err(UpdaterError::ReleaseNotFound)).unwrap_err()), CHECK_FAILED);
    }

    #[test]
    fn compares_macos_versions() {
        assert!(older_than("12.7.4", "13.0"));
        assert!(older_than("12.7", "13"));
        assert!(older_than("10.15.7", "12.0"));
        assert!(!older_than("12.0", "12.0"));
        assert!(!older_than("12", "12.0"));
        assert!(!older_than("12.7.4", "12.0"));
        assert!(!older_than("13.0", "12.7"));
        assert!(!older_than("26.1", "12.0"));
        // Unreadable either way: not blocked.
        assert!(!older_than("12.7.4", ""));
        assert!(!older_than("", "13.0"));
        assert!(!older_than("12.x", "13.0"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_this_macs_version() {
        let os = macos_version().unwrap();
        assert!(!older_than(&os, "11.0"), "{os}");
        assert!(older_than(&os, "999.0"), "{os} unreadable");
    }

    #[test]
    fn signature_problems_say_nothing_changed() {
        let mismatch = UpdaterError::SignedVersionMismatch { signed: "0.4.0".into(), announced: "0.5.0".into() };
        assert_eq!(message(install_error(&mismatch)), NOT_VERIFIED);
        assert_eq!(message(install_error(&UpdaterError::MissingSignedVersion)), NOT_VERIFIED);
        assert_eq!(message(install_error(&UpdaterError::SignatureUtf8("x".into()))), NOT_VERIFIED);
        assert!(NOT_VERIFIED.contains("hiçbir şey değişmedi"));
    }

    #[test]
    fn install_failures_are_turkish() {
        let status = UpdaterError::Network("Download request failed with status: 404 Not Found".into());
        assert_eq!(message(install_error(&status)), DOWNLOAD_FAILED);
        assert_eq!(message(install_error(&UpdaterError::InvalidUpdaterFormat)), INSTALL_FAILED);
        let kept = UpdaterError::PreviousAppNotRestored("/Applications/Ambar (previous version).app".into());
        assert!(message(install_error(&kept)).ends_with("/Applications/Ambar (previous version).app"));
        let denied = UpdaterError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let expected = if cfg!(target_os = "macos") { NO_PERMISSION } else { INSTALL_FAILED };
        assert_eq!(message(install_error(&denied)), expected);
        // The UI's heading already says "Güncelleme kurulamadı."
        for text in [BUSY, UP_TO_DATE, NOT_VERIFIED, DOWNLOAD_OFFLINE, DOWNLOAD_FAILED, INSTALL_FAILED, NO_PERMISSION] {
            assert!(!text.contains("kurulamadı"), "{text}");
        }
        assert!(!message(install_error(&kept)).contains("kurulamadı"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_the_app_bundle() {
        use std::path::Path;
        let exe = Path::new("/Applications/Ambar.app/Contents/MacOS/ambar");
        assert_eq!(app_bundle(exe), Some(Path::new("/Applications/Ambar.app")));
        assert_eq!(app_bundle(Path::new("/Users/sen/ambar/src-tauri/target/debug/ambar")), None);
        assert_eq!(app_bundle(Path::new("/Applications/Ambar/Contents/MacOS/ambar")), None);
        assert_eq!(app_bundle(Path::new("/ambar")), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn spots_read_only_volumes() {
        // The system volume is mounted read-only since macOS 10.15.
        assert!(on_read_only_volume(std::path::Path::new("/System")));
        let dir = tempfile::tempdir().unwrap();
        assert!(!on_read_only_volume(dir.path()));
        assert!(!on_read_only_volume(&dir.path().join("missing")));
    }
}

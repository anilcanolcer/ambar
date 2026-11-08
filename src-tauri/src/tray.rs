//! The menu bar icon (on Windows, the tray icon by the clock). It downloads
//! the video open in the browser without copying its link (on Windows, the
//! copied link), shows download progress next to the icon (and on the Dock
//! icon or the taskbar button), and keeps Ambar reachable while its window is
//! closed.

#[cfg(target_os = "macos")]
use crate::browser::{self, BrowserError};
use crate::commands::AppState;
use crate::jobs::{JobStatus, JobView};
use crate::settings::{Kind, NOTICE_VERSION};
use crate::ytdlp::{self, DownloadRequest, MediaInfo};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::window::{ProgressBarState, ProgressBarStatus};
use tauri::{AppHandle, Emitter, Manager, UserAttentionType, Wry};
use tauri_plugin_dialog::DialogExt;
#[cfg(windows)]
use tauri_plugin_dialog::{MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

const TRAY_ID: &str = "ambar";
/// Menu item ids. Job rows are "job:<id>".
pub const QUIT: &str = "quit";
const GRAB_AUDIO: &str = "grab:audio";
const GRAB_VIDEO: &str = "grab:video";
const GRAB_CHOOSE: &str = "grab:choose";
const OPEN_WINDOW: &str = "open-window";
const OPEN_FOLDER: &str = "open-folder";
/// The menu bar icon at rest: Ambar's house mark, so any download (which starts
/// from empty) looks clearly different.
#[cfg(target_os = "macos")]
const RESTING: &[u8] = include_bytes!("../icons/tray/20.png");
/// While downloading, the bucket fills with the overall progress in 5% steps.
#[cfg(target_os = "macos")]
const FILL: [&[u8]; 21] = [
    include_bytes!("../icons/tray/00.png"),
    include_bytes!("../icons/tray/01.png"),
    include_bytes!("../icons/tray/02.png"),
    include_bytes!("../icons/tray/03.png"),
    include_bytes!("../icons/tray/04.png"),
    include_bytes!("../icons/tray/05.png"),
    include_bytes!("../icons/tray/06.png"),
    include_bytes!("../icons/tray/07.png"),
    include_bytes!("../icons/tray/08.png"),
    include_bytes!("../icons/tray/09.png"),
    include_bytes!("../icons/tray/10.png"),
    include_bytes!("../icons/tray/11.png"),
    include_bytes!("../icons/tray/12.png"),
    include_bytes!("../icons/tray/13.png"),
    include_bytes!("../icons/tray/14.png"),
    include_bytes!("../icons/tray/15.png"),
    include_bytes!("../icons/tray/16.png"),
    include_bytes!("../icons/tray/17.png"),
    include_bytes!("../icons/tray/18.png"),
    include_bytes!("../icons/tray/19.png"),
    include_bytes!("../icons/tray/20.png"),
];

/// Windows shows tray icons as they are, so there is one set for a dark and
/// one for a light taskbar (made by scripts/make-tray-icons.mjs). Frames 00..19
/// fill up in red while downloading; 20, the full bucket without red, is the
/// icon at rest.
#[cfg(windows)]
macro_rules! tray_frames {
    ($theme:literal) => {
        [
            include_bytes!(concat!("../icons/tray-win/", $theme, "/00.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/01.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/02.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/03.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/04.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/05.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/06.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/07.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/08.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/09.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/10.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/11.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/12.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/13.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/14.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/15.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/16.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/17.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/18.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/19.png")),
            include_bytes!(concat!("../icons/tray-win/", $theme, "/20.png")),
        ]
    };
}
#[cfg(windows)]
const FILL_DARK: [&[u8]; 21] = tray_frames!("dark");
#[cfg(windows)]
const FILL_LIGHT: [&[u8]; 21] = tray_frames!("light");
/// Set while the Windows taskbar is light; watched by `watch`.
#[cfg(windows)]
static LIGHT_TASKBAR: AtomicBool = AtomicBool::new(false);

/// Which icon is showing, as the key from `icon`.
static SHOWN_ICON: AtomicUsize = AtomicUsize::new(usize::MAX);

/// The icon for `frame` (an index into FILL, or usize::MAX at rest) and a
/// key that tells it apart from every other icon.
#[cfg(target_os = "macos")]
fn icon(frame: usize) -> (usize, &'static [u8]) {
    (frame, if frame == usize::MAX { RESTING } else { FILL[frame] })
}

/// On Windows the key also says which taskbar colour the icon is for.
#[cfg(windows)]
fn icon(frame: usize) -> (usize, &'static [u8]) {
    let frame = frame.min(FILL_DARK.len() - 1);
    if LIGHT_TASKBAR.load(Ordering::SeqCst) {
        (FILL_DARK.len() + frame, FILL_LIGHT[frame])
    } else {
        (frame, FILL_DARK[frame])
    }
}

/// How many downloads the menu lists.
const MENU_JOBS: usize = 6;
#[cfg(target_os = "macos")]
const DEFAULT_GRAB_LABEL: &str = "Tarayıcıdaki videoyu indir";
/// Windows reads the copied link instead of the browser (see clipboard.rs).
#[cfg(windows)]
const NO_LINK_LABEL: &str = "Önce bir video linki kopyala";

/// The menu is only ever changed in place: replacing it would close it
/// while the user is looking at it.
struct Live {
    menu: Menu<Wry>,
    grab: Submenu<Wry>,
    header: MenuItem<Wry>,
    separator: PredefinedMenuItem<Wry>,
    jobs: Vec<(u64, MenuItem<Wry>)>,
    /// Which jobs are listed and whether each is finished; a change swaps the rows.
    shape: Vec<(u64, bool)>,
}

/// Where the job rows start: after the download submenu and a separator.
const JOBS_AT: usize = 2;

static LIVE: Mutex<Option<Live>> = Mutex::new(None);
static REFRESH_PENDING: AtomicBool = AtomicBool::new(false);
/// Something changed while a refresh was already running.
static REFRESH_AGAIN: AtomicBool = AtomicBool::new(false);
/// Every download that ran since the queue was last idle.
static BUSY_IDS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static GRABBING: AtomicBool = AtomicBool::new(false);
/// Held for a whole refresh, so two never change the menu at the same time.
static REFRESHING: Mutex<()> = Mutex::new(());
/// Bumped whenever the window is shown or a delayed hide is scheduled, so an
/// outdated delayed hide can tell it must not run (see main.rs).
pub static WINDOW_GEN: AtomicU64 = AtomicU64::new(0);
/// The Windows quit question is on screen.
#[cfg(windows)]
static QUIT_ASKING: AtomicBool = AtomicBool::new(false);
/// What the download item knows about the clipboard (Windows). Held while
/// the item changes, so two checks can't leave an outdated label behind.
#[cfg(windows)]
static CLIPBOARD_SEEN: Mutex<ClipboardSeen> = Mutex::new(ClipboardSeen { shown: None, failed: None });
/// How often a copy that can't be read is tried before the item gives up
/// on it: a few seconds of checks.
#[cfg(windows)]
const CLIPBOARD_TRIES: u8 = 5;
/// The job rows changed while the menu was open (Windows); `watch` puts
/// them in once it closes.
#[cfg(windows)]
static MENU_OUTDATED: AtomicBool = AtomicBool::new(false);
/// What the job rows last said (Windows). An open menu is drawn again only
/// when that changes, not on every progress event.
#[cfg(windows)]
static ROW_TEXT: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(windows)]
struct ClipboardSeen {
    /// The clipboard change the download item shows.
    shown: Option<u32>,
    /// A change that could not be read yet, and how often that failed.
    failed: Option<(u32, u8)>,
}

#[derive(Clone, Serialize)]
struct Notice {
    text: String,
    kind: &'static str,
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    #[cfg(windows)]
    {
        LIGHT_TASKBAR.store(crate::platform::light_taskbar(), Ordering::SeqCst);
        // The tray icon is made on this thread, which also shows its menu.
        crate::platform::tray_menu::remember_thread();
    }
    let (key, resting) = icon(usize::MAX);
    SHOWN_ICON.store(key, Ordering::SeqCst);
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(resting)?)
        .icon_as_template(true)
        .tooltip("Ambar")
        .menu(&menu)
        // Windows opens the menu on a right click anyway; a left click opens
        // it too, as on the Mac.
        .show_menu_on_left_click(true)
        .on_tray_icon_event(|tray, event| {
            // Hovering happens just before the click that opens the menu:
            // name the browser the item will read from.
            if let TrayIconEvent::Enter { .. } = event {
                let _ = tray;
                #[cfg(target_os = "macos")]
                tauri::async_runtime::spawn(name_browser());
                // On Windows, catch a link copied a moment ago.
                #[cfg(windows)]
                tauri::async_runtime::spawn_blocking(|| check_clipboard(false));
            }
        })
        .build(app)?;
    #[cfg(windows)]
    {
        let app = app.clone();
        std::thread::Builder::new().name("kova-tray".into()).spawn(move || watch(&app))?;
    }
    Ok(())
}

/// Windows: watches the clipboard for a video link, which the download item
/// offers, and the taskbar for a switch between light and dark, which the
/// icon's colours follow.
#[cfg(windows)]
fn watch(app: &AppHandle) {
    loop {
        check_clipboard(true);
        let light = crate::platform::light_taskbar();
        if LIGHT_TASKBAR.swap(light, Ordering::SeqCst) != light {
            // Redraws the icon for the new colour.
            jobs_changed(app);
        }
        // Job rows that waited for the menu to close (see update_menu).
        if !crate::platform::tray_menu::is_open() && MENU_OUTDATED.swap(false, Ordering::SeqCst) {
            jobs_changed(app);
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}

/// Windows: names the copied link's site in the download item, or disables
/// the item when no video link is copied. Only reads the clipboard when
/// something new was copied. The check in the background (`watch`) leaves
/// Office copies to the hover just before a click (see
/// clipboard::holds_document).
#[cfg(windows)]
fn check_clipboard(background: bool) {
    let mut seen = CLIPBOARD_SEEN.lock().unwrap_or_else(|p| p.into_inner());
    let sequence = crate::clipboard::sequence();
    if seen.shown == Some(sequence) {
        return;
    }
    let Some(grab) = LIVE.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|l| l.grab.clone()) else {
        return;
    };
    if background && crate::clipboard::holds_document() {
        return;
    }
    let link = match crate::clipboard::read_link() {
        Ok(link) => link,
        // Usually another program has the clipboard open for a moment: try
        // again on the next check instead of saying "no link" until
        // something else is copied.
        Err(_) => {
            let tries = match seen.failed {
                Some((failed, tries)) if failed == sequence => tries + 1,
                _ => 1,
            };
            seen.failed = Some((sequence, tries));
            if tries < CLIPBOARD_TRIES {
                return;
            }
            None
        }
    };
    seen.shown = Some(sequence);
    let label = match link.as_deref().and_then(crate::clipboard::site) {
        // A single "&" would be taken as a keyboard-shortcut marker.
        Some(site) => format!("Kopyalanan linki indir · {}", site.replace('&', "&&")),
        None if link.is_some() => "Kopyalanan linki indir".into(),
        None => NO_LINK_LABEL.into(),
    };
    let _ = grab.set_text(label);
    let _ = grab.set_enabled(link.is_some());
    // The menu may already be open, when reading took a while.
    crate::platform::tray_menu::repaint();
}

#[cfg(target_os = "macos")]
async fn name_browser() {
    let label = match browser::pick().await {
        Some(b) => format!("{} videoyu indir", b.locative),
        None => DEFAULT_GRAB_LABEL.into(),
    };
    let grab = LIVE.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|l| l.grab.clone());
    if let Some(grab) = grab {
        let _ = grab.set_text(label);
    }
}

fn short(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let cut: String = title.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

fn percent(fraction: f64) -> String {
    format!("%{}", (fraction.clamp(0.0, 1.0) * 100.0).floor() as u32)
}

fn job_label(job: &JobView) -> String {
    // A single "&" would be taken as a keyboard-shortcut marker and vanish.
    let title = short(&job.title, 42).replace('&', "&&");
    match job.status {
        JobStatus::Queued => format!("Sırada · {title}"),
        JobStatus::Starting => format!("Hazırlanıyor · {title}"),
        JobStatus::Downloading => match job.progress {
            Some(p) => format!("{} · {title}", percent(p)),
            None => format!("İniyor · {title}"),
        },
        JobStatus::Processing => format!("Son rötuşlar · {title}"),
        JobStatus::Done => format!("✓ {title}"),
        JobStatus::Error => format!("Hata · {title}"),
        JobStatus::Canceled => format!("İptal edildi · {title}"),
    }
}

/// Running downloads first, then the most recent finished ones.
fn listed(jobs: &[JobView]) -> Vec<&JobView> {
    let mut active: Vec<&JobView> = jobs.iter().filter(|j| !j.status.is_finished()).collect();
    let mut finished: Vec<&JobView> = jobs.iter().filter(|j| j.status.is_finished() && j.status != JobStatus::Canceled).collect();
    finished.sort_by(|a, b| b.id.cmp(&a.id));
    active.extend(finished);
    active.truncate(MENU_JOBS);
    active
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    #[cfg(target_os = "macos")]
    let (grab_label, grab_enabled) = (DEFAULT_GRAB_LABEL, true);
    // Until the clipboard is checked (right after start).
    #[cfg(windows)]
    let (grab_label, grab_enabled) = (NO_LINK_LABEL, false);
    let grab = Submenu::with_items(
        app,
        grab_label,
        grab_enabled,
        &[
            &MenuItem::with_id(app, GRAB_AUDIO, "Ses olarak (MP3)", true, None::<&str>)?,
            &MenuItem::with_id(app, GRAB_VIDEO, "Video olarak (MP4)", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, GRAB_CHOOSE, "Ambar'da seç…", true, None::<&str>)?,
        ],
    )?;
    let menu = Menu::with_items(
        app,
        &[
            &grab,
            &PredefinedMenuItem::separator(app)?,
            // Job rows go here (JOBS_AT).
            &MenuItem::with_id(app, OPEN_WINDOW, "Ambar'yı aç", true, None::<&str>)?,
            &MenuItem::with_id(app, OPEN_FOLDER, "İndirme klasörünü aç", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, QUIT, "Ambar'dan çık", true, None::<&str>)?,
        ],
    )?;
    *LIVE.lock().unwrap_or_else(|p| p.into_inner()) = Some(Live {
        menu: menu.clone(),
        grab,
        header: MenuItem::new(app, "İndirilenler", false, None::<&str>)?,
        separator: PredefinedMenuItem::separator(app)?,
        jobs: Vec::new(),
        shape: Vec::new(),
    });
    Ok(menu)
}

fn update_menu(app: &AppHandle, jobs: &[JobView]) {
    let shown = listed(jobs);
    let shape: Vec<(u64, bool)> = shown.iter().map(|j| (j.id, j.status.is_finished())).collect();
    // Copied out so no lock is held while the menu changes on the main thread.
    let Some((menu, header, separator, old, same)) = ({
        let live = LIVE.lock().unwrap_or_else(|p| p.into_inner());
        live.as_ref().map(|l| (l.menu.clone(), l.header.clone(), l.separator.clone(), l.jobs.clone(), l.shape == shape))
    }) else {
        return;
    };
    if same {
        // Same rows: just update their text, which also refreshes an open
        // menu on macOS. Windows draws it again only when told to.
        let labels: Vec<String> = shown.iter().map(|job| job_label(job)).collect();
        for (label, (_, item)) in labels.iter().zip(&old) {
            let _ = item.set_text(label);
        }
        #[cfg(windows)]
        repaint_rows(labels);
        return;
    }
    // An open menu on Windows keeps the layout it opened with: rows put in
    // or taken out now would show as gaps until it is opened again. Until it
    // closes (see watch), only the rows already there are updated.
    #[cfg(windows)]
    if crate::platform::tray_menu::is_open() {
        MENU_OUTDATED.store(true, Ordering::SeqCst);
        let mut labels = Vec::new();
        for (id, item) in &old {
            let Some(job) = jobs.iter().find(|j| j.id == *id) else { continue };
            let label = job_label(job);
            let _ = item.set_text(&label);
            labels.push(label);
        }
        repaint_rows(labels);
        return;
    }
    if !old.is_empty() {
        for (_, item) in &old {
            let _ = menu.remove(item);
        }
        let _ = menu.remove(&header);
        let _ = menu.remove(&separator);
    }
    let mut rows = Vec::new();
    for job in &shown {
        if let Ok(item) = MenuItem::with_id(app, format!("job:{}", job.id), job_label(job), true, None::<&str>) {
            rows.push((job.id, item));
        }
    }
    if !rows.is_empty() {
        let mut items: Vec<&dyn IsMenuItem<Wry>> = vec![&header];
        items.extend(rows.iter().map(|(_, item)| item as &dyn IsMenuItem<Wry>));
        items.push(&separator);
        let _ = menu.insert_items(&items, JOBS_AT);
    }
    if let Some(live) = LIVE.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
        live.jobs = rows;
        live.shape = shape;
    }
}

/// Windows: draws the open menu again when the job rows say something new.
#[cfg(windows)]
fn repaint_rows(labels: Vec<String>) {
    let mut drawn = ROW_TEXT.lock().unwrap_or_else(|p| p.into_inner());
    if *drawn != labels {
        *drawn = labels;
        crate::platform::tray_menu::repaint();
    }
}

/// The tray icon's tooltip on Windows, which shows no text next to tray
/// icons: "Ambar · %45 · 2 indirme" while downloading, "…" while the tray is
/// preparing a download.
#[cfg_attr(not(windows), allow(dead_code))]
fn tooltip(active: usize, fraction: f64, grabbing: bool) -> String {
    let mut text = String::from("Ambar");
    if active > 0 {
        text += &format!(" · {} · {active} indirme", percent(fraction));
    }
    if grabbing {
        text += " · …";
    }
    text
}

/// Called on every job event; coalesces bursts into one refresh.
pub fn jobs_changed(app: &AppHandle) {
    if REFRESH_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        REFRESH_PENDING.store(false, Ordering::SeqCst);
        // Menu changes wait for the main thread; keep that off the async workers.
        let _ = tauri::async_runtime::spawn_blocking(move || refresh_coalesced(&app)).await;
    });
}

/// Runs refreshes one at a time. If one is already running, it is asked to
/// go round once more instead of a second thread waiting for it.
fn refresh_coalesced(app: &AppHandle) {
    REFRESH_AGAIN.store(true, Ordering::SeqCst);
    loop {
        let Ok(_runner) = REFRESHING.try_lock() else { return };
        while REFRESH_AGAIN.swap(false, Ordering::SeqCst) {
            refresh(app);
        }
        drop(_runner);
        // A change that arrived just as the lock was released.
        if !REFRESH_AGAIN.load(Ordering::SeqCst) {
            return;
        }
    }
}

fn refresh(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let jobs = state.jobs.list();
    let active: Vec<&JobView> = jobs.iter().filter(|j| !j.status.is_finished()).collect();
    let fraction = if active.is_empty() {
        0.0
    } else {
        active
            .iter()
            .map(|j| match j.status {
                JobStatus::Downloading => j.progress.unwrap_or(0.0),
                JobStatus::Processing => 1.0,
                _ => 0.0,
            })
            .sum::<f64>()
            / active.len() as f64
    };

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        // The bucket itself shows the progress; "…" only while the menu bar
        // is reading the browser.
        #[cfg(target_os = "macos")]
        let _ = tray.set_title(GRABBING.load(Ordering::SeqCst).then_some("…"));
        #[cfg(windows)]
        let _ = tray.set_tooltip(Some(tooltip(active.len(), fraction, GRABBING.load(Ordering::SeqCst))));
        // The full bucket means "nothing running", so work in progress
        // (including converting at the end) stops one step short of it.
        let frame = if active.is_empty() { usize::MAX } else { ((fraction * 20.0).round() as usize).min(19) };
        let (key, bytes) = icon(frame);
        if SHOWN_ICON.swap(key, Ordering::SeqCst) != key {
            if let Ok(image) = Image::from_bytes(bytes) {
                let _ = tray.set_icon_with_as_template(Some(image), true);
            }
        }
        update_menu(app, &jobs);
    }

    if let Some(window) = app.get_webview_window("main") {
        let bar = if active.is_empty() {
            ProgressBarState { status: Some(ProgressBarStatus::None), progress: None }
        } else {
            ProgressBarState { status: Some(ProgressBarStatus::Normal), progress: Some((fraction * 100.0).floor() as u64) }
        };
        let _ = window.set_progress_bar(bar);
        // Windows has no badge; the tray tooltip has the count there.
        #[cfg(target_os = "macos")]
        let _ = window.set_badge_count((!active.is_empty()).then_some(active.len() as i64));
        // Everything finished while the user was elsewhere: bounce the Dock
        // icon once (flash the taskbar button on Windows), but only if
        // something that was running actually got done (restored history and
        // failed downloads don't count).
        let mut busy = BUSY_IDS.lock().unwrap_or_else(|p| p.into_inner());
        for job in &active {
            if !busy.contains(&job.id) {
                busy.push(job.id);
            }
        }
        if active.is_empty() && !busy.is_empty() {
            let done: Vec<&JobView> = jobs.iter().filter(|j| j.status == JobStatus::Done && busy.contains(&j.id)).collect();
            #[cfg(windows)]
            let failed = jobs.iter().filter(|j| j.status == JobStatus::Error && busy.contains(&j.id)).count();
            busy.clear();
            if !done.is_empty() && !window.is_focused().unwrap_or(false) {
                // A window closed to the tray has no taskbar button to
                // flash; a notification says it instead.
                #[cfg(windows)]
                if !window.is_visible().unwrap_or(true) {
                    notify_finished(app, &done, failed);
                } else {
                    let _ = window.request_user_attention(Some(UserAttentionType::Informational));
                }
                #[cfg(not(windows))]
                let _ = window.request_user_attention(Some(UserAttentionType::Informational));
            }
        }
    }
}

/// Windows: says that downloads finished while the window was in the tray.
/// Clicking the notification opens the window (see main.rs).
#[cfg(windows)]
fn notify_finished(app: &AppHandle, done: &[&JobView], failed: usize) {
    use tauri_plugin_notification::NotificationExt;
    let (title, body) = finished_text(done, failed);
    let _ = app.notification().builder().title(title).body(body).show();
}

/// That notification's title and text: the download's name when there was
/// one, otherwise how many.
#[cfg_attr(not(windows), allow(dead_code))]
fn finished_text(done: &[&JobView], failed: usize) -> (&'static str, String) {
    match (done, failed) {
        ([job], 0) => ("İndirme tamamlandı", job.title.clone()),
        (_, 0) => ("İndirmeler tamamlandı", format!("{} dosya hazır.", done.len())),
        _ => ("İndirmeler bitti", format!("{} dosya hazır, {failed} indirme başarısız oldu.", done.len())),
    }
}

pub fn show_main(app: &AppHandle) {
    WINDOW_GEN.fetch_add(1, Ordering::SeqCst);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn notify(app: &AppHandle, text: impl Into<String>, kind: &'static str) {
    show_main(app);
    let _ = app.emit("notice", Notice { text: text.into(), kind });
}

/// Handles clicks in the menu bar menu and the ⌘Q item of the app menu.
pub fn on_menu(app: &AppHandle, id: &str) {
    match id {
        QUIT => request_quit(app),
        OPEN_WINDOW => show_main(app),
        OPEN_FOLDER => {
            if let Some(state) = app.try_state::<AppState>() {
                let dir = PathBuf::from(state.settings().download_dir);
                let _ = std::fs::create_dir_all(&dir);
                let _ = app.opener().open_path(dir.to_string_lossy(), None::<&str>);
            }
        }
        GRAB_AUDIO => grab(app, Some(Kind::Mp3)),
        GRAB_VIDEO => grab(app, Some(Kind::Mp4)),
        GRAB_CHOOSE => grab(app, None),
        _ => {
            let Some(id) = id.strip_prefix("job:").and_then(|n| n.parse::<u64>().ok()) else { return };
            let Some(state) = app.try_state::<AppState>() else { return };
            // A finished download is shown in Finder (Explorer); anything
            // else in the window.
            match state.jobs.file_of(id) {
                Some(path) => {
                    let _ = app.opener().reveal_item_in_dir(path);
                }
                None => show_main(app),
            }
        }
    }
}

/// Downloads (or, with `kind` None, opens in the window) the page that is
/// open in the browser; on Windows, the copied link.
fn grab(app: &AppHandle, kind: Option<Kind>) {
    if GRABBING.swap(true, Ordering::SeqCst) {
        // Usually macOS's permission question is waiting for an answer.
        #[cfg(target_os = "macos")]
        notify(app, "Önceki istek hâlâ sürüyor. Ekranda macOS'un bir izin sorusu varsa ona yanıt ver.", "info");
        // Here only the video's details are still loading.
        #[cfg(windows)]
        notify(app, "Önceki istek hâlâ sürüyor. Birkaç saniye sonra yeniden dene.", "info");
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        #[cfg(target_os = "macos")]
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_title(Some("…"));
        }
        // Puts "…" in the tooltip.
        #[cfg(windows)]
        jobs_changed(&app);
        grab_inner(&app, kind).await;
        GRABBING.store(false, Ordering::SeqCst);
        // Also puts the "…" title back.
        jobs_changed(&app);
    });
}

async fn grab_inner(app: &AppHandle, kind: Option<Kind>) {
    let Some(state) = app.try_state::<AppState>() else { return };
    let settings = state.settings();
    if settings.accepted_notice < NOTICE_VERSION || !state.dirs.all_installed() {
        // The first-run screen comes first.
        show_main(app);
        return;
    }
    #[cfg(target_os = "macos")]
    let (url, fallback_title) = {
        let Some(browser) = browser::pick().await else {
            notify(app, BrowserError::NoBrowser.text(), "error");
            return;
        };
        let page = match browser::current_page(browser).await {
            Ok(page) => page,
            Err(e) => {
                notify(app, e.text(), "error");
                return;
            }
        };
        if ytdlp::validate_url(&page.url).is_err() {
            notify(app, "Tarayıcıdaki sayfa bir video bağlantısı değil.", "error");
            return;
        }
        let title = browser::clean_title(&page.title);
        (page.url, title)
    };
    // Read again: something else may have been copied since the menu opened.
    // There is no page title to fall back on, so the link stands in for it.
    #[cfg(windows)]
    let (url, fallback_title) = {
        let link = tauri::async_runtime::spawn_blocking(crate::clipboard::copied_link).await.ok().flatten();
        let Some(url) = link else {
            notify(app, "Kopyalanmış bir video linki bulunamadı. Videonun linkini kopyalayıp yeniden dene.", "error");
            return;
        };
        (url.clone(), url)
    };
    let Some(kind) = kind else {
        show_main(app);
        let _ = app.emit("open-url", url);
        return;
    };
    let Some(runner) = AppState::runner(&state.dirs, &state.ytdlp_cache) else {
        show_main(app);
        return;
    };
    let info = match ytdlp::fetch_info(&runner, &url, false, crate::platform::av1_hardware_decode()).await {
        Ok(MediaInfo::Video(info)) => info,
        // A playlist, a channel or an error: the window lets the user pick
        // or shows what went wrong.
        _ => {
            show_main(app);
            let _ = app.emit("open-url", url);
            return;
        }
    };
    // A song on SoundCloud has no picture to make an MP4 from, and the
    // reverse; fall back to what the page offers.
    let kind = match kind {
        Kind::Mp4 if !info.has_video => Kind::Mp3,
        Kind::Mp3 if !info.has_audio => Kind::Mp4,
        k => k,
    };
    let res = (!kind.is_audio()).then(|| {
        info.resolutions.iter().copied().find(|r| *r <= settings.default_res).or(info.resolutions.last().copied()).unwrap_or(settings.default_res)
    });
    let title = if info.title.trim().is_empty() { fallback_title } else { info.title.clone() };
    let request = DownloadRequest {
        url: info.url.clone(),
        title,
        thumbnail: info.thumbnail.clone(),
        kind,
        res,
        split_artist: false,
        audio_quality: None,
        subtitles: false,
        save_dir: None,
    };
    if settings.ask_folder {
        // Attached to the window, so the picker shows up in front even when
        // the window was closed or behind the browser.
        show_main(app);
        let mut picker = app.dialog().file().set_title("Nereye kaydedilsin?").set_directory(&settings.download_dir);
        if let Some(window) = app.get_webview_window("main") {
            picker = picker.set_parent(&window);
        }
        let app2 = app.clone();
        picker.pick_folder(move |folder| {
            let Some(folder) = folder.and_then(|f| f.into_path().ok()) else { return };
            enqueue(&app2, DownloadRequest { save_dir: Some(folder.to_string_lossy().into_owned()), ..request });
        });
    } else {
        enqueue(app, request);
    }
}

fn enqueue(app: &AppHandle, request: DownloadRequest) {
    let Some(state) = app.try_state::<AppState>() else { return };
    if let Err(e) = state.jobs.enqueue(vec![request]) {
        notify(app, e.to_string(), "error");
    }
}

/// ⌘Q and "Ambar'dan çık". They quit like the Dock does, so the same
/// question is asked while downloads are running (see platform::quit).
/// On Windows: the tray item and Ctrl+Q in the window (see ask_quit).
pub fn request_quit(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread(crate::platform::quit::request);
    }
    #[cfg(windows)]
    ask_quit(app);
    #[cfg(not(any(target_os = "macos", windows)))]
    app.exit(0);
}

/// Windows has no app-wide quit question like macOS's, so it is asked here
/// while downloads are running.
#[cfg(windows)]
fn ask_quit(app: &AppHandle) {
    let busy = busy_count(app);
    if busy == 0 {
        app.exit(0);
        return;
    }
    // Quitting again while the question is open must not ask twice.
    if QUIT_ASKING.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut question = app
        .dialog()
        .message(format!("{busy} indirme hâlâ sürüyor. Çıkarsan yarıda kalır."))
        .title("Ambar'dan çıkılsın mı?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Çık".into(), "Vazgeç".into()));
    // In front of the window when it is open. Not for a minimized window,
    // which would hide the question with it.
    if let Some(window) = app
        .get_webview_window("main")
        .filter(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
    {
        question = question.parent(&window);
    }
    let app = app.clone();
    question.show(move |quit| {
        QUIT_ASKING.store(false, Ordering::SeqCst);
        if quit {
            app.exit(0);
        }
    });
}

/// Windows: the close button only hides the window (see main.rs). The first
/// time, say where Ambar went, or it would seem to have quit with its
/// downloads still running.
#[cfg(windows)]
pub fn explain_tray_once(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else { return };
    {
        let mut settings = state.settings.lock().unwrap_or_else(|p| p.into_inner());
        if settings.tray_note_shown {
            return;
        }
        settings.tray_note_shown = true;
        // If this fails, the note simply comes once more next time.
        let _ = settings.save(&state.settings_path);
    }
    app.dialog()
        .message(
            "İndirmeler sürerken pencereyi kapatabilirsin. Ambar'yı saatin yanındaki kova simgesinden açıp \
             kapatabilirsin. Simgeyi göremiyorsan görev çubuğundaki ^ okuna bak.",
        )
        .title("Ambar arka planda çalışıyor")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCustom("Tamam".into()))
        .show(|_| {});
}

/// Downloads that are not finished yet.
pub fn busy_count(app: &AppHandle) -> usize {
    app.try_state::<AppState>().map_or(0, |s| s.jobs.list().iter().filter(|j| !j.status.is_finished()).count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64, status: JobStatus, progress: Option<f64>) -> JobView {
        JobView {
            id,
            url: "https://example.com".into(),
            title: format!("Video {id}"),
            thumbnail: None,
            kind: Kind::Mp3,
            res: None,
            status,
            progress,
            speed: None,
            eta: None,
            stage: None,
            file_path: None,
            error: None,
            attempt: 0,
            batch: 0,
        }
    }

    #[test]
    fn lists_running_then_recent() {
        let jobs = vec![
            job(1, JobStatus::Done, None),
            job(2, JobStatus::Downloading, Some(0.456)),
            job(3, JobStatus::Canceled, None),
            job(4, JobStatus::Error, None),
            job(5, JobStatus::Queued, None),
        ];
        let ids: Vec<u64> = listed(&jobs).iter().map(|j| j.id).collect();
        assert_eq!(ids, [2, 5, 4, 1]);
        assert_eq!(job_label(&jobs[1]), "%45 · Video 2");
        assert_eq!(job_label(&jobs[0]), "✓ Video 1");
        let mut song = job(6, JobStatus::Done, None);
        song.title = "Simon & Garfunkel".into();
        assert_eq!(job_label(&song), "✓ Simon && Garfunkel");
    }

    #[test]
    fn tooltip_shows_progress() {
        assert_eq!(tooltip(0, 0.0, false), "Ambar");
        assert_eq!(tooltip(2, 0.456, false), "Ambar · %45 · 2 indirme");
        assert_eq!(tooltip(0, 0.0, true), "Ambar · …");
    }

    #[test]
    fn says_what_finished() {
        let one = job(1, JobStatus::Done, None);
        let two = job(2, JobStatus::Done, None);
        assert_eq!(finished_text(&[&one], 0), ("İndirme tamamlandı", "Video 1".to_string()));
        assert_eq!(finished_text(&[&one, &two], 0), ("İndirmeler tamamlandı", "2 dosya hazır.".to_string()));
        assert_eq!(finished_text(&[&one], 1), ("İndirmeler bitti", "1 dosya hazır, 1 indirme başarısız oldu.".to_string()));
    }

    #[test]
    fn shortens_long_titles() {
        assert_eq!(short("abc", 5), "abc");
        assert_eq!(short("abcdefgh", 5), "abcd…");
        assert_eq!(short("İğüşçöİğüş", 4), "İğü…");
    }
}

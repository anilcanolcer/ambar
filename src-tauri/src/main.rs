#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod browser;
mod clipboard;
mod commands;
mod engines;
mod error;
mod history;
mod jobs;
mod platform;
mod proc;
mod settings;
mod tray;
mod update;
mod util;
mod ytdlp;

use commands::AppState;
use engines::{EngineDirs, Net};
use jobs::{JobEnv, JobManager, JobOptions};
use settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

/// Used by the development self-test (selftest.js) to report to the terminal.
#[tauri::command]
fn debug_log(line: String) {
    if cfg!(debug_assertions) {
        eprintln!("[selftest] {line}");
    }
}

/// macOS menu bar in Turkish. Predefined items keep their standard
/// shortcuts (⌘C, ⌘V ...); quitting is our own item so it can ask first
/// while downloads are running.
#[cfg(target_os = "macos")]
fn turkish_menu<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
    let quit = MenuItemBuilder::with_id(tray::QUIT, "Ambar'dan Çık").accelerator("CmdOrCtrl+Q").build(app)?;
    let app_menu = SubmenuBuilder::new(app, "Ambar")
        .about_with_text("Ambar Hakkında", None)
        .separator()
        .services_with_text("Servisler")
        .separator()
        .hide_with_text("Ambar'yı Gizle")
        .hide_others_with_text("Diğerlerini Gizle")
        .show_all_with_text("Tümünü Göster")
        .separator()
        .item(&quit)
        .build()?;
    let edit = SubmenuBuilder::new(app, "Düzen")
        .undo_with_text("Geri Al")
        .redo_with_text("Yinele")
        .separator()
        .cut_with_text("Kes")
        .copy_with_text("Kopyala")
        .paste_with_text("Yapıştır")
        .select_all_with_text("Tümünü Seç")
        .build()?;
    let window = SubmenuBuilder::new(app, "Pencere")
        .minimize_with_text("Küçült")
        .maximize_with_text("Büyüt")
        .separator()
        .close_window_with_text("Pencereyi Kapat")
        .build()?;
    MenuBuilder::new(app).items(&[&app_menu, &edit, &window]).build()
}

fn main() {
    let builder = tauri::Builder::default();
    // Must come first. Opening Ambar again (Start menu, desktop shortcut)
    // while it sits in the tray brings the running one forward instead.
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_main(app)));
    // Windows has no app menu bar; a menu there would sit inside the window.
    #[cfg(target_os = "macos")]
    let builder = builder.menu(turkish_menu);
    // Windows: says when downloads finished while the window is in the tray.
    #[cfg(windows)]
    let builder = builder.plugin(tauri_plugin_notification::init());
    builder
        // Ambar's own "Güncelle"; the web UI only reaches it through our commands.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(update::UpdateState::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .on_menu_event(|app, event| tray::on_menu(app, event.id().as_ref()))
        .on_window_event(|window, event| {
            // Closing the window keeps Ambar (and its downloads) running in the
            // menu bar, like other Mac apps; the Dock icon brings it back.
            // On Windows it stays in the tray, and the tray icon brings it back.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    #[cfg(windows)]
                    {
                        let _ = window.hide();
                        tray::explain_tray_once(window.app_handle());
                    }
                    #[cfg(target_os = "macos")]
                    if window.is_fullscreen().unwrap_or(false) {
                        // Hiding a full-screen window leaves an empty black
                        // Space behind; leave full screen first. Showing the
                        // window again in the meantime cancels the hide.
                        let _ = window.set_fullscreen(false);
                        let generation = tray::WINDOW_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                        let window = window.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(900));
                            // Checked on the main thread, so a show can't slip in between.
                            let w = window.clone();
                            let _ = window.run_on_main_thread(move || {
                                if tray::WINDOW_GEN.load(std::sync::atomic::Ordering::SeqCst) == generation {
                                    let _ = w.hide();
                                }
                            });
                        });
                    } else {
                        let _ = window.hide();
                    }
                }
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let paths = app.path();
            // The same folder as app_data_dir on macOS; on Windows it keeps
            // the engines (~250 MB) out of the roaming profile.
            let data_dir = paths.app_local_data_dir()?;
            #[cfg(all(windows, not(debug_assertions)))]
            platform::log_to_file(&data_dir.join("kova.log"));
            // ~/Library/Logs/com.alp.downloader, where Console.app looks.
            #[cfg(all(target_os = "macos", not(debug_assertions)))]
            if let Ok(dir) = paths.app_log_dir() {
                platform::log_to_file(&dir.join("kova.log"));
            }
            let cache_dir = paths.app_cache_dir()?;
            let downloads = paths
                .download_dir()
                .or_else(|_| paths.home_dir().map(|h| h.join("Downloads")))?;
            let default_download_dir = downloads.join("Ambar");

            let settings_path = data_dir.join("settings.json");
            let settings = Arc::new(Mutex::new(Settings::load(&settings_path, &default_download_dir)));
            // Half-copied files from a quit during a copy to another disk.
            {
                let dir = PathBuf::from(&settings.lock().unwrap_or_else(|p| p.into_inner()).download_dir);
                jobs::remove_part_files(Some(&dir));
            }
            let dirs = EngineDirs::new(data_dir.join("engines"));
            let ytdlp_cache = cache_dir.join("yt-dlp");
            let temp_root = cache_dir.join("jobs");
            // Leftovers from a previous session that was killed mid-download.
            util::remove_dir_quiet(&temp_root);
            std::fs::create_dir_all(&temp_root)?;
            std::fs::create_dir_all(&ytdlp_cache)?;
            // No download can be running yet, so old engine versions can go.
            dirs.remove_old_versions();

            let history = Arc::new(history::History::load(&data_dir.join("history.json")));
            let env = JobEnv {
                emit: {
                    let handle = handle.clone();
                    Box::new(move |event| {
                        let _ = handle.emit("job-event", event);
                        tray::jobs_changed(&handle);
                    })
                },
                runner: {
                    let dirs = dirs.clone();
                    let cache = ytdlp_cache.clone();
                    Box::new(move || AppState::runner(&dirs, &cache))
                },
                options: {
                    let settings = Arc::clone(&settings);
                    Box::new(move || {
                        let s = settings.lock().unwrap_or_else(|p| p.into_inner());
                        JobOptions {
                            download_dir: PathBuf::from(&s.download_dir),
                            square_cover: s.square_cover,
                            max_concurrent: s.max_concurrent as usize,
                            prefer_av1: platform::av1_hardware_decode(),
                        }
                    })
                },
                temp_root,
            };

            let jobs = Arc::new(JobManager::new(env));
            jobs.set_on_done({
                let history = Arc::clone(&history);
                move |job| {
                    if let Some(entry) = history::Entry::from_job(job) {
                        history.add(entry);
                    }
                }
            });
            jobs.restore(history.present());
            app.manage(AppState {
                dirs,
                net: Net::new()?,
                ytdlp_cache,
                settings_path,
                default_download_dir,
                settings,
                engine_busy: AtomicBool::new(false),
                jobs,
                history,
            });
            tray::create(app.handle())?;
            // Lists the restored downloads in the menu bar menu right away.
            tray::jobs_changed(app.handle());
            // Clicking the "downloads finished" notification opens the window.
            #[cfg(windows)]
            {
                use tauri_plugin_notification::NotificationExt;
                let handle = app.handle().clone();
                let _ = app.notification().on_action(move |action| {
                    if action.action_id() == "tap" {
                        tray::show_main(&handle);
                    }
                });
            }
            #[cfg(target_os = "macos")]
            {
                let handle = app.handle().clone();
                platform::quit::install(move || tray::busy_count(&handle));
            }

            // Development check of quitting from the menu while idle: must exit.
            #[cfg(debug_assertions)]
            if std::env::var("KOVA_SELFTEST").as_deref() == Ok("quit-idle") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    eprintln!("[selftest] quitting from the menu");
                    tray::on_menu(&handle, tray::QUIT);
                });
            }

            // Development check of the quit question: start a download, then quit.
            #[cfg(debug_assertions)]
            if std::env::var("KOVA_SELFTEST").as_deref() == Ok("quit") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    let state = handle.state::<AppState>();
                    let request = ytdlp::DownloadRequest {
                        url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
                        title: "Big Buck Bunny".into(),
                        thumbnail: None,
                        kind: settings::Kind::Mp3,
                        res: None,
                        split_artist: false,
                        audio_quality: None,
                        subtitles: false,
                        save_dir: None,
                    };
                    eprintln!("[selftest] enqueue: {:?}", state.jobs.enqueue(vec![request]).map(|v| v.len()));
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    eprintln!("[selftest] busy before quit: {}", tray::busy_count(&handle));
                    tray::request_quit(&handle);
                });
            }

            // Development check of the app update: against a test endpoint
            // serving a newer build, check and install it the way the
            // Güncelle button does; Ambar must come back as the new version.
            #[cfg(debug_assertions)]
            if std::env::var("KOVA_SELFTEST").as_deref() == Ok("update") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(4));
                    eprintln!("[selftest] running version {}", handle.package_info().version);
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.eval(
                            r#"(async () => {
                              const { invoke } = window.__TAURI_INTERNALS__;
                              const log = (line) => invoke("debug_log", { line });
                              try {
                                const update = await invoke("check_app_update");
                                await log("update check: " + JSON.stringify(update));
                                if (update) await invoke("install_app_update");
                              } catch (e) {
                                await log("update error: " + e);
                              }
                              await log("SELFTEST DONE");
                            })()"#,
                        );
                    }
                });
            }

            #[cfg(debug_assertions)]
            if std::env::var("KOVA_SELFTEST").as_deref() == Ok("1") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(4));
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.eval(include_str!("../selftest.js"));
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::accept_notice,
            commands::install_engines,
            commands::update_engines,
            commands::engines_state,
            commands::fetch_info,
            commands::enqueue,
            commands::cancel_job,
            commands::retry_job,
            commands::remove_job,
            commands::clear_finished,
            commands::save_settings,
            commands::reveal_job_file,
            commands::open_job_file,
            commands::open_download_dir,
            commands::open_link,
            commands::quit_app,
            update::check_app_update,
            update::install_app_update,
            debug_log,
        ])
        .build(tauri::generate_context!())
        .expect("Ambar başlatılamadı")
        .run(|app, event| match event {
            tauri::RunEvent::Exit => {
                #[cfg(debug_assertions)]
                eprintln!("[selftest] exit cleanup");
                if let Some(state) = app.try_state::<AppState>() {
                    state.jobs.kill_all();
                }
                jobs::remove_part_files(None);
            }
            // Clicking the Dock icon while the window is closed.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => tray::show_main(app),
            _ => {}
        });
}

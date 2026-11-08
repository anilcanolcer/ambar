//! Small platform capability checks: macOS frameworks, the Windows taskbar
//! and tray menu.

use std::path::Path;
use std::sync::OnceLock;

#[cfg(target_os = "macos")]
#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    // Boolean VTIsHardwareDecodeSupported(CMVideoCodecType codecType)
    fn VTIsHardwareDecodeSupported(codec_type: u32) -> u8;
}

/// True when this Mac decodes AV1 in hardware (Apple M3 and newer). Then
/// QuickTime plays AV1 files, so 2K/4K downloads can use AV1, which is also
/// about half the size of VP9 at the same quality. Always false elsewhere.
pub fn av1_hardware_decode() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        #[cfg(target_os = "macos")]
        {
            const AV1: u32 = u32::from_be_bytes(*b"av01");
            unsafe { VTIsHardwareDecodeSupported(AV1) != 0 }
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    })
}

/// Asks before quitting while downloads are running, whichever way the user
/// quits: ⌘Q, the menu bar icon, the Dock's "Çık", logging out or shutting
/// down. All of these reach `applicationShouldTerminate:`, which tao's app
/// delegate does not implement, so it is added to that class here.
#[cfg(target_os = "macos")]
pub mod quit {
    use objc2::ffi::{class_addMethod, object_getClass};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{class, msg_send, sel};
    use std::ffi::CString;
    use std::sync::OnceLock;

    type BusyCount = Box<dyn Fn() -> usize + Send + Sync>;
    static BUSY: OnceLock<BusyCount> = OnceLock::new();

    // NSApplicationTerminateReply and NSAlertFirstButtonReturn.
    const TERMINATE_CANCEL: usize = 0;
    const TERMINATE_NOW: usize = 1;
    const FIRST_BUTTON: isize = 1000;

    pub(super) fn ns_string(text: &str) -> Option<Retained<AnyObject>> {
        let c = CString::new(text.replace('\0', "")).ok()?;
        unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
    }

    extern "C-unwind" fn should_terminate(_this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> usize {
        let busy = BUSY.get().map_or(0, |count| count());
        if busy == 0 {
            return TERMINATE_NOW;
        }
        let (Some(title), Some(text), Some(quit), Some(stay)) = (
            ns_string("Ambar'dan çıkılsın mı?"),
            ns_string(&format!("{busy} indirme hâlâ sürüyor. Çıkarsan yarıda kalır.")),
            ns_string("Çık"),
            ns_string("Vazgeç"),
        ) else {
            return TERMINATE_NOW;
        };
        unsafe {
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![ns_app, activateIgnoringOtherApps: Bool::YES];
            let Some(alert): Option<Retained<AnyObject>> = msg_send![class!(NSAlert), new] else {
                return TERMINATE_NOW;
            };
            let _: () = msg_send![&*alert, setMessageText: &*title];
            let _: () = msg_send![&*alert, setInformativeText: &*text];
            let _: *mut AnyObject = msg_send![&*alert, addButtonWithTitle: &*quit];
            let _: *mut AnyObject = msg_send![&*alert, addButtonWithTitle: &*stay];
            let response: isize = msg_send![&*alert, runModal];
            // ⌘Q pressed again while the question was open must not ask again.
            cancel_queued(ns_app);
            if response == FIRST_BUTTON {
                TERMINATE_NOW
            } else {
                TERMINATE_CANCEL
            }
        }
    }

    /// `busy` returns how many downloads are still running. Call on the main
    /// thread once the app has started.
    pub fn install(busy: impl Fn() -> usize + Send + Sync + 'static) {
        if BUSY.set(Box::new(busy)).is_err() {
            return;
        }
        unsafe {
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let delegate: *mut AnyObject = msg_send![ns_app, delegate];
            if delegate.is_null() {
                return;
            }
            let class = object_getClass(delegate) as *mut AnyClass;
            let imp: Imp = std::mem::transmute(should_terminate as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize);
            // NSUInteger return, then self, _cmd and the sender.
            let added = class_addMethod(class, sel!(applicationShouldTerminate:), imp, c"Q@:@".as_ptr());
            if cfg!(debug_assertions) {
                eprintln!("[kova] quit question installed: {}", added.as_bool());
            }
        }
    }

    /// Drops quit requests queued by `request` that have not run yet.
    unsafe fn cancel_queued(ns_app: *mut AnyObject) {
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: ns_app,
            selector: sel!(terminate:),
            object: std::ptr::null_mut::<AnyObject>()
        ];
    }

    /// Quits the way the Dock does, so the question above is asked. Main
    /// thread only. The quit is queued on the run loop: sent straight from a
    /// menu click, which tao is still handling, its shutdown would deadlock.
    pub fn request() {
        unsafe {
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            // Several quick requests become one.
            cancel_queued(ns_app);
            let _: () = msg_send![
                ns_app,
                performSelector: sel!(terminate:),
                withObject: std::ptr::null_mut::<AnyObject>(),
                afterDelay: 0.0f64
            ];
        }
    }
}

/// While downloads run, keeps the Mac from going to sleep on its own and
/// keeps App Nap from slowing Ambar down once its window is closed. The
/// display may still turn off, and closing the lid still puts the Mac to
/// sleep. Cheap to call again with the same value.
#[cfg(target_os = "macos")]
pub fn keep_awake(on: bool) {
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use std::sync::Mutex;

    struct Activity(Retained<AnyObject>);
    // SAFETY: the token is only handed back to NSProcessInfo, which may be
    // used from any thread.
    unsafe impl Send for Activity {}
    static ACTIVITY: Mutex<Option<Activity>> = Mutex::new(None);
    // NSActivityUserInitiated, which includes NSActivityIdleSystemSleepDisabled.
    const USER_INITIATED: u64 = 0x00FF_FFFF;

    let mut held = ACTIVITY.lock().unwrap_or_else(|p| p.into_inner());
    if on == held.is_some() {
        return;
    }
    autoreleasepool(|_| unsafe {
        let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        if on {
            // Shown by `pmset -g assertions`, which mangles non-ASCII text.
            let Some(reason) = quit::ns_string("Ambar is downloading") else { return };
            let token: Option<Retained<AnyObject>> = msg_send![info, beginActivityWithOptions: USER_INITIATED, reason: &*reason];
            *held = token.map(Activity);
        } else if let Some(Activity(token)) = held.take() {
            let _: () = msg_send![info, endActivity: &*token];
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub fn keep_awake(_on: bool) {}

/// The static FFmpeg build cannot find the system certificate store on its
/// own; macOS keeps a PEM bundle here. The Windows build uses the system's
/// store itself.
pub fn ca_bundle() -> Option<&'static Path> {
    if cfg!(not(target_os = "macos")) {
        return None;
    }
    let path = Path::new("/etc/ssl/cert.pem");
    path.is_file().then_some(path)
}

/// True when the Windows taskbar is light (Settings › Personalization ›
/// Colors). The tray icon must then be dark, and the reverse. Read anew on
/// every call, since the user can switch at any time; older Windows 10
/// without the setting has a dark taskbar.
#[cfg(windows)]
pub fn light_taskbar() -> bool {
    use windows_sys::core::w;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: both names are NUL-terminated literals, and `value` and `size`
    // describe a DWORD buffer that outlives the call. No key is left open.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    status == ERROR_SUCCESS && value == 1
}

/// Windows: the open tray menu. Windows lays a menu out once, when it opens,
/// and changes to its items show only where it happens to be drawn again.
#[cfg(windows)]
pub mod tray_menu {
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::Graphics::Gdi::InvalidateRect;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumThreadWindows, GetClassNameW, GetGUIThreadInfo, IsWindowVisible, GUITHREADINFO, GUI_POPUPMENUMODE,
    };

    /// The thread that shows the menu: the one the tray icon was made on.
    static THREAD: AtomicU32 = AtomicU32::new(0);

    /// Call on the thread that creates the tray icon.
    pub fn remember_thread() {
        // SAFETY: takes nothing.
        THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    }

    /// True while the tray menu is open.
    pub fn is_open() -> bool {
        let thread = THREAD.load(Ordering::SeqCst);
        if thread == 0 {
            return false;
        }
        let mut info = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        // SAFETY: `info` is a GUITHREADINFO with cbSize set, as required.
        unsafe { GetGUIThreadInfo(thread, &mut info) != 0 && info.flags & GUI_POPUPMENUMODE != 0 }
    }

    /// Draws the open menu again, so changed item text shows at once.
    pub fn repaint() {
        unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> BOOL {
            // Menus open in a window of the system class "#32768".
            let mut class = [0u16; 16];
            // SAFETY: `hwnd` comes from the enumeration; the buffer length
            // is passed along.
            let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
            let is_menu = class.get(..len.max(0) as usize).is_some_and(|name| String::from_utf16_lossy(name) == "#32768");
            // SAFETY: as above.
            if is_menu && unsafe { IsWindowVisible(hwnd) } != 0 {
                // SAFETY: a null rectangle means the whole window.
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 1) };
            }
            // Go on with the next window.
            1
        }
        if !is_open() {
            return;
        }
        // SAFETY: `each` matches WNDENUMPROC and keeps nothing after it returns.
        unsafe { EnumThreadWindows(THREAD.load(Ordering::SeqCst), Some(each), 0) };
    }
}

/// A release build writes standard error nowhere (no console on Windows, and
/// a Mac app opened from Finder has none either), so what Ambar prints for
/// troubleshooting ("[kova] ..." lines, a failed download's yt-dlp output)
/// goes to this file instead, which can be sent along with a bug report.
/// It starts over once it is larger than 1 MB. Development runs keep
/// printing to the terminal.
#[cfg(not(debug_assertions))]
pub fn log_to_file(path: &Path) {
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(path));
    let fresh = std::fs::metadata(path).map_or(true, |m| m.len() > 1_000_000);
    let mut options = std::fs::OpenOptions::new();
    options.create(true);
    if fresh {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    let Ok(file) = options.open(path) else { return };
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
        // SAFETY: the handle is an open file that is never closed, so it stays
        // valid as standard error until Ambar exits. Rust's stderr looks the
        // handle up on every write, so all later output lands in the file.
        unsafe { SetStdHandle(STD_ERROR_HANDLE, file.into_raw_handle()) };
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: both are open descriptors; dup2 makes 2 a second handle on
        // the file, which stays open after `file` is dropped.
        unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) };
    }
    eprintln!(
        "[kova] {} started at {} ({} {})",
        env!("CARGO_PKG_VERSION"),
        crate::util::log_time(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn capability_probe_does_not_crash() {
        println!("av1 hardware decode: {}", super::av1_hardware_decode());
        #[cfg(target_os = "macos")]
        assert!(super::ca_bundle().is_some(), "macOS should ship /etc/ssl/cert.pem");
        #[cfg(windows)]
        println!("light taskbar: {}", super::light_taskbar());
    }
}

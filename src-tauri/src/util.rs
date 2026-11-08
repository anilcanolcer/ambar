use crate::error::AppResult;
use serde::Serialize;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Short Turkish description of a file system error.
pub fn io_text(e: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match e.kind() {
        StorageFull => "Diskte yer kalmadı.",
        PermissionDenied => "Bu klasöre yazma izni yok.",
        NotFound => "Dosya ya da klasör bulunamadı.",
        ReadOnlyFilesystem => "Bu disk salt okunur.",
        AlreadyExists => "Aynı adda bir dosya zaten var.",
        _ => e.raw_os_error().and_then(os_error_text).unwrap_or("Dosya işlemi başarısız oldu."),
    }
}

/// Raw error codes worth naming that the error kind may not cover.
#[cfg(unix)]
fn os_error_text(code: i32) -> Option<&'static str> {
    match code {
        libc::ENOSPC => Some("Diskte yer kalmadı."),
        libc::EACCES | libc::EPERM => Some("Bu klasöre yazma izni yok."),
        _ => None,
    }
}

/// Raw error codes worth naming that the error kind may not cover. On
/// Windows these are Win32 error codes, not errno values.
#[cfg(windows)]
fn os_error_text(code: i32) -> Option<&'static str> {
    use windows_sys::Win32::Foundation::{
        ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL, ERROR_VIRUS_DELETED, ERROR_VIRUS_INFECTED, ERROR_WRITE_PROTECT,
    };
    match code as u32 {
        ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL => Some("Diskte yer kalmadı."),
        ERROR_WRITE_PROTECT => Some("Bu disk salt okunur."),
        ERROR_VIRUS_INFECTED | ERROR_VIRUS_DELETED => Some("Windows Güvenliği bu dosyayı engelledi."),
        _ => None,
    }
}

/// Why a folder could not be created. A folder on a disk that is not
/// connected would otherwise look like a permission problem (/Volumes itself
/// is not writable) or, on Windows, like a folder that is simply missing.
pub fn dir_error(path: &Path, e: &std::io::Error) -> String {
    unplugged_disk(path).unwrap_or_else(|| io_text(e).to_string())
}

#[cfg(not(windows))]
fn unplugged_disk(path: &Path) -> Option<String> {
    let mut parts = path.components();
    if let (Some(std::path::Component::RootDir), Some(volumes), Some(disk)) = (parts.next(), parts.next(), parts.next()) {
        if volumes.as_os_str() == "Volumes" && !Path::new("/Volumes").join(disk.as_os_str()).exists() {
            return Some(format!("\"{}\" diski bağlı değil. Diski tak ya da başka bir klasör seç.", disk.as_os_str().to_string_lossy()));
        }
    }
    None
}

/// A drive letter that is not there (an unplugged USB disk) or a network
/// share that cannot be reached.
#[cfg(windows)]
fn unplugged_disk(path: &Path) -> Option<String> {
    use std::path::{Component, Prefix};
    let Some(Component::Prefix(prefix)) = path.components().next() else { return None };
    match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            let drive = format!("{}:", letter.to_ascii_uppercase() as char);
            (!Path::new(&format!("{drive}\\")).exists())
                .then(|| format!("\"{drive}\" sürücüsü bağlı değil. Diski tak ya da başka bir klasör seç."))
        }
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            let share = format!("\\\\{}\\{}", server.to_string_lossy(), share.to_string_lossy());
            (!Path::new(&format!("{share}\\")).exists())
                .then(|| format!("\"{share}\" ağ konumuna ulaşılamıyor. Bağlantını kontrol et ya da başka bir klasör seç."))
        }
        _ => None,
    }
}

/// std::fs::rename, retried for about a second on Windows, where Defender
/// or the search indexer often holds a file that was just written open for
/// a moment. Blocks while it waits.
pub fn rename_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
        for _ in 0..5 {
            match std::fs::rename(from, to) {
                Err(e) if matches!(e.raw_os_error().map(|c| c as u32), Some(ERROR_SHARING_VIOLATION | ERROR_ACCESS_DENIED)) => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                other => return other,
            }
        }
    }
    std::fs::rename(from, to)
}

/// A move within one NTFS disk keeps the file's own permissions, so a file
/// moved out of AppData would stay readable only by this user even in a
/// shared or Public folder. This makes it take what the new folder passes
/// down instead, as a copied file would. Best effort; errors are ignored.
#[cfg(windows)]
pub fn inherit_folder_permissions(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::{
        InitializeAcl, ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, PSID, UNPROTECTED_DACL_SECURITY_INFORMATION,
    };
    // windows-sys only has this behind a feature Ambar does not enable, so it
    // is declared here the way windows-sys declares it.
    #[link(name = "advapi32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
    extern "system" {
        fn SetNamedSecurityInfoW(name: *const u16, kind: i32, info: u32, owner: PSID, group: PSID, dacl: *const ACL, sacl: *const ACL) -> u32;
    }
    const SE_FILE_OBJECT: i32 = 1;
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    // A folder that passes nothing down would leave the file with no
    // entries at all, readable by no one; it then keeps what it has.
    match path.parent() {
        Some(dir) if passes_down_to_files(&wide(dir)) => {}
        _ => return,
    }
    // No entries of its own: only the ones inherited from the new folder remain.
    let mut acl = ACL::default();
    unsafe {
        if InitializeAcl(&mut acl, std::mem::size_of::<ACL>() as u32, ACL_REVISION) != 0 {
            let info = DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION;
            SetNamedSecurityInfoW(wide(path).as_ptr(), SE_FILE_OBJECT, info, std::ptr::null_mut(), std::ptr::null_mut(), &acl, std::ptr::null());
        }
    }
}

/// The folder's permissions include an entry that new files inherit.
/// `dir` is a NUL-terminated wide path.
#[cfg(windows)]
fn passes_down_to_files(dir: &[u16]) -> bool {
    use windows_sys::Win32::Security::{
        GetAce, GetFileSecurityW, GetSecurityDescriptorDacl, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE,
    };
    let mut needed = 0u32;
    unsafe {
        // The first call only reports the size.
        GetFileSecurityW(dir.as_ptr(), DACL_SECURITY_INFORMATION, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return false;
        }
        // u64 keeps the descriptor suitably aligned.
        let mut sd = vec![0u64; (needed as usize).div_ceil(8)];
        if GetFileSecurityW(dir.as_ptr(), DACL_SECURITY_INFORMATION, sd.as_mut_ptr().cast(), needed, &mut needed) == 0 {
            return false;
        }
        let (mut present, mut defaulted, mut dacl) = (0, 0, std::ptr::null_mut::<ACL>());
        // No DACL at all (everyone may do anything) passes nothing down either.
        if GetSecurityDescriptorDacl(sd.as_mut_ptr().cast(), &mut present, &mut dacl, &mut defaulted) == 0 || present == 0 || dacl.is_null() {
            return false;
        }
        (0..u32::from((*dacl).AceCount)).any(|i| {
            let mut ace = std::ptr::null_mut();
            GetAce(dacl, i, &mut ace) != 0 && u32::from((*ace.cast::<ACE_HEADER>()).AceFlags) & OBJECT_INHERIT_ACE != 0
        })
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The current time for kova.log, as "2026-11-02 00:31:15 UTC".
pub fn log_time() -> String {
    utc_text(now_secs())
}

fn utc_text(secs: u64) -> String {
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's civil_from_days).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", rest / 3600, rest / 60 % 60, rest % 60)
}

/// Writes JSON next to the target and renames it into place so a crash never
/// leaves a half-written settings or manifest file behind.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> AppResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Removes a directory tree, ignoring "not found".
pub fn remove_dir_quiet(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_times() {
        assert_eq!(utc_text(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_text(1_793_012_632), "2026-10-26 11:03:52 UTC");
        assert_eq!(utc_text(951_782_400), "2000-02-29 00:00:00 UTC");
    }

    #[cfg(unix)]
    #[test]
    fn names_unplugged_disks() {
        let denied = std::io::Error::from_raw_os_error(libc::EACCES);
        assert_eq!(dir_error(Path::new("/Volumes/AmbarYokDisk/Videolar"), &denied), "\"AmbarYokDisk\" diski bağlı değil. Diski tak ya da başka bir klasör seç.");
        assert_eq!(dir_error(Path::new("/Users/x/Downloads"), &denied), "Bu klasöre yazma izni yok.");
    }

    #[cfg(windows)]
    #[test]
    fn names_unplugged_drives_and_shares() {
        use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
        let denied = std::io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32);
        let missing = (b'D'..=b'Z').rev().find(|l| !Path::new(&format!("{}:\\", *l as char)).exists()).expect("a free drive letter");
        let folder = format!("{}:\\Videolar", (missing as char).to_ascii_lowercase());
        let drive = format!("{}:", missing as char);
        assert_eq!(dir_error(Path::new(&folder), &denied), format!("\"{drive}\" sürücüsü bağlı değil. Diski tak ya da başka bir klasör seç."));
        // .invalid never resolves (RFC 2606).
        assert_eq!(
            dir_error(Path::new(r"\\kova-yok.invalid\paylasim\Videolar"), &denied),
            "\"\\\\kova-yok.invalid\\paylasim\" ağ konumuna ulaşılamıyor. Bağlantını kontrol et ya da başka bir klasör seç."
        );
        assert_eq!(dir_error(Path::new(r"C:\Users\x\Downloads"), &denied), "Bu klasöre yazma izni yok.");
    }

    #[cfg(windows)]
    #[test]
    fn names_windows_error_codes() {
        let text = |code: u32| io_text(&std::io::Error::from_raw_os_error(code as i32));
        assert_eq!(text(112), "Diskte yer kalmadı."); // ERROR_DISK_FULL
        assert_eq!(text(39), "Diskte yer kalmadı."); // ERROR_HANDLE_DISK_FULL
        assert_eq!(text(19), "Bu disk salt okunur."); // ERROR_WRITE_PROTECT
        assert_eq!(text(5), "Bu klasöre yazma izni yok."); // ERROR_ACCESS_DENIED
        assert_eq!(text(225), "Windows Güvenliği bu dosyayı engelledi."); // ERROR_VIRUS_INFECTED
    }
}

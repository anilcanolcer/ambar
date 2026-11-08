//! The copied link, for the tray icon on Windows: there the icon offers to
//! download a video link from the clipboard instead of reading the browser.

// Only the tray on Windows reads the clipboard; the parsing is tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::ytdlp;

/// Sites whose links are often copied without "https://".
const BARE_HOSTS: [&str; 4] = ["youtube.com/", "youtu.be/", "vimeo.com/", "soundcloud.com/"];

/// The first web link in `text`, cleaned up like the window's paste box does
/// (extractUrl in src/format.ts). A bare "youtube.com/…" gets "https://".
pub fn find_link(text: &str) -> Option<String> {
    let text = text.trim();
    let url = match first_address(text) {
        Some(found) => trim_link(found),
        None => bare_link(text)?,
    };
    // "https://" alone, left over after trimming, is no link.
    if !matches!(url.split_once("://"), Some((_, rest)) if !rest.is_empty()) {
        return None;
    }
    ytdlp::validate_url(&url).ok()
}

/// Where the first "http://" or "https://" starts, up to a space, "<", ">"
/// or a double quote.
fn first_address(text: &str) -> Option<&str> {
    // ASCII lowercasing keeps every byte offset, so they index `text` too.
    let lower = text.to_ascii_lowercase();
    lower.match_indices("http").find_map(|(start, _)| {
        let rest = &lower[start + 4..];
        let scheme = if rest.starts_with("s://") {
            8
        } else if rest.starts_with("://") {
            7
        } else {
            return None;
        };
        let end = text[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"'))
            .map_or(text.len(), |len| start + len);
        (end > start + scheme).then(|| &text[start..end])
    })
}

/// "[text](link)" Markdown, then sentence punctuation or a closing quote
/// stuck to the end; ")" only when it closes nothing.
fn trim_link(found: &str) -> String {
    let url = found.split("](").next().unwrap_or(found);
    let mut url = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'', '"']);
    while url.ends_with(')') && url.matches('(').count() < url.matches(')').count() {
        url = &url[..url.len() - 1];
    }
    url.to_string()
}

/// "youtube.com/watch?v=…" (also with "www.", "m." or "music.") -> https.
fn bare_link(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let host = ["www.", "m.", "music."].iter().find_map(|p| lower.strip_prefix(p)).unwrap_or(&lower);
    let path = BARE_HOSTS.iter().find_map(|h| host.strip_prefix(h))?;
    (!path.is_empty() && !path.contains(char::is_whitespace)).then(|| format!("https://{text}"))
}

/// "https://www.youtube.com/watch?v=…" -> "youtube.com", for the menu.
pub fn site(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    // Drop "user@" and ":port".
    let host = host.rsplit('@').next()?.split(':').next()?;
    let host = host.strip_prefix("www.").unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

#[cfg(windows)]
pub use self::win32::{copied_link, holds_document, read_link, sequence};

#[cfg(windows)]
mod win32 {
    use std::sync::OnceLock;
    use std::time::Duration;
    use windows_sys::core::w;
    use windows_sys::Win32::Foundation::HGLOBAL;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
        RegisterClipboardFormatW,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    /// A link is near the start; a whole copied book need not be read.
    const MAX_UNITS: usize = 64 * 1024;

    /// The clipboard could not be read just now: another program had it
    /// open, or the program that copied did not hand over its text.
    #[derive(Debug)]
    pub struct Unreadable;

    /// Changes whenever anything is copied, so it is cheap to watch.
    pub fn sequence() -> u32 {
        // SAFETY: takes nothing and only reads a counter.
        unsafe { GetClipboardSequenceNumber() }
    }

    /// The video link on the clipboard, if there is one. A clipboard that
    /// can't be read counts as none.
    pub fn copied_link() -> Option<String> {
        read_link().ok().flatten()
    }

    /// Like `copied_link`, but tells a clipboard that can't be read apart
    /// from one without a link, so it can be tried again.
    pub fn read_link() -> Result<Option<String>, Unreadable> {
        Ok(text()?.and_then(|text| super::find_link(&text)))
    }

    /// True when the clipboard also holds an Office document, a spreadsheet
    /// or rich text. Word and Excel make the plain text of a copy only when
    /// a program asks for it, all of it at once, which can hold them up for
    /// seconds after a big copy; a look in the background must not do that.
    pub fn holds_document() -> bool {
        static FORMATS: OnceLock<Vec<u32>> = OnceLock::new();
        let formats = FORMATS.get_or_init(|| {
            [w!("Biff12"), w!("Biff8"), w!("XML Spreadsheet"), w!("Rich Text Format"), w!("Embed Source"), w!("Object Descriptor")]
                .into_iter()
                // SAFETY: each name is a NUL-terminated literal. Registering
                // a name only looks up (or adds) its format number.
                .map(|name| unsafe { RegisterClipboardFormatW(name) })
                .filter(|&format| format != 0)
                .collect()
        });
        // SAFETY: only asks which formats are on the clipboard.
        formats.iter().any(|&format| unsafe { IsClipboardFormatAvailable(format) } != 0)
    }

    /// Closes the clipboard when dropped.
    struct Opened;

    impl Drop for Opened {
        fn drop(&mut self) {
            // SAFETY: only created after OpenClipboard succeeded on this thread.
            unsafe { CloseClipboard() };
        }
    }

    /// Unlocks the clipboard text when dropped.
    struct Locked(HGLOBAL);

    impl Drop for Locked {
        fn drop(&mut self) {
            // SAFETY: only created after GlobalLock succeeded on this handle.
            unsafe { GlobalUnlock(self.0) };
        }
    }

    fn open() -> Option<Opened> {
        // Another program may hold the clipboard for a moment while it copies.
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(20));
            }
            // SAFETY: no owner window is needed for reading; closed by `Opened`.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(Opened);
            }
        }
        None
    }

    /// The copied text; None when nothing textual was copied.
    fn text() -> Result<Option<String>, Unreadable> {
        // SAFETY: only asks whether text is on the clipboard.
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.into()) } == 0 {
            return Ok(None);
        }
        // Dropped in reverse order: unlocked before the clipboard is closed,
        // which is when the handle stops being valid.
        let _opened = open().ok_or(Unreadable)?;
        // SAFETY: the clipboard is open; the handle belongs to it.
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT.into()) };
        if handle.is_null() {
            return Err(Unreadable);
        }
        // SAFETY: CF_UNICODETEXT data is a global memory handle.
        let data = unsafe { GlobalLock(handle) }.cast::<u16>();
        if data.is_null() {
            return Err(Unreadable);
        }
        let _locked = Locked(handle);
        // SAFETY: the locked block is GlobalSize bytes long and suitably
        // aligned, and it stays locked while the slice is used.
        let units = unsafe { std::slice::from_raw_parts(data, (GlobalSize(handle) / 2).min(MAX_UNITS)) };
        // The text ends at its NUL, which may come before the block's end.
        let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        Ok(Some(String::from_utf16_lossy(&units[..len])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links_in_text() {
        assert_eq!(find_link("  https://youtu.be/aqz-KE-bpKQ \n").as_deref(), Some("https://youtu.be/aqz-KE-bpKQ"));
        assert_eq!(
            find_link("İzle: https://www.youtube.com/watch?v=aqz-KE-bpKQ, çok iyi.").as_deref(),
            Some("https://www.youtube.com/watch?v=aqz-KE-bpKQ")
        );
        assert_eq!(find_link("HTTPS://Vimeo.com/76979871.").as_deref(), Some("HTTPS://Vimeo.com/76979871"));
        assert_eq!(find_link("\"http://example.com/a\"").as_deref(), Some("http://example.com/a"));
        assert_eq!(find_link("<https://example.com/a>").as_deref(), Some("https://example.com/a"));
        assert_eq!(find_link("ftp://x https://example.com/b").as_deref(), Some("https://example.com/b"));
        assert_eq!(find_link("httpx https://example.com/c").as_deref(), Some("https://example.com/c"));
    }

    #[test]
    fn trims_like_the_paste_box() {
        assert_eq!(find_link("[video](https://youtu.be/abc)").as_deref(), Some("https://youtu.be/abc"));
        assert_eq!(find_link("(bkz. https://youtu.be/abc)").as_deref(), Some("https://youtu.be/abc"));
        assert_eq!(
            find_link("https://en.wikipedia.org/wiki/Big_Buck_Bunny_(film)").as_deref(),
            Some("https://en.wikipedia.org/wiki/Big_Buck_Bunny_(film)")
        );
        assert_eq!(find_link("'https://youtu.be/abc'!").as_deref(), Some("https://youtu.be/abc"));
    }

    #[test]
    fn adds_https_to_bare_links() {
        assert_eq!(find_link("youtube.com/watch?v=abc").as_deref(), Some("https://youtube.com/watch?v=abc"));
        assert_eq!(find_link("www.youtube.com/watch?v=abc").as_deref(), Some("https://www.youtube.com/watch?v=abc"));
        assert_eq!(find_link("music.youtube.com/watch?v=abc").as_deref(), Some("https://music.youtube.com/watch?v=abc"));
        assert_eq!(find_link("YOUTU.BE/abc").as_deref(), Some("https://YOUTU.BE/abc"));
        assert_eq!(find_link("soundcloud.com/artist/song").as_deref(), Some("https://soundcloud.com/artist/song"));
        assert_eq!(find_link("example.com/video"), None);
        assert_eq!(find_link("youtube.com/"), None);
        assert_eq!(find_link("bak: youtube.com/watch?v=abc"), None);
    }

    #[test]
    fn rejects_non_links() {
        assert_eq!(find_link(""), None);
        assert_eq!(find_link("merhaba dünya"), None);
        assert_eq!(find_link("https://"), None);
        assert_eq!(find_link("https://..."), None);
        assert_eq!(find_link("http:// boşluk"), None);
        assert_eq!(find_link("https://example.com/a\u{7}b"), None);
        assert_eq!(find_link(&format!("https://example.com/{}", "a".repeat(5000))), None);
    }

    #[test]
    fn names_the_site() {
        assert_eq!(site("https://www.youtube.com/watch?v=abc"), Some("youtube.com"));
        assert_eq!(site("https://youtu.be/abc"), Some("youtu.be"));
        assert_eq!(site("http://user@example.com:8080/x"), Some("example.com"));
        assert_eq!(site("https://vimeo.com?x#y"), Some("vimeo.com"));
        assert_eq!(site("https://"), None);
    }
}

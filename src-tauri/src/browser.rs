//! Reads the page open in the user's browser, for the menu bar's "download
//! this video" item. Safari and Chromium-based browsers answer AppleScript;
//! macOS asks the user once (Privacy & Security › Automation) before Ambar may
//! do this. Firefox has no such interface, so it is not listed.

use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    Safari,
    Chromium,
}

#[derive(Debug)]
pub struct Browser {
    pub bundle_id: &'static str,
    /// "Safari'deki", used in menu labels ("Safari'deki videoyu indir").
    pub locative: &'static str,
    dialect: Dialect,
}

const BROWSERS: &[Browser] = &[
    Browser { bundle_id: "com.apple.Safari", locative: "Safari'deki", dialect: Dialect::Safari },
    Browser { bundle_id: "com.apple.SafariTechnologyPreview", locative: "Safari'deki", dialect: Dialect::Safari },
    Browser { bundle_id: "com.google.Chrome", locative: "Chrome'daki", dialect: Dialect::Chromium },
    Browser { bundle_id: "company.thebrowser.Browser", locative: "Arc'taki", dialect: Dialect::Chromium },
    Browser { bundle_id: "com.brave.Browser", locative: "Brave'deki", dialect: Dialect::Chromium },
    Browser { bundle_id: "com.microsoft.edgemac", locative: "Edge'deki", dialect: Dialect::Chromium },
    Browser { bundle_id: "com.vivaldi.Vivaldi", locative: "Vivaldi'deki", dialect: Dialect::Chromium },
    Browser { bundle_id: "com.operasoftware.Opera", locative: "Opera'daki", dialect: Dialect::Chromium },
    Browser { bundle_id: "org.chromium.Chromium", locative: "Chromium'daki", dialect: Dialect::Chromium },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub url: String,
    pub title: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BrowserError {
    NoBrowser,
    NoPage,
    NotAllowed,
    /// No answer; usually macOS's permission question is still on screen.
    TimedOut,
    Failed(String),
}

impl BrowserError {
    pub fn text(&self) -> String {
        match self {
            BrowserError::NoBrowser => "Açık bir tarayıcı bulunamadı. Safari, Chrome, Arc, Brave ya da Edge'de videoyu aç.".into(),
            BrowserError::NoPage => "Tarayıcıda açık bir sayfa yok.".into(),
            BrowserError::NotAllowed => {
                "Ambar'nın tarayıcındaki bağlantıyı okumasına izin verilmedi. Sistem Ayarları › Gizlilik ve Güvenlik › Otomasyon'dan Ambar'ya izin ver.".into()
            }
            BrowserError::TimedOut => "Tarayıcı yanıt vermedi. İlk kullanımda macOS bir izin sorusu gösterir; ona izin ver ya da Sistem Ayarları › Gizlilik ve Güvenlik › Otomasyon'u kontrol et.".into(),
            BrowserError::Failed(_) => "Tarayıcıdaki sayfa okunamadı. Videoyu Ambar'ya bağlantısıyla yapıştırabilirsin.".into(),
        }
    }
}

async fn run(program: &str, args: &[&str], timeout: Duration) -> Option<std::process::Output> {
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    tokio::time::timeout(timeout, child.wait_with_output()).await.ok()?.ok()
}

/// "ASN:0x0-0x4f64f6-"Notes": ASN:0x0-0x40040-"Safari":" -> ["ASN:0x0-0x4f64f6", ...]
fn parse_asns(text: &str) -> Vec<&str> {
    text.match_indices("ASN:0x")
        .map(|(i, _)| {
            let rest = &text[i..];
            let end = rest
                .char_indices()
                .skip(4)
                .find(|&(_, c)| !(c.is_ascii_hexdigit() || c == 'x' || c == '-'))
                .map_or(rest.len(), |(j, _)| j);
            rest[..end].trim_end_matches('-')
        })
        .collect()
}

/// Pulls `bundleID="com.apple.Safari"` out of `lsappinfo info` output.
fn parse_bundle_id(text: &str) -> Option<&str> {
    let start = text.find("bundleID=\"")? + "bundleID=\"".len();
    let len = text[start..].find('"')?;
    Some(&text[start..start + len])
}

fn known(bundle_id: &str) -> Option<&'static Browser> {
    BROWSERS.iter().find(|b| b.bundle_id == bundle_id)
}

/// The browser the user was in most recently. Apps are listed front to
/// back, so this also works while Ambar itself is in front.
pub async fn pick() -> Option<&'static Browser> {
    const QUICK: Duration = Duration::from_secs(2);
    let list = run("/usr/bin/lsappinfo", &["visibleProcessList"], QUICK).await?;
    let list = String::from_utf8_lossy(&list.stdout).into_owned();
    for asn in parse_asns(&list) {
        let Some(info) = run("/usr/bin/lsappinfo", &["info", "-only", "bundleID", asn], QUICK).await else {
            continue;
        };
        if let Some(browser) = parse_bundle_id(&String::from_utf8_lossy(&info.stdout)).and_then(known) {
            return Some(browser);
        }
    }
    None
}

fn script(browser: &Browser) -> String {
    let id = browser.bundle_id;
    // Tabs are separated from titles by a line feed; neither can contain one.
    match browser.dialect {
        // An empty tab has no URL ("missing value"); joining that with text
        // would make a list, so it is checked first.
        Dialect::Safari => format!(
            "tell application id \"{id}\"\n\
             if (count of documents) is 0 then return \"\"\n\
             set u to URL of front document\n\
             if u is missing value then return \"\"\n\
             return u & linefeed & (name of front document)\n\
             end tell"
        ),
        Dialect::Chromium => format!(
            "tell application id \"{id}\"\n\
             if (count of windows) is 0 then return \"\"\n\
             set t to active tab of front window\n\
             set u to URL of t\n\
             if u is missing value then return \"\"\n\
             return u & linefeed & (title of t)\n\
             end tell"
        ),
    }
}

fn parse_page(stdout: &str) -> Option<Page> {
    let mut lines = stdout.trim_end_matches(['\n', '\r']).splitn(2, '\n');
    let url = lines.next()?.trim().to_string();
    if url.is_empty() || url.starts_with("missing value") {
        return None;
    }
    let title = lines.next().unwrap_or("").trim().to_string();
    Some(Page { url, title })
}

fn classify_error(stderr: &str) -> BrowserError {
    // osascript reports AppleScript error numbers in parentheses.
    if stderr.contains("(-1743)") {
        BrowserError::NotAllowed
    } else if stderr.contains("(-1712)") {
        BrowserError::TimedOut
    } else if stderr.contains("(-600)") {
        BrowserError::NoBrowser
    } else if stderr.contains("(-1728)") || stderr.contains("(-1719)") {
        BrowserError::NoPage
    } else {
        BrowserError::Failed(stderr.lines().last().unwrap_or("").trim().to_string())
    }
}

/// The address and title of the front tab of `browser`. The first time,
/// macOS shows its permission prompt and this waits for the answer.
pub async fn current_page(browser: &Browser) -> Result<Page, BrowserError> {
    let source = script(browser);
    let out = run("/usr/bin/osascript", &["-e", &source], Duration::from_secs(120)).await.ok_or(BrowserError::TimedOut)?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("[kova] browser: {}", stderr.trim());
        return Err(classify_error(&stderr));
    }
    parse_page(&String::from_utf8_lossy(&out.stdout)).ok_or(BrowserError::NoPage)
}

/// "Big Buck Bunny - YouTube" -> "Big Buck Bunny"
pub fn clean_title(title: &str) -> String {
    let t = title.trim();
    let t = t.strip_prefix('(').and_then(|r| r.split_once(") ")).filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit())).map_or(t, |(_, rest)| rest);
    t.strip_suffix(" - YouTube").or_else(|| t.strip_suffix(" - YouTube Music")).unwrap_or(t).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsappinfo() {
        let list = "ASN:0x0-0x4f64f6-\"Notes\": ASN:0x0-0x40040-\"Safari\": ASN:0x0-0x3f03f-\"‎WhatsApp\":";
        assert_eq!(parse_asns(list), ["ASN:0x0-0x4f64f6", "ASN:0x0-0x40040", "ASN:0x0-0x3f03f"]);
        let info = "[ NULL ]  ASN:0x0-0x40040: (in front)\n    bundleID=\"com.apple.Safari\"\n    bundle path=[ NULL ]";
        assert_eq!(parse_bundle_id(info), Some("com.apple.Safari"));
        assert!(known("com.apple.Safari").is_some());
        assert!(known("org.mozilla.firefox").is_none());
    }

    #[test]
    fn parses_pages_and_errors() {
        assert_eq!(
            parse_page("https://www.youtube.com/watch?v=aqz-KE-bpKQ\nBig Buck Bunny - YouTube\n"),
            Some(Page { url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(), title: "Big Buck Bunny - YouTube".into() })
        );
        assert_eq!(parse_page("\n"), None);
        assert_eq!(parse_page("missing value, \n, Favorites\n"), None);
        assert_eq!(classify_error("execution error: Not authorized to send Apple events to Safari. (-1743)"), BrowserError::NotAllowed);
        assert_eq!(classify_error("execution error: Can’t get window 1. (-1728)"), BrowserError::NoPage);
        assert_eq!(classify_error("execution error: Safari got an error: AppleEvent timed out. (-1712)"), BrowserError::TimedOut);
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(clean_title("Big Buck Bunny - YouTube"), "Big Buck Bunny");
        assert_eq!(clean_title("(3) Sintel - YouTube"), "Sintel");
        assert_eq!(clean_title("(Live) Song"), "(Live) Song");
        assert_eq!(clean_title("Plain"), "Plain");
    }

    #[test]
    fn scripts_name_the_browser() {
        let safari = known("com.apple.Safari").unwrap();
        assert!(script(safari).contains("front document"));
        assert!(script(safari).contains("missing value"));
        let chrome = known("com.google.Chrome").unwrap();
        assert!(script(chrome).contains("active tab of front window"));
    }
}

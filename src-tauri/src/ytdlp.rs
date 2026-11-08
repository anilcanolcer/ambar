//! Everything that talks to yt-dlp: building command lines, reading media
//! info, parsing progress output and turning errors into plain Turkish.
//!
//! Ambar never implements any download or protection logic of its own. It
//! only passes options to the official yt-dlp program.

use crate::error::{msg, AppResult};
use crate::settings::Kind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Upper bound on playlist entries we list, so a huge channel does not
/// freeze the UI.
pub const PLAYLIST_LIMIT: usize = 1000;

#[derive(Clone, Debug)]
pub struct Runner {
    pub ytdlp: PathBuf,
    pub deno: PathBuf,
    pub ffmpeg_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Runner {
    /// Base command with options that make every run predictable: user
    /// config files and plugins are ignored, and the bundled helpers are
    /// passed by absolute path instead of being looked up on PATH.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.ytdlp);
        // The official builds start Python in isolated mode, which ignores
        // PYTHONUTF8 and PYTHONIOENCODING. Only --encoding keeps printed
        // paths and errors in UTF-8; Windows would otherwise use the ANSI
        // code page (cp1254 on Turkish systems) and break "ş" or "ı".
        cmd.env("PYTHONDONTWRITEBYTECODE", "1")
            .args(["--ignore-config", "--no-plugin-dirs", "--no-colors", "--encoding", "utf-8"])
            .arg("--js-runtimes")
            .arg(format!("deno:{}", self.deno.display()))
            .arg("--ffmpeg-location")
            .arg(&self.ffmpeg_dir)
            .arg("--cache-dir")
            .arg(&self.cache_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        crate::proc::hide_console(&mut cmd);
        // Lets FFmpeg verify HTTPS certificates when it downloads directly.
        if let Some(bundle) = crate::platform::ca_bundle() {
            cmd.env("SSL_CERT_FILE", bundle);
        }
        cmd
    }
}

/// Only plain web links are accepted. This also guarantees the value can
/// never be mistaken for a command-line option.
pub fn validate_url(input: &str) -> AppResult<String> {
    let url = input.trim();
    if url.is_empty() {
        return Err(msg("Bir bağlantı yapıştır."));
    }
    if url.len() > 4096 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(msg("Bu geçerli bir bağlantı gibi görünmüyor."));
    }
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err(msg("Bağlantı https:// ile başlamalı."));
    }
    Ok(url.to_string())
}

// ---------------------------------------------------------------- media info

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MediaInfo {
    Video(VideoInfo),
    Playlist(PlaylistInfo),
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeEstimates {
    /// MP3 at the default V0 quality.
    pub mp3: Option<u64>,
    pub m4a: Option<u64>,
    pub opus: Option<u64>,
    pub flac: Option<u64>,
    pub wav: Option<u64>,
    /// Video formats, keyed by resolution (shorter side, e.g. 1080).
    pub mp4: BTreeMap<u32, u64>,
    pub mkv: BTreeMap<u32, u64>,
    pub webm: BTreeMap<u32, u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub uploader: Option<String>,
    pub duration: Option<u64>,
    pub thumbnail: Option<String>,
    pub site: Option<String>,
    pub license: Option<String>,
    pub creative_commons: bool,
    /// Available video resolutions, highest first.
    pub resolutions: Vec<u32>,
    pub has_video: bool,
    pub has_audio: bool,
    /// The site offers VP8/VP9/AV1 video, so a WEBM file can be made.
    pub webm: bool,
    /// The site already provides artist/track tags (e.g. YouTube Music).
    pub has_music_tags: bool,
    /// Title looks like "Artist - Song".
    pub title_has_separator: bool,
    pub sizes: SizeEstimates,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistEntry {
    pub id: String,
    pub url: String,
    pub title: String,
    pub duration: Option<u64>,
    pub thumbnail: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub uploader: Option<String>,
    pub entries: Vec<PlaylistEntry>,
    pub truncated: bool,
    /// Entries that were themselves lists (e.g. a channel's playlists tab).
    #[serde(skip)]
    pub skipped_lists: usize,
}

fn s(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0)
}

fn fsize(f: &Value) -> Option<u64> {
    num(f, "filesize").or_else(|| num(f, "filesize_approx")).map(|n| n as u64)
}

fn is_none_codec(f: &Value, key: &str) -> bool {
    matches!(f.get(key).and_then(Value::as_str), None | Some("none"))
}

fn is_video(f: &Value) -> bool {
    !is_none_codec(f, "vcodec")
}

fn is_audio_only(f: &Value) -> bool {
    is_none_codec(f, "vcodec") && !is_none_codec(f, "acodec")
}

/// Codec not reported at all (null or missing). Many sites (direct file
/// links, archive.org, plain HTML5 pages) leave codecs empty; yt-dlp still
/// downloads them, so for availability this means "unknown", not "absent".
fn unknown_codec(f: &Value, key: &str) -> bool {
    f.get(key).map(Value::is_null).unwrap_or(true)
}

const AUDIO_EXTS: [&str; 8] = ["m4a", "mp3", "opus", "ogg", "oga", "wav", "flac", "aac"];

fn maybe_video(f: &Value) -> bool {
    is_video(f) || (unknown_codec(f, "vcodec") && !AUDIO_EXTS.contains(&s(f, "ext").unwrap_or_default().as_str()))
}

fn maybe_audio(f: &Value) -> bool {
    !is_none_codec(f, "acodec") || unknown_codec(f, "acodec")
}

/// yt-dlp's `res` sort key uses the shorter side, so vertical videos are
/// labelled the same way as yt-dlp will select them.
fn resolution(f: &Value) -> Option<u32> {
    let w = num(f, "width").map(|n| n as u32);
    let h = num(f, "height").map(|n| n as u32);
    match (w, h) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some(w.min(h)),
        (_, Some(h)) if h > 0 => Some(h),
        _ => None,
    }
}

fn best_thumbnail(v: &Value) -> Option<String> {
    if let Some(t) = s(v, "thumbnail") {
        return Some(t);
    }
    v.get("thumbnails")
        .and_then(Value::as_array)
        .and_then(|list| list.iter().rev().find_map(|t| s(t, "url")))
}

fn duration_secs(v: &Value) -> Option<u64> {
    num(v, "duration").map(|d| d.round() as u64)
}

/// Sites state licenses as text ("Creative Commons Attribution license"),
/// as creativecommons.org URLs or as short codes like "by-nc-sa".
pub fn is_creative_commons(license: &str) -> bool {
    let l = license.trim().to_ascii_lowercase();
    l.contains("creative commons")
        || l.contains("creativecommons.org/")
        || matches!(l.as_str(), "by" | "by-sa" | "by-nd" | "by-nc" | "by-nc-sa" | "by-nc-nd" | "cc0" | "cc-by" | "cc by")
        || l.starts_with("cc-by")
        || l.starts_with("cc by")
}

/// Human-friendly site name: the page's domain ("vimeo.com") rather than
/// yt-dlp's internal extractor name ("Generic", "TwitchVod").
fn site_name(v: &Value) -> Option<String> {
    let domain = s(v, "webpage_url_domain").map(|d| d.trim_start_matches("www.").trim_start_matches("m.").to_string());
    domain.or_else(|| s(v, "extractor_key").filter(|k| k != "Generic"))
}

pub fn parse_video(v: &Value, prefer_av1: bool) -> AppResult<VideoInfo> {
    match s(v, "live_status").as_deref() {
        Some("is_live") => return Err(msg("Canlı yayınlar indirilemez. Yayın bittikten sonra tekrar dene.")),
        Some("is_upcoming") => return Err(msg("Bu yayın henüz başlamadı.")),
        _ => {}
    }
    match s(v, "availability").as_deref() {
        Some("premium_only") => return Err(msg("Ücretli üyelik gerektiren içerikler indirilemez.")),
        Some("subscriber_only") => return Err(msg("Sadece kanal üyelerine açık içerikler indirilemez.")),
        Some("needs_auth") => return Err(msg("Bu içerik giriş yapmayı gerektiriyor, indirilemez.")),
        _ => {}
    }

    let empty = Vec::new();
    let formats = v.get("formats").and_then(Value::as_array).unwrap_or(&empty);
    let usable: Vec<&Value> = formats
        .iter()
        .filter(|f| !f.get("has_drm").and_then(Value::as_bool).unwrap_or(false))
        .collect();
    if !formats.is_empty() && usable.is_empty() {
        return Err(msg("Bu içerik DRM korumalı, indirilemez."));
    }
    // Some sites return a single direct file without a formats list.
    let single_file = formats.is_empty() && v.get("url").is_some();

    let has_video = usable.iter().any(|f| maybe_video(f)) || (single_file && maybe_video(v));
    let has_audio = usable.iter().any(|f| maybe_audio(f)) || (single_file && maybe_audio(v));
    // WEBM can only hold VP8/VP9/AV1 video; many sites only serve H.264.
    let webm_possible = usable.iter().any(|f| {
        let codec = s(f, "vcodec").unwrap_or_default();
        ["vp8", "vp9", "vp09", "av01"].iter().any(|c| codec.starts_with(c)) || s(f, "ext").as_deref() == Some("webm")
    });
    if !has_video && !has_audio {
        return Err(msg("Bu bağlantıda indirilebilir bir medya bulunamadı."));
    }

    let mut resolutions: Vec<u32> = usable
        .iter()
        .filter(|f| maybe_video(f))
        .filter_map(|f| resolution(f))
        .filter(|r| *r >= 144)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    resolutions.sort_unstable_by(|a, b| b.cmp(a));

    let duration = duration_secs(v);
    let non_drc = |f: &&&Value| !s(f, "format_id").unwrap_or_default().ends_with("-drc");
    let best_audio = |pred: &dyn Fn(&Value) -> bool| {
        usable
            .iter()
            .filter(|f| is_audio_only(f) && pred(f))
            .filter(non_drc)
            .max_by(|a, b| {
                num(a, "abr").unwrap_or(0.0).partial_cmp(&num(b, "abr").unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
    };
    let m4a_audio = best_audio(&|f| s(f, "ext").as_deref() == Some("m4a"));
    let opus_audio = best_audio(&|f| s(f, "acodec").map(|c| c.starts_with("opus")).unwrap_or(false));
    let any_audio = best_audio(&|_| true);
    let m4a_size = m4a_audio.and_then(fsize).or_else(|| any_audio.and_then(fsize));
    let opus_size = opus_audio.and_then(fsize).or_else(|| any_audio.and_then(fsize));
    let mut sizes = SizeEstimates {
        // LAME V0 averages roughly 235 kbit/s.
        mp3: duration.map(|d| d * 235_000 / 8),
        m4a: m4a_size,
        opus: opus_size,
        // Decoded lossy audio barely compresses: about 1.36 Mbit/s as FLAC,
        // and 48 kHz 16-bit stereo PCM is 1.536 Mbit/s as WAV (measured).
        flac: duration.map(|d| d * 170_000),
        wav: duration.map(|d| d * 192_000),
        mp4: BTreeMap::new(),
        mkv: BTreeMap::new(),
        webm: BTreeMap::new(),
    };
    // Size of the best video stream at `res` whose codec matches the first
    // matching preference group; falls back to the largest stream.
    // Returns the size and whether that very stream already carries audio.
    // `strict` skips the any-codec fallback (WEBM cannot use other codecs).
    let video_size = |res: u32, prefs: &[&[&str]], strict: bool| -> Option<(u64, bool)> {
        let candidates: Vec<&&Value> =
            usable.iter().filter(|f| maybe_video(f) && resolution(f) == Some(res) && fsize(f).is_some()).collect();
        let pick = |group: &[&str]| {
            candidates
                .iter()
                .filter(|f| {
                    let codec = s(f, "vcodec").unwrap_or_default();
                    group.iter().any(|w| codec.starts_with(w))
                })
                .max_by_key(|f| fsize(f))
                .copied()
        };
        let chosen = prefs
            .iter()
            .find_map(|group| pick(group))
            .or_else(|| if strict { None } else { candidates.iter().max_by_key(|f| fsize(f)).copied() })?;
        Some((fsize(chosen)?, !is_none_codec(chosen, "acodec")))
    };
    const H264: &[&str] = &["avc1", "h264"];
    const AV1: &[&str] = &["av01"];
    const VP9: &[&str] = &["vp9", "vp09"];
    for res in resolutions.iter().copied() {
        let mp4_prefs: &[&[&str]] = if res <= 1080 {
            &[H264]
        } else if prefer_av1 {
            &[AV1, VP9]
        } else {
            &[VP9]
        };
        if let Some((v, own)) = video_size(res, mp4_prefs, false) {
            sizes.mp4.insert(res, v + if own { 0 } else { m4a_size.unwrap_or(0) });
        }
        // MKV keeps whatever yt-dlp ranks best (AV1, then VP9) with Opus.
        if let Some((v, own)) = video_size(res, &[AV1, VP9, H264], false) {
            sizes.mkv.insert(res, v + if own { 0 } else { opus_size.unwrap_or(0) });
        }
        if let Some((v, own)) = video_size(res, &[VP9, AV1], true) {
            sizes.webm.insert(res, v + if own { 0 } else { opus_size.unwrap_or(0) });
        }
    }

    let title = s(v, "title").unwrap_or_else(|| "Adsız".to_string());
    let license = s(v, "license");
    let creative_commons = license.as_deref().map(is_creative_commons).unwrap_or(false);
    let has_music_tags = s(v, "artist").is_some()
        || s(v, "track").is_some()
        || v.get("artists").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);

    Ok(VideoInfo {
        id: s(v, "id").unwrap_or_default(),
        url: s(v, "webpage_url").or_else(|| s(v, "original_url")).unwrap_or_default(),
        title_has_separator: title.contains(" - ") || title.contains(" – "),
        title,
        uploader: s(v, "uploader").or_else(|| s(v, "channel")).or_else(|| s(v, "creator")),
        duration,
        thumbnail: best_thumbnail(v),
        site: site_name(v),
        license,
        creative_commons,
        resolutions,
        has_video,
        has_audio,
        webm: webm_possible,
        has_music_tags,
        sizes,
    })
}

const UNAVAILABLE_TITLES: [&str; 4] = ["[Private video]", "[Deleted video]", "[Unavailable video]", "[Removed video]"];

/// Flattens nested lists (a channel page lists its Videos/Shorts/Live tabs
/// as sub-playlists) and drops entries that are links to other lists, so
/// every remaining entry is exactly one video.
fn collect_entries(v: &Value, out: &mut Vec<Value>, skipped_lists: &mut usize, depth: usize) {
    let Some(entries) = v.get("entries").and_then(Value::as_array) else { return };
    for e in entries {
        if out.len() >= PLAYLIST_LIMIT {
            return;
        }
        let ty = e.get("_type").and_then(Value::as_str);
        if ty == Some("playlist") && depth < 3 && e.get("entries").is_some() {
            collect_entries(e, out, skipped_lists, depth + 1);
            continue;
        }
        let ie = e.get("ie_key").and_then(Value::as_str).unwrap_or("");
        let url = s(e, "url").or_else(|| s(e, "webpage_url")).unwrap_or_default();
        if ty == Some("playlist") || ie.ends_with("Tab") || ie.ends_with("Playlist") || is_list_url(&url) {
            *skipped_lists += 1;
            continue;
        }
        out.push(e.clone());
    }
}

/// Links that point at a whole list rather than one item. Some sites give
/// these without an extractor hint, so the URL shape is checked too.
fn is_list_url(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    (u.contains("list=") && !u.contains("v="))
        || (u.contains("soundcloud.com/") && u.contains("/sets/"))
        || u.contains("bandcamp.com/album/")
        || u.contains("vimeo.com/showcase/")
        || u.contains("vimeo.com/album/")
}

pub fn parse_playlist(v: &Value) -> PlaylistInfo {
    let mut flat = Vec::new();
    let mut skipped_lists = 0;
    collect_entries(v, &mut flat, &mut skipped_lists, 0);
    let raw = &flat;
    let mut seen = HashSet::new();
    let entries: Vec<PlaylistEntry> = raw
        .iter()
        .filter_map(|e| {
            let title = s(e, "title").unwrap_or_else(|| "Adsız".to_string());
            if UNAVAILABLE_TITLES.contains(&title.as_str()) {
                return None;
            }
            let url = s(e, "url").or_else(|| s(e, "webpage_url"))?;
            let url = validate_url(&url).ok()?;
            let id = s(e, "id").unwrap_or_else(|| url.clone());
            if !seen.insert(id.clone()) {
                return None;
            }
            Some(PlaylistEntry { id, url, title, duration: duration_secs(e), thumbnail: best_thumbnail(e) })
        })
        .take(PLAYLIST_LIMIT)
        .collect();
    let reported = v.get("playlist_count").and_then(Value::as_u64).unwrap_or(0) as usize;
    PlaylistInfo {
        id: s(v, "id").unwrap_or_default(),
        url: s(v, "webpage_url").or_else(|| s(v, "original_url")).unwrap_or_default(),
        title: s(v, "title").unwrap_or_else(|| "Oynatma listesi".to_string()),
        uploader: s(v, "uploader").or_else(|| s(v, "channel")),
        truncated: raw.len() >= PLAYLIST_LIMIT || reported > PLAYLIST_LIMIT,
        entries,
        skipped_lists,
    }
}

pub fn parse_info_json(text: &str, prefer_av1: bool) -> AppResult<MediaInfo> {
    let v: Value = serde_json::from_str(text.trim())?;
    if v.get("_type").and_then(Value::as_str) == Some("playlist") {
        let list = parse_playlist(&v);
        if list.entries.is_empty() {
            return Err(msg(if list.skipped_lists > 0 {
                "Bu sayfa oynatma listelerinden oluşuyor. Bir listeyi açıp onun bağlantısını yapıştır."
            } else {
                "Bu listede indirilebilir bir video bulunamadı."
            }));
        }
        Ok(MediaInfo::Playlist(list))
    } else {
        Ok(MediaInfo::Video(parse_video(&v, prefer_av1)?))
    }
}

pub async fn fetch_info(runner: &Runner, url: &str, playlist: bool, prefer_av1: bool) -> AppResult<MediaInfo> {
    let url = validate_url(url)?;
    let mut cmd = runner.command();
    cmd.args(["-J", "--flat-playlist"])
        .arg(if playlist { "--yes-playlist" } else { "--no-playlist" })
        .arg("--playlist-items")
        .arg(format!("1:{PLAYLIST_LIMIT}"))
        .arg("--")
        .arg(&url);
    let out = tokio::time::timeout(Duration::from_secs(120), cmd.output())
        .await
        .map_err(|_| msg("Bilgi alınırken zaman aşımı oldu. Bağlantını kontrol edip tekrar dene."))??;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(msg(humanize_error(&stderr, false)));
    }
    parse_info_json(&String::from_utf8_lossy(&out.stdout), prefer_av1)
}

// ----------------------------------------------------------------- downloads

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRequest {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    pub kind: Kind,
    /// Wanted resolution for MP4 (shorter side). Ignored for audio.
    #[serde(default)]
    pub res: Option<u32>,
    /// Fill artist/track tags from an "Artist - Song" title.
    #[serde(default)]
    pub split_artist: bool,
    /// MP3 bitrate in kbit/s; None means the best variable quality (V0).
    #[serde(default)]
    pub audio_quality: Option<u32>,
    /// Embed Turkish and English subtitles (video only).
    #[serde(default)]
    pub subtitles: bool,
    /// Folder picked for this download; None means the download folder
    /// from the settings.
    #[serde(default)]
    pub save_dir: Option<String>,
}

/// MP3 bitrates the UI offers.
pub const MP3_BITRATES: [u32; 4] = [320, 256, 192, 128];

pub const MARK_META: &str = "KOVAMETA|";
pub const MARK_PROGRESS: &str = "KOVA|";
pub const MARK_POST: &str = "KOVAPP|";
pub const MARK_FILE: &str = "KOVAFILE|";

/// Crops the cover art to a centered square, the way music players expect.
const SQUARE_COVER: &str =
    r#"ThumbnailsConvertor:-qmin 1 -q:v 1 -vf crop="'if(gt(ih,iw),iw,ih)':'if(gt(iw,ih),ih,iw)'""#;

/// Strips "(Official Video)", "[Lyrics]" and similar suffixes from the
/// track tag only; the file name keeps the original title.
const TRACK_CLEANUP: &str = r"(?i)\s*[(\[][^)\]]*\b(official|lyrics?|audio|video|klip|visuali[sz]er|hd|4k)\b[^)\]]*[)\]]";

fn push(a: &mut Vec<OsString>, items: &[&str]) {
    a.extend(items.iter().map(OsString::from));
}

pub fn download_args(req: &DownloadRequest, home: &Path, temp: &Path, square_cover: bool, prefer_av1: bool) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    push(&mut a, &["--no-playlist", "--newline", "--progress", "--no-mtime", "--continue"]);
    // A dropped connection is retried after a pause that doubles from 1 s
    // up to 30 s, so a Wi-Fi drop of a few minutes is ridden out in the same
    // run. Without the pause all retries fail within a second.
    // Fragments get no pause: a dropped connection inside one is retried by
    // the HTTP downloader under the pause above, and a missing fragment
    // (HTTP 404) would otherwise wait three minutes before being skipped.
    push(&mut a, &["--retries", "10", "--fragment-retries", "10", "--retry-sleep", "http:exp=1:30"]);
    // Progress at most twice a second; the window shows no more than that.
    push(&mut a, &["--progress-delta", "0.5"]);
    a.push("-P".into());
    a.push(format!("home:{}", home.display()).into());
    a.push("-P".into());
    a.push(format!("temp:{}", temp.display()).into());
    push(&mut a, &["-o", &output_template(home, temp), "--embed-metadata"]);
    if req.kind.takes_cover() {
        push(&mut a, &["--embed-thumbnail"]);
    }

    if req.kind.is_audio() {
        match req.kind {
            Kind::Mp3 => {
                let quality = match req.audio_quality {
                    Some(kbps) if MP3_BITRATES.contains(&kbps) => format!("{kbps}K"),
                    _ => "0".to_string(), // best variable bitrate (V0)
                };
                push(&mut a, &["-f", "ba/b", "-x", "--audio-format", "mp3", "--audio-quality", &quality]);
            }
            // Keeps YouTube's original AAC stream without re-encoding.
            Kind::M4a => push(&mut a, &["-f", "ba[ext=m4a]/ba/b", "-x", "--audio-format", "m4a"]),
            // YouTube's own Opus stream, copied as is.
            Kind::Opus => push(&mut a, &["-f", "ba[acodec^=opus]/ba/b", "-x", "--audio-format", "opus"]),
            Kind::Flac => push(&mut a, &["-f", "ba/b", "-x", "--audio-format", "flac"]),
            _ => push(&mut a, &["-f", "ba/b", "-x", "--audio-format", "wav"]),
        }
        if req.kind.takes_cover() {
            push(&mut a, &["--convert-thumbnails", "jpg"]);
            if square_cover {
                push(&mut a, &["--ppa", SQUARE_COVER]);
            }
        }
        if req.split_artist {
            push(&mut a, &["--parse-metadata", "title:%(artist)s - %(track)s"]);
            push(&mut a, &["--replace-in-metadata", "track", TRACK_CLEANUP, ""]);
        }
    } else {
        let res = req.res.unwrap_or(1080).clamp(144, 4320);
        match req.kind {
            Kind::Mkv => {
                // Best streams as they are (usually AV1/VP9 + Opus), no conversion.
                let sort = format!("res:{res}");
                push(&mut a, &["-f", "bv*+ba/b", "-S", &sort, "--merge-output-format", "mkv", "--remux-video", "mkv"]);
            }
            Kind::Webm => {
                // WEBM only holds VP8/VP9/AV1 video with Opus/Vorbis audio.
                let sort = format!("res:{res},vcodec:vp9,acodec:opus");
                push(&mut a, &[
                    "-f",
                    "bv*[vcodec~='^(vp0?9|vp8|av01)']+ba[acodec~='^(opus|vorbis)']/b[ext=webm]",
                    "-S",
                    &sort,
                    "--merge-output-format",
                    "webm",
                ]);
            }
            _ => {
                // Up to 1080p H.264 + AAC is preferred because every Mac
                // player opens it. Above that YouTube only offers VP9/AV1;
                // AV1 is used when the Mac decodes it in hardware, since
                // QuickTime cannot play VP9 at all.
                let codec = if res > 1080 && prefer_av1 { "av01" } else { "h264" };
                let sort = format!("res:{res},vcodec:{codec},acodec:aac");
                push(&mut a, &["-f", "bv*+ba/b", "-S", &sort, "--merge-output-format", "mp4", "--remux-video", "mp4"]);
            }
        }
        push(&mut a, &["--embed-chapters"]);
        if req.subtitles {
            // Manual subtitles only; auto-translated tracks are not requested.
            // A subtitle that cannot be fetched (e.g. HTTP 429) must not stop
            // the video; --ignore-errors turns that into a warning while real
            // download errors still fail. TTML/DFXP are converted because
            // FFmpeg cannot embed them.
            push(&mut a, &["--embed-subs", "--sub-langs", "tr.*,en.*", "--convert-subs", "vtt", "--ignore-errors"]);
        }
    }

    push(&mut a, &[
        "--print",
        "before_dl:KOVAMETA|%(requested_formats.0.format_id)s|%(requested_formats.0.filesize,requested_formats.0.filesize_approx)s|%(requested_formats.1.format_id)s|%(requested_formats.1.filesize,requested_formats.1.filesize_approx)s|%(format_id)s|%(filesize,filesize_approx)s",
        "--progress-template",
        "download:KOVA|%(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s|%(info.format_id)s",
        "--progress-template",
        "postprocess:KOVAPP|%(progress.status)s|%(progress.postprocessor)s",
        // As JSON, the path is plain ASCII whatever the console encoding.
        "--print",
        "after_move:KOVAFILE|%(filepath)j",
        // --print turns yt-dlp quiet, which also hides its "Got error ...
        // Retrying (2/10)" notices; Ambar shows those as a lost connection.
        "--no-quiet",
        "--",
    ]);
    a.push(OsString::from(&req.url));
    a
}

/// File name template: the title, cut to a number of UTF-8 bytes.
#[cfg(not(windows))]
fn output_template(_home: &Path, _temp: &Path) -> String {
    "%(title).150B.%(ext)s".to_string()
}

/// Windows keeps the 260-character path limit unless long paths were turned
/// on in the registry, and the work folder sits deep in AppData, so the
/// title gets whatever room the longer of the two folders leaves.
#[cfg(windows)]
fn output_template(home: &Path, temp: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    let longest = [home, temp].iter().map(|p| p.as_os_str().encode_wide().count()).max().unwrap_or(0);
    format!("%(title).{}B.%(ext)s", windows_title_bytes(longest))
}

/// Title length in UTF-8 bytes for a folder path of `dir_len` UTF-16 units.
/// N bytes of UTF-8 never make more than N UTF-16 units, so the cap also
/// holds for the characters Windows counts.
#[cfg(any(windows, test))]
fn windows_title_bytes(dir_len: usize) -> usize {
    // 259 characters plus the final NUL; the separator; and room for the
    // longest name yt-dlp builds from the title, a fragment of one stream:
    // "<title>.f<format id>.<ext>.part-Frag<N>.part".
    const SUFFIX: usize = 60;
    259usize.saturating_sub(dir_len + 1 + SUFFIX).clamp(40, 150)
}

// ------------------------------------------------------------ output parsing

#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Meta { parts: Vec<(String, Option<u64>)> },
    Progress { finished: bool, downloaded: Option<u64>, total: Option<u64>, speed: Option<f64>, eta: Option<u64>, format_id: String },
    Post { started: bool, name: String },
    File(String),
    Error(String),
    /// A request failed and yt-dlp is about to try again; `network` is
    /// false when the site answered with an error instead.
    Retry { attempt: u32, of: u32, network: bool },
    Other,
}

/// "[download] Got error: Read timed out.. Retrying (2/10)..." (also
/// "Retrying fragment 12 (2/10)...") gives (2, 10).
fn retry_count(line: &str) -> Option<(u32, u32)> {
    if !(line.starts_with('[') && line.contains("] Got error: ")) {
        return None;
    }
    let rest = &line[line.rfind("Retrying")?..];
    let inside = &rest[rest.rfind('(')? + 1..];
    let (n, m) = inside[..inside.find(')')?].split_once('/')?;
    Some((n.trim().parse().ok()?, m.trim().parse().ok()?))
}

fn field_u64(s: &str) -> Option<u64> {
    s.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64)
}

fn field_str(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s != "NA").then(|| s.to_string())
}

pub fn parse_line(raw: &str) -> Line {
    let line = raw.trim_end_matches(['\r', '\n']);
    if let Some(rest) = line.strip_prefix(MARK_FILE) {
        // The path comes as a JSON string; anything else is taken as it is.
        let path = serde_json::from_str::<String>(rest.trim()).unwrap_or_else(|_| rest.to_string());
        return if path.trim().is_empty() { Line::Other } else { Line::File(path) };
    }
    if let Some(rest) = line.strip_prefix(MARK_META) {
        let f: Vec<&str> = rest.split('|').collect();
        let get = |i: usize| f.get(i).copied().unwrap_or("NA");
        let mut parts = Vec::new();
        if let (Some(a), Some(b)) = (field_str(get(0)), field_str(get(2))) {
            parts.push((a, field_u64(get(1))));
            parts.push((b, field_u64(get(3))));
        } else if let Some(single) = field_str(get(4)) {
            parts.push((single, field_u64(get(5))));
        }
        return Line::Meta { parts };
    }
    if let Some(rest) = line.strip_prefix(MARK_PROGRESS) {
        let f: Vec<&str> = rest.split('|').collect();
        let get = |i: usize| f.get(i).copied().unwrap_or("NA");
        let total = field_u64(get(2)).or_else(|| field_u64(get(3)));
        return Line::Progress {
            finished: get(0).trim() == "finished",
            downloaded: field_u64(get(1)),
            total,
            speed: get(4).trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n >= 0.0),
            eta: field_u64(get(5)),
            format_id: field_str(get(6)).unwrap_or_default(),
        };
    }
    if let Some(rest) = line.strip_prefix(MARK_POST) {
        let (status, name) = rest.split_once('|').unwrap_or((rest, ""));
        return Line::Post { started: status.trim() == "started", name: name.trim().to_string() };
    }
    if let Some(rest) = line.strip_prefix("ERROR:") {
        return Line::Error(rest.trim().to_string());
    }
    if let Some((attempt, of)) = retry_count(line) {
        return Line::Retry { attempt, of, network: !line.to_ascii_lowercase().contains("http error") };
    }
    Line::Other
}

/// Turns per-stream progress into one overall fraction. MP4 downloads are
/// two streams (video, then audio), so sizes printed before the download
/// are used to weight them.
#[derive(Debug, Default)]
pub struct ProgressTracker {
    parts: Vec<(String, Option<u64>)>,
    finished: Vec<String>,
    done_bytes: u64,
}

impl ProgressTracker {
    pub fn set_parts(&mut self, parts: Vec<(String, Option<u64>)>) {
        self.parts = parts;
        self.finished.clear();
        self.done_bytes = 0;
    }

    pub fn part_count(&self) -> usize {
        self.parts.len()
    }

    /// Index of the stream currently downloading, if known.
    pub fn part_index(&self, format_id: &str) -> Option<usize> {
        self.parts.iter().position(|(id, _)| id == format_id)
    }

    pub fn update(&mut self, finished: bool, format_id: &str, downloaded: Option<u64>, total: Option<u64>) -> Option<f64> {
        let current = downloaded.unwrap_or(0);
        let already = self.finished.iter().any(|f| f == format_id);
        if finished && !already {
            self.finished.push(format_id.to_string());
            self.done_bytes += downloaded.or(total).unwrap_or(0);
        }
        let in_flight = if finished || already { 0 } else { current };
        let known_total: Option<u64> = if self.parts.is_empty() {
            None
        } else {
            self.parts.iter().map(|(_, size)| *size).sum()
        };
        let fraction = match known_total {
            Some(t) if t > 0 => (self.done_bytes + in_flight) as f64 / t as f64,
            _ => {
                let n = self.parts.len().max(1) as f64;
                let index = self.part_index(format_id).unwrap_or(self.finished.len().saturating_sub(usize::from(finished))) as f64;
                let within = if finished { 1.0 } else { total.map(|t| current as f64 / t.max(1) as f64).unwrap_or(0.0) };
                (index + within) / n
            }
        };
        // Size estimates can be slightly off; never show 100% before the
        // last stream really finished.
        let all_done = !self.parts.is_empty() && self.finished.len() >= self.parts.len();
        Some(if all_done { 1.0 } else { fraction.clamp(0.0, 0.99) })
    }
}

pub fn postprocessor_label(name: &str, kind: Kind) -> &'static str {
    match name {
        "ExtractAudio" => "Ses dönüştürülüyor",
        "Merger" => "Görüntü ve ses birleştiriliyor",
        "EmbedThumbnail" | "ThumbnailsConvertor" => "Kapak resmi ekleniyor",
        "Metadata" | "MetadataParser" | "FFmpegMetadata" => "Bilgiler ekleniyor",
        "VideoRemuxer" | "VideoConvertor" => match kind {
            Kind::Mkv => "MKV'ye çevriliyor",
            Kind::Webm => "WEBM'e çevriliyor",
            _ => "MP4'e çevriliyor",
        },
        "FFmpegSubtitlesConvertor" | "EmbedSubtitle" => "Altyazılar ekleniyor",
        "FixupM4a" | "FixupM3u8" | "FixupStretched" | "FixupDuplicateMoov" | "FixupTimestamp" | "FixupDuration" => "Dosya düzeltiliyor",
        "MoveFiles" | "MoveFilesAfterDownload" => "Klasöre taşınıyor",
        _ => "Son rötuşlar yapılıyor",
    }
}

/// Transient failures worth retrying with fresh URLs. "Giving up after"
/// follows yt-dlp's own retries, which only cover connection errors and
/// 5xx answers.
pub fn is_retryable(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    is_network_error(&s)
        || ["http error 403", "http error 500", "http error 502", "http error 503", "giving up after"]
            .iter()
            .chain(WINDOWS_LOCKED.iter())
            .any(|p| s.contains(p))
}

/// The connection dropped or the network is gone, as opposed to the site
/// answering with an error.
pub fn is_network_error(text: &str) -> bool {
    let s = text.to_ascii_lowercase();
    ["timed out", "connection reset", "incompleteread", "remote end closed", "temporary failure in name resolution"]
        .iter()
        .chain(NETWORK_DOWN.iter())
        .chain(WINDOWS_NETWORK.iter())
        .any(|p| s.contains(p))
}

/// How a missing network shows up while Wi-Fi drops or comes back after
/// sleep: the name lookup fails ("[Errno 8] nodename nor servname provided"
/// on macOS) or there is no route yet.
const NETWORK_DOWN: [&str; 8] = [
    "failed to resolve",
    "nodename nor servname",
    "network is unreachable",
    "network is down",
    "no route to host",
    "connection refused",
    "connection aborted",
    "eof occurred in violation of protocol",
];

/// How Windows reports a reset or aborted connection and a connect timeout.
/// Windows words these in the system language, so the error codes are
/// matched as well as the English text: "ConnectionResetError(10054, '...')"
/// or "[WinError 10054] ...".
const WINDOWS_NETWORK: [&str; 8] = [
    "forcibly closed by the remote host",
    "did not properly respond",
    "(10054,",
    "[winerror 10054]",
    "(10053,",
    "[winerror 10053]",
    "(10060,",
    "[winerror 10060]",
];

/// A file briefly locked by Defender or the search indexer (yt-dlp only
/// writes inside Ambar's work folder).
const WINDOWS_LOCKED: [&str; 2] = ["[winerror 32]", "[winerror 5]"];

/// Maps yt-dlp's English error output to short Turkish explanations.
/// `downloading` only changes the wording of the unmapped fallback.
pub fn humanize_error(stderr: &str, downloading: bool) -> String {
    // Warnings mention things like skipped DRM formats even when they have
    // nothing to do with the failure, so only real error lines count.
    let lower = stderr
        .lines()
        .filter(|l| !l.trim_start().starts_with("WARNING:"))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let has = |p: &str| lower.contains(p);
    let text = if has("not a bot") {
        "YouTube bu bağlantı için bot kontrolü istedi. Biraz bekleyip tekrar dene. VPN açıksa kapatmayı dene."
    } else if has("confirm your age") || has("age-restricted") || has("inappropriate for some users") {
        "Bu içerik yaş kısıtlamalı. Giriş yapmadan indirilemiyor."
    } else if has("private video") {
        "Bu video gizli."
    } else if has("members-only") || has("join this channel") || has("members only") {
        "Sadece kanal üyelerine açık içerikler indirilemez."
    } else if has("drm protect") || has("drm-protected") {
        "Bu içerik DRM korumalı, indirilemez."
    } else if has("premieres in") || has("live event will begin") || has("is_upcoming") {
        "Bu yayın henüz başlamadı."
    } else if has("unsupported url") {
        "Bu bağlantı desteklenmiyor."
    } else if has("video unavailable") || has("this video is unavailable") || has("has been removed") || has("not available in your country") {
        "Video kullanılamıyor. Silinmiş ya da bulunduğun ülkede erişime kapalı olabilir."
    } else if has("http error 429") || has("too many requests") {
        "Çok fazla istek gönderildi. Birkaç dakika bekleyip tekrar dene."
    } else if has("http error 403") {
        "Site isteği reddetti (403). Birkaç dakika sonra tekrar dene."
    } else if has("no space left") || has("[winerror 112]") {
        // yt-dlp only writes to Ambar's work folder on the system disk.
        if cfg!(windows) {
            "Sistem diskinde (genelde C:) yer kalmadı. Ambar indirmeyi önce orada hazırlar; biraz yer açıp tekrar dene."
        } else {
            "Mac'in diskinde yer kalmadı. Ambar indirmeyi önce orada hazırlar; biraz yer açıp tekrar dene."
        }
    } else if has("[winerror 32]") || has("[winerror 5]") || (cfg!(windows) && (has("permission denied") || has("operation not permitted"))) {
        // yt-dlp and FFmpeg only write inside Ambar's work folder, never the
        // download folder; on Windows a refusal there is a scanner holding
        // a file that was just written.
        "Dosya başka bir program tarafından kullanılıyor (genelde Windows Güvenliği taraması). Biraz bekleyip tekrar dene."
    } else if has("permission denied") || has("operation not permitted") {
        "İndirme klasörüne yazılamıyor. Ayarlardan başka bir klasör seç."
    } else if has("requested format is not available") {
        "İstenen kalite bu içerik için mevcut değil. Başka bir kalite seç."
    } else if has("unable to download webpage") || has("getaddrinfo") || has("failed to resolve") || has("nodename nor servname") || has("network is unreachable") || has("network is down") || has("no route to host") || has("timed out") || has("did not properly respond") || has("(10060,") || has("[winerror 10060]") || has("[winerror 10051]") {
        "Siteye ulaşılamadı. İnternet bağlantını kontrol et."
    } else {
        ""
    };
    if !text.is_empty() {
        return text.to_string();
    }
    let last_error = stderr
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("ERROR:").map(str::trim))
        .map(|l| {
            // "[youtube] abc123: Something" -> "Something"
            match (l.starts_with('['), l.find("]: "), l.find(": ")) {
                (true, _, Some(i)) => l[i + 2..].to_string(),
                _ => l.to_string(),
            }
        });
    let prefix = if downloading { "İndirme başarısız" } else { "Bağlantı açılamadı" };
    match last_error {
        Some(e) if !e.is_empty() => format!("{prefix}: {e}"),
        _ => "Beklenmeyen bir hata oluştu. Tekrar dene.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(kind: Kind) -> DownloadRequest {
        DownloadRequest {
            url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".into(),
            title: "t".into(),
            thumbnail: None,
            kind,
            res: Some(720),
            split_artist: true,
            audio_quality: None,
            subtitles: false,
            save_dir: None,
        }
    }

    fn args(kind: Kind) -> Vec<String> {
        download_args(&req(kind), Path::new("/Users/x/Downloads/Ambar"), Path::new("/tmp/j 1"), true, false)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn url_validation() {
        assert!(validate_url("https://youtu.be/abc").is_ok());
        assert_eq!(validate_url("  https://a.com/x  ").unwrap(), "https://a.com/x");
        assert!(validate_url("--exec rm").is_err());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("https://a.com/x y").is_err());
        assert!(validate_url("").is_err());
    }

    #[test]
    fn every_format_gets_valid_options() {
        let has = |a: &[String], x: &str| a.iter().any(|v| v == x);
        let pair = |a: &[String], k: &str, v: &str| a.windows(2).any(|w| w[0] == k && w[1] == v);
        for (kind, ext) in [(Kind::Mp3, "mp3"), (Kind::M4a, "m4a"), (Kind::Opus, "opus"), (Kind::Flac, "flac"), (Kind::Wav, "wav")] {
            let a = args(kind);
            assert!(pair(&a, "--audio-format", ext), "{kind:?}");
            assert!(!has(&a, "--embed-subs"), "audio never gets subtitles");
        }
        for (kind, ext) in [(Kind::Mp4, "mp4"), (Kind::Mkv, "mkv"), (Kind::Webm, "webm")] {
            assert!(pair(&args(kind), "--merge-output-format", ext), "{kind:?}");
        }
        // Formats that cannot hold cover art must not ask for it (it fails the job).
        for kind in [Kind::Wav, Kind::Webm] {
            let a = args(kind);
            assert!(!has(&a, "--embed-thumbnail") && !has(&a, "--ppa"), "{kind:?}");
        }
        for kind in [Kind::Mp3, Kind::Opus, Kind::Flac, Kind::Mp4, Kind::Mkv] {
            assert!(has(&args(kind), "--embed-thumbnail"), "{kind:?}");
        }
        let mut mp3 = req(Kind::Mp3);
        mp3.audio_quality = Some(320);
        let a: Vec<String> = download_args(&mp3, Path::new("/o"), Path::new("/t"), true, false).into_iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(pair(&a, "--audio-quality", "320K"));
        mp3.audio_quality = Some(999);
        let a: Vec<String> = download_args(&mp3, Path::new("/o"), Path::new("/t"), true, false).into_iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(pair(&a, "--audio-quality", "0"), "unknown bitrates fall back to V0");
        let mut subs = req(Kind::Mkv);
        subs.subtitles = true;
        let a: Vec<String> = download_args(&subs, Path::new("/o"), Path::new("/t"), true, false).into_iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(has(&a, "--embed-subs") && pair(&a, "--sub-langs", "tr.*,en.*"));
        assert!(pair(&a, "--convert-subs", "vtt") && has(&a, "--ignore-errors"));
        assert!(!has(&args(Kind::Mkv), "--ignore-errors"), "errors are only softened when subtitles are requested");
    }

    #[test]
    fn url_is_last_and_after_separator() {
        for kind in [Kind::Mp3, Kind::M4a, Kind::Opus, Kind::Flac, Kind::Wav, Kind::Mp4, Kind::Mkv, Kind::Webm] {
            let a = args(kind);
            assert_eq!(a.last().unwrap(), "https://www.youtube.com/watch?v=aqz-KE-bpKQ");
            assert_eq!(a[a.len() - 2], "--");
        }
    }

    #[test]
    fn kind_specific_args() {
        let mp3 = args(Kind::Mp3);
        assert!(mp3.windows(2).any(|w| w[0] == "--audio-format" && w[1] == "mp3"));
        assert!(mp3.iter().any(|a| a == "title:%(artist)s - %(track)s"));
        assert!(mp3.iter().any(|a| a == "temp:/tmp/j 1"));
        let mp4 = args(Kind::Mp4);
        assert!(mp4.windows(2).any(|w| w[0] == "-S" && w[1] == "res:720,vcodec:h264,acodec:aac"));
        assert!(!mp4.iter().any(|a| a == "-x"));
        let mut hi = req(Kind::Mp4);
        hi.res = Some(2160);
        let with_av1: Vec<String> = download_args(&hi, Path::new("/o"), Path::new("/t"), true, true)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(with_av1.iter().any(|a| a == "res:2160,vcodec:av01,acodec:aac"));
        let low_res_av1: Vec<String> = download_args(&req(Kind::Mp4), Path::new("/o"), Path::new("/t"), true, true)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(low_res_av1.iter().any(|a| a == "res:720,vcodec:h264,acodec:aac"), "H.264 stays the choice up to 1080p");
    }

    #[test]
    fn parses_progress_lines() {
        assert_eq!(
            parse_line("KOVAMETA|160|4323893|140|10271496|160+140|14595389"),
            Line::Meta { parts: vec![("160".into(), Some(4323893)), ("140".into(), Some(10271496))] }
        );
        assert_eq!(
            parse_line("KOVAMETA|NA|NA|NA|NA|251|NA"),
            Line::Meta { parts: vec![("251".into(), None)] }
        );
        match parse_line("KOVA|downloading|3072|10271496|NA|300971.75|34|140") {
            Line::Progress { finished, downloaded, total, eta, format_id, .. } => {
                assert!(!finished);
                assert_eq!(downloaded, Some(3072));
                assert_eq!(total, Some(10271496));
                assert_eq!(eta, Some(34));
                assert_eq!(format_id, "140");
            }
            other => panic!("{other:?}"),
        }
        match parse_line("KOVA|downloading|100|NA|2000.5|NA|NA|251-drc") {
            Line::Progress { total, speed, .. } => {
                assert_eq!(total, Some(2000));
                assert_eq!(speed, None);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(parse_line("KOVAPP|started|Merger"), Line::Post { started: true, name: "Merger".into() });
        assert_eq!(parse_line("KOVAFILE|/a/b|c.mp3"), Line::File("/a/b|c.mp3".into()));
        assert_eq!(parse_line(r#"KOVAFILE|"/a/b|c.mp3""#), Line::File("/a/b|c.mp3".into()));
        assert_eq!(
            parse_line(r#"KOVAFILE|"C:\\Users\\\u00c7a\u011fr\u0131\\out\\Bar\u0131\u015f Man\u00e7o \ud83c\udfb5.mp3""#),
            Line::File("C:\\Users\\Çağrı\\out\\Barış Manço 🎵.mp3".into())
        );
        assert_eq!(parse_line("KOVAFILE|\"\""), Line::Other);
        assert_eq!(parse_line("ERROR: boom"), Line::Error("boom".into()));
        assert_eq!(parse_line("[download] something"), Line::Other);
        assert_eq!(
            parse_line("[download] Got error: HTTPSConnectionPool(host='rr1.googlevideo.com', port=443): Read timed out.. Retrying (2/10)..."),
            Line::Retry { attempt: 2, of: 10, network: true }
        );
        assert_eq!(parse_line("[download] Got error: HTTP Error 503. Retrying fragment 12 (1/10)..."), Line::Retry { attempt: 1, of: 10, network: false });
        assert_eq!(parse_line("[download] Got error: boom (1/2)"), Line::Other);
        assert_eq!(parse_line("[download] Sleeping 4.00 seconds ..."), Line::Other);
    }

    #[test]
    fn tracker_weights_two_streams() {
        let mut t = ProgressTracker::default();
        t.set_parts(vec![("299".into(), Some(900)), ("140".into(), Some(100))]);
        assert_eq!(t.update(false, "299", Some(450), Some(900)), Some(0.45));
        assert_eq!(t.update(true, "299", Some(900), Some(900)), Some(0.9));
        assert_eq!(t.update(false, "140", Some(50), Some(100)), Some(0.95));
        assert_eq!(t.update(true, "140", Some(100), Some(100)), Some(1.0));
    }

    #[test]
    fn tracker_without_sizes() {
        let mut t = ProgressTracker::default();
        t.set_parts(vec![("a".into(), None), ("b".into(), None)]);
        assert_eq!(t.update(false, "a", Some(50), Some(100)), Some(0.25));
        assert_eq!(t.update(true, "a", Some(100), Some(100)), Some(0.5));
        assert_eq!(t.update(false, "b", Some(50), Some(100)), Some(0.75));
    }

    #[test]
    fn humanizes_errors() {
        assert!(humanize_error("ERROR: [youtube] x: Sign in to confirm you’re not a bot", true).contains("bot"));
        assert!(humanize_error("ERROR: [youtube] x: Private video. Sign in", true).contains("gizli"));
        assert_eq!(humanize_error("ERROR: [generic] x: Something odd", true), "İndirme başarısız: Something odd");
        assert_eq!(humanize_error("ERROR: [generic] x: Something odd", false), "Bağlantı açılamadı: Something odd");
        let drm_warning = "WARNING: [youtube] x: Some tv client https formats have been skipped as they are DRM protected.\nERROR: unable to download video data: HTTP Error 403: Forbidden";
        assert!(humanize_error(drm_warning, true).contains("403"), "a DRM warning must not hide the real error");
        assert!(humanize_error("ERROR: [youtube] aDrMxxxxxxx: Video unavailable", true).contains("kullanılamıyor"));
        assert!(humanize_error("ERROR: [x] y: This video is DRM protected", true).contains("DRM"));
        assert!(is_retryable("ERROR: unable to download video data: HTTP Error 403: Forbidden"));
        assert!(!is_retryable("ERROR: Private video"));
    }

    #[test]
    fn network_drops_are_retried() {
        // As yt-dlp reported them on macOS while the network was gone.
        let dns = "ERROR: \r[download] Got error: HTTPSConnectionPool(host='rr3---sn-u0g3uxax3-xncs.googlevideo.com', port=443): Max retries exceeded with url: /videoplayback (Caused by NameResolutionError(\"HTTPSConnection(host='rr3---sn-u0g3uxax3-xncs.googlevideo.com', port=443): Failed to resolve 'rr3---sn-u0g3uxax3-xncs.googlevideo.com' ([Errno 8] nodename nor servname provided, or not known)\")). Giving up after 10 retries";
        assert!(is_retryable(dns) && humanize_error(dns, true).contains("Siteye ulaşılamadı"));
        let refused = "ERROR: \r[download] Got error: [Errno 61] Connection refused. Giving up after 10 retries";
        assert!(is_retryable(refused));
        let offline_start = "ERROR: [youtube] aqz-KE-bpKQ: Unable to download API page: Failed to resolve 'www.youtube.com' ([Errno 8] nodename nor servname provided, or not known) (caused by TransportError(\"Failed to resolve 'www.youtube.com' ([Errno 8] nodename nor servname provided, or not known)\"))";
        assert!(is_retryable(offline_start) && humanize_error(offline_start, false).contains("Siteye ulaşılamadı"));
        assert!(humanize_error("ERROR: [Errno 65] No route to host", true).contains("Siteye ulaşılamadı"));
        assert!(!is_retryable("ERROR: [youtube] x: Video unavailable"));
        assert!(is_network_error(dns) && is_network_error(offline_start));
        assert!(!is_network_error("ERROR: unable to download video data: HTTP Error 403: Forbidden"));
        assert!(!is_network_error("ERROR: Unable to rename file: [WinError 32] The process cannot access the file"));
    }

    #[test]
    fn humanizes_windows_errors() {
        assert!(humanize_error("ERROR: [WinError 112] There is not enough space on the disk", true).contains("yer kalmadı"));
        // yt-dlp only writes in the work folder, so this is a lock, not the download folder.
        let denied = "ERROR: Unable to rename file: [WinError 5] Erişim engellendi: 'C:\\x.part' -> 'C:\\x.mp4'";
        assert!(is_retryable(denied) && humanize_error(denied, true).contains("Windows Güvenliği"));
        assert!(!humanize_error("ERROR: [WinError 53] The network path was not found", true).contains("yazılamıyor"));
        assert!(!is_retryable("ERROR: [WinError 53] The network path was not found"), "[WinError 5] must not match 53");
        let locked = "ERROR: Unable to rename file: [WinError 32] The process cannot access the file because it is being used by another process";
        assert!(is_retryable(locked) && humanize_error(locked, true).contains("Windows Güvenliği"));
        assert!(is_retryable("ERROR: ('Connection aborted.', ConnectionResetError(10054, 'An existing connection was forcibly closed by the remote host', None, 10054, None))"));
        // Turkish Windows words socket errors in Turkish.
        let reset = "ERROR: [youtube] x: Unable to download API page: ('Connection aborted.', ConnectionResetError(10054, 'Varolan bir bağlantı uzaktaki bir ana bilgisayar tarafından zorla kapatıldı', None, 10054, None))";
        assert!(is_retryable(reset));
        assert!(is_retryable("ERROR: <urlopen error [WinError 10053] Kurulan bir bağlantı ana bilgisayarınızdaki yazılım tarafından iptal edildi>"));
        let timeout = "ERROR: [generic] x: Unable to download webpage: <urlopen error [WinError 10060] Bağlanılan uygun olarak belli bir süre içinde yanıt vermediğinden bağlantı kurulamadı>";
        assert!(is_retryable(timeout) && humanize_error(timeout, false).contains("Siteye ulaşılamadı"));
        assert!(humanize_error("ERROR: [x] y: <urlopen error [WinError 10051] Ulaşılamayan bir ağ>", false).contains("Siteye ulaşılamadı"));
    }

    #[test]
    fn permission_denied_wording() {
        let denied = "ERROR: unable to open for writing: [Errno 13] Permission denied: '/x/y.part'";
        let expected = if cfg!(windows) { "Windows Güvenliği" } else { "yazılamıyor" };
        assert!(humanize_error(denied, true).contains(expected));
        assert!(!is_retryable(denied));
    }

    #[test]
    fn file_names_fit_windows_paths() {
        // C:\Users\<name>\AppData\Local\com.alp.downloader\jobs\job-12\parts
        let dir = |user: &str| format!(r"C:\Users\{user}\AppData\Local\com.alp.downloader\jobs\job-12\parts").len();
        for user in ["Alp", "abdurrahman.ozturk.CORP", "a-very-long-domain-account-name.CONTOSO-EUROPE"] {
            let title = windows_title_bytes(dir(user));
            // Longest fragment name: ".f" + a 33-character format id + ".webm.part-Frag12345.part".
            assert!(dir(user) + 1 + title + 2 + 33 + 25 <= 259, "{user}: {title}");
        }
        assert_eq!(windows_title_bytes(dir("Alp")), 135);
        assert_eq!(windows_title_bytes(10), 150, "short folders keep the full 150 bytes");
        assert_eq!(windows_title_bytes(400), 40, "a title never shrinks to nothing");
    }

    #[cfg(windows)]
    #[test]
    fn windows_template_follows_the_work_folder() {
        let home = Path::new(r"C:\Users\abdurrahman.ozturk.CORP\AppData\Local\com.alp.downloader\jobs\job-12\out");
        let temp = Path::new(r"C:\Users\abdurrahman.ozturk.CORP\AppData\Local\com.alp.downloader\jobs\job-12\parts");
        let a: Vec<String> = download_args(&req(Kind::Mp4), home, temp, true, false).into_iter().map(|x| x.to_string_lossy().into_owned()).collect();
        let expected = format!("%(title).{}B.%(ext)s", windows_title_bytes(temp.as_os_str().len()));
        assert!(a.windows(2).any(|w| w[0] == "-o" && w[1] == expected), "{a:?}");
    }

    #[cfg(not(windows))]
    #[test]
    fn mac_template_is_unchanged() {
        assert!(args(Kind::Mp4).windows(2).any(|w| w[0] == "-o" && w[1] == "%(title).150B.%(ext)s"));
    }

    #[test]
    fn parses_video_info() {
        let json = r#"{"_type":"video","id":"abc","title":"Artist - Song (Official Video)","uploader":"Up","duration":200.4,
          "webpage_url":"https://www.youtube.com/watch?v=abc","license":"Creative Commons Attribution license (reuse allowed)",
          "thumbnail":"https://i.ytimg.com/x.jpg","extractor_key":"Youtube",
          "formats":[
            {"format_id":"140","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","abr":129.4,"filesize":3000},
            {"format_id":"140-drc","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","abr":130.0,"filesize":9999},
            {"format_id":"299","ext":"mp4","vcodec":"avc1.64002A","acodec":"none","width":1920,"height":1080,"filesize":50000},
            {"format_id":"303","ext":"webm","vcodec":"vp9","acodec":"none","width":1920,"height":1080,"filesize":40000},
            {"format_id":"308","ext":"webm","vcodec":"vp9","acodec":"none","width":2560,"height":1440,"filesize_approx":80000},
            {"format_id":"sb0","ext":"mhtml","vcodec":"none","acodec":"none"},
            {"format_id":"short","ext":"mp4","vcodec":"avc1","acodec":"none","width":1080,"height":1920,"filesize":7}
          ]}"#;
        let MediaInfo::Video(v) = parse_info_json(json, false).unwrap() else { panic!() };
        assert_eq!(v.resolutions, vec![1440, 1080]);
        assert!(v.creative_commons);
        assert!(v.title_has_separator);
        assert!(!v.has_music_tags);
        assert_eq!(v.duration, Some(200));
        assert_eq!(v.sizes.m4a, Some(3000), "DRC variants must be skipped");
        assert_eq!(v.sizes.mp4.get(&1080), Some(&53000), "H.264 preferred up to 1080p");
        assert_eq!(v.sizes.mp4.get(&1440), Some(&83000));
        assert_eq!(v.sizes.webm.get(&1080), Some(&43000), "VP9 + best audio for WEBM");
        assert_eq!(v.sizes.wav, Some(200 * 192_000));
        assert_eq!(v.site.as_deref(), Some("Youtube"), "falls back to the extractor key without a domain");
    }

    #[test]
    fn unknown_codecs_count_as_available() {
        // Direct .mp4 link / archive.org: yt-dlp reports no codecs at all.
        let direct = r#"{"id":"x","title":"t","formats":[{"format_id":"mp4","ext":"mp4","url":"https://a/v.mp4"}]}"#;
        let MediaInfo::Video(v) = parse_info_json(direct, false).unwrap() else { panic!() };
        assert!(v.has_video && v.has_audio);
        assert!(!v.webm, "an H.264-only site cannot give WEBM");
        // Direct audio link: vcodec "none", acodec missing.
        let audio = r#"{"id":"x","title":"t","formats":[{"format_id":"a","ext":"m4a","vcodec":"none"}]}"#;
        let MediaInfo::Video(a) = parse_info_json(audio, false).unwrap() else { panic!() };
        assert!(a.has_audio && !a.has_video);
        // H.264-only video gets no WEBM estimate.
        let h264 = r#"{"id":"x","title":"t","formats":[{"format_id":"1","vcodec":"avc1","acodec":"mp4a","width":1280,"height":720,"filesize":5}]}"#;
        let MediaInfo::Video(h) = parse_info_json(h264, false).unwrap() else { panic!() };
        assert!(h.sizes.webm.is_empty() && !h.webm);
        assert_eq!(h.sizes.mp4.get(&720), Some(&5), "a combined stream gets no extra audio added");
    }

    #[test]
    fn license_and_site_names() {
        assert!(is_creative_commons("Creative Commons Attribution license (reuse allowed)"));
        assert!(is_creative_commons("https://creativecommons.org/licenses/by-nc-nd/3.0/"));
        assert!(is_creative_commons("http://creativecommons.org/licenses/publicdomain/"));
        assert!(is_creative_commons("by-nc"));
        assert!(!is_creative_commons("Standard YouTube License"));
        let v: Value = serde_json::from_str(r#"{"webpage_url_domain":"www.vimeo.com","extractor_key":"Vimeo"}"#).unwrap();
        assert_eq!(site_name(&v).as_deref(), Some("vimeo.com"));
        let g: Value = serde_json::from_str(r#"{"extractor_key":"Generic"}"#).unwrap();
        assert_eq!(site_name(&g), None);
    }

    #[test]
    fn rejects_live_and_drm() {
        assert!(parse_info_json(r#"{"id":"x","title":"t","live_status":"is_live","formats":[]}"#, false).is_err());
        let drm = r#"{"id":"x","title":"t","formats":[{"format_id":"1","vcodec":"avc1","acodec":"aac","has_drm":true}]}"#;
        assert!(parse_info_json(drm, false).unwrap_err().to_string().contains("DRM"));
    }

    #[test]
    fn flattens_channel_tabs_and_skips_list_links() {
        let channel = r#"{"_type":"playlist","id":"UC1","title":"Chan","entries":[
            {"_type":"playlist","id":"UC1","title":"Chan - Videos","webpage_url":"https://www.youtube.com/@c/videos","entries":[
                {"_type":"url","id":"a","title":"A","url":"https://www.youtube.com/watch?v=a"},
                {"_type":"url","id":"b","title":"B","url":"https://www.youtube.com/watch?v=b"}]},
            {"_type":"playlist","id":"UC1","title":"Chan - Shorts","webpage_url":"https://www.youtube.com/@c/shorts","entries":[
                {"_type":"url","id":"c","title":"C","url":"https://www.youtube.com/shorts/c"}]}
        ]}"#;
        let MediaInfo::Playlist(p) = parse_info_json(channel, false).unwrap() else { panic!() };
        assert_eq!(p.entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        let sets = r#"{"_type":"playlist","id":"u","title":"Sets","entries":[
            {"_type":"url","title":"Ecclesia","url":"https://soundcloud.com/forss/sets/ecclesia"},
            {"_type":"url","title":"Track","url":"https://soundcloud.com/forss/flickermood"}]}"#;
        let MediaInfo::Playlist(p) = parse_info_json(sets, false).unwrap() else { panic!() };
        assert_eq!(p.entries.len(), 1, "a SoundCloud set is a list, not a track");
        assert_eq!(p.entries[0].title, "Track");
        let tabs = r#"{"_type":"playlist","id":"UC1","title":"Chan - Playlists","entries":[
            {"_type":"url","ie_key":"YoutubeTab","id":"PL1","title":"L","url":"https://www.youtube.com/playlist?list=PL1"}]}"#;
        assert!(parse_info_json(tabs, false).unwrap_err().to_string().contains("oynatma listelerinden"));
    }

    #[test]
    fn parses_playlists() {
        let json = r#"{"_type":"playlist","id":"PL1","title":"List","entries":[
            {"id":"a","title":"One","url":"https://www.youtube.com/watch?v=a","duration":10},
            {"id":"b","title":"[Private video]","url":"https://www.youtube.com/watch?v=b"},
            {"id":"a","title":"One again","url":"https://www.youtube.com/watch?v=a"},
            {"id":"c","title":"Bad","url":"javascript:alert(1)"}
        ]}"#;
        let MediaInfo::Playlist(p) = parse_info_json(json, false).unwrap() else { panic!() };
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].title, "One");
        assert!(!p.truncated);
    }
}

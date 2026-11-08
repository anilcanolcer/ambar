//! Downloads, verifies and updates the three helper programs Ambar drives:
//! yt-dlp (media extraction), FFmpeg (conversion) and Deno (JavaScript runtime
//! yt-dlp needs for YouTube). Nothing is bundled with the app. Each engine is
//! fetched from its official release channel on the user's own machine and
//! checked against the SHA-256 sum the publisher puts out. On macOS, where
//! the publisher signs with an Apple Developer ID, the signature must also
//! carry that publisher's Team ID. The Windows builds are not signed by a
//! publisher we could pin, so there the published sum is the check.
//!
//! FFmpeg comes from Martin Riedl's builds on macOS and from Gyan Doshi's
//! (the Windows builds ffmpeg.org links to) on Windows.
//!
//! Every installed version lives in its own folder (engines/<engine>/<dir>)
//! and the manifest points at the current one. An update therefore never
//! touches files a running download is using: running jobs finish with the
//! old version, new jobs pick up the new one, and old folders are removed at
//! the next launch, when nothing can be running.

use crate::error::{msg, AppResult};
use crate::settings::Channel;
use crate::util::{now_secs, remove_dir_quiet, write_json_atomic};
use futures_util::StreamExt;
use reqwest::header::LOCATION;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Apple Developer Team IDs of the publishers whose signed builds we accept.
#[cfg(not(windows))]
pub const DENO_TEAM_ID: &str = "2H4KBF436B"; // Deno Land Inc.
#[cfg(not(windows))]
pub const FFMPEG_TEAM_ID: &str = "KU3N25YGLU"; // Martin Riedl (ffmpeg.martin-riedl.de)

const USER_AGENT: &str = concat!("Ambar/", env!("CARGO_PKG_VERSION"));
#[cfg(not(windows))]
const FFMPEG_HOST: &str = "https://ffmpeg.martin-riedl.de/";
/// Where Gyan's "latest release" link must lead.
#[cfg(any(windows, test))]
const GYAN_PACKAGES: &str = "https://www.gyan.dev/ffmpeg/builds/packages/";

/// File names of the programs inside an engine's version folder.
#[cfg(not(windows))]
pub mod bin {
    pub const YTDLP: &str = "yt-dlp_macos";
    pub const DENO: &str = "deno";
    pub const FFMPEG: &str = "ffmpeg";
    pub const FFPROBE: &str = "ffprobe";
}

/// File names of the programs inside an engine's version folder.
#[cfg(windows)]
pub mod bin {
    /// Needs the "_internal" folder that sits next to it.
    pub const YTDLP: &str = "yt-dlp.exe";
    pub const DENO: &str = "deno.exe";
    pub const FFMPEG: &str = "ffmpeg.exe";
    pub const FFPROBE: &str = "ffprobe.exe";
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineId {
    YtDlp,
    Ffmpeg,
    Deno,
}

impl EngineId {
    pub const ALL: [EngineId; 3] = [EngineId::YtDlp, EngineId::Ffmpeg, EngineId::Deno];

    pub fn key(self) -> &'static str {
        match self {
            EngineId::YtDlp => "yt-dlp",
            EngineId::Ffmpeg => "ffmpeg",
            EngineId::Deno => "deno",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EngineId::YtDlp => "yt-dlp",
            EngineId::Ffmpeg => "FFmpeg",
            EngineId::Deno => "Deno",
        }
    }

    /// The programs this engine installs, by file name in its version folder.
    fn file_names(self) -> &'static [&'static str] {
        match self {
            EngineId::YtDlp => &[bin::YTDLP],
            EngineId::Deno => &[bin::DENO],
            EngineId::Ffmpeg => &[bin::FFMPEG, bin::FFPROBE],
        }
    }

    /// yt-dlp has to keep up with YouTube, so it is checked far more often.
    fn check_interval_secs(self) -> u64 {
        match self {
            EngineId::YtDlp => 6 * 3600,
            EngineId::Ffmpeg | EngineId::Deno => 7 * 24 * 3600,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Checking,
    Downloading,
    Installing,
    Done,
    UpToDate,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineProgress {
    pub engine: EngineId,
    pub phase: Phase,
    pub downloaded: u64,
    pub fraction: Option<f64>,
    pub message: Option<String>,
    pub version: Option<String>,
}

impl EngineProgress {
    fn new(engine: EngineId, phase: Phase) -> Self {
        EngineProgress { engine, phase, downloaded: 0, fraction: None, message: None, version: None }
    }
}

pub type ProgressFn = dyn Fn(EngineProgress) + Send + Sync;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub engines: BTreeMap<String, InstalledEngine>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledEngine {
    pub version: String,
    pub display_version: String,
    #[serde(default)]
    pub channel: Option<String>,
    pub installed_at: u64,
    pub last_checked: u64,
    /// Folder under engines/<engine>/ holding this version. Empty means the
    /// files sit directly in engines/<engine>/ (layout of early builds).
    #[serde(default)]
    pub dir: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub id: EngineId,
    pub name: &'static str,
    pub installed: bool,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnginesState {
    pub all_installed: bool,
    pub engines: Vec<EngineStatus>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOutcome {
    pub engine: EngineId,
    pub from: Option<String>,
    pub to: String,
}

/// HTTP clients: one that follows redirects for downloads and one that
/// does not, used to read "latest release" redirects without the GitHub API
/// (which is rate-limited for anonymous callers).
#[derive(Clone)]
pub struct Net {
    client: reqwest::Client,
    no_redirect: reqwest::Client,
}

impl Net {
    pub fn new() -> AppResult<Net> {
        let builder = || {
            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(Duration::from_secs(20))
                .read_timeout(Duration::from_secs(60))
        };
        Ok(Net {
            client: builder().build()?,
            no_redirect: builder().redirect(reqwest::redirect::Policy::none()).build()?,
        })
    }

    /// The client that follows redirects.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

#[derive(Clone, Debug)]
pub struct EngineDirs {
    pub root: PathBuf,
}

impl EngineDirs {
    pub fn new(root: PathBuf) -> Self {
        EngineDirs { root }
    }

    /// Parent folder that holds every installed version of one engine.
    fn base(&self, id: EngineId) -> PathBuf {
        self.root.join(id.key())
    }

    fn current_in(&self, manifest: &Manifest, id: EngineId) -> PathBuf {
        match manifest.engines.get(id.key()).map(|e| e.dir.as_str()) {
            Some(dir) if !dir.is_empty() => self.base(id).join(dir),
            _ => self.base(id),
        }
    }

    /// Folder of the version currently in use.
    #[cfg(test)]
    pub fn current(&self, id: EngineId) -> PathBuf {
        self.current_in(&self.load_manifest(), id)
    }

    fn required_in(&self, manifest: &Manifest, id: EngineId) -> Vec<PathBuf> {
        let dir = self.current_in(manifest, id);
        id.file_names().iter().map(|name| dir.join(name)).collect()
    }

    fn installed_in(&self, manifest: &Manifest, id: EngineId) -> bool {
        self.required_in(manifest, id).iter().all(|p| p.is_file())
    }

    pub fn is_installed(&self, id: EngineId) -> bool {
        self.installed_in(&self.load_manifest(), id)
    }

    pub fn all_installed(&self) -> bool {
        let manifest = self.load_manifest();
        EngineId::ALL.iter().all(|id| self.installed_in(&manifest, *id))
    }

    /// Paths of the three programs as one consistent snapshot, or None if
    /// anything is missing.
    pub fn programs(&self) -> Option<Programs> {
        let manifest = self.load_manifest();
        if !EngineId::ALL.iter().all(|id| self.installed_in(&manifest, *id)) {
            return None;
        }
        Some(Programs {
            ytdlp: self.current_in(&manifest, EngineId::YtDlp).join(bin::YTDLP),
            deno: self.current_in(&manifest, EngineId::Deno).join(bin::DENO),
            ffmpeg_dir: self.current_in(&manifest, EngineId::Ffmpeg),
        })
    }

    /// Deletes every version folder that is not current, plus leftovers of
    /// interrupted installs. Only call this when no download can be running
    /// (at start-up).
    pub fn remove_old_versions(&self) {
        remove_dir_quiet(&self.root.join(".staging"));
        let manifest = self.load_manifest();
        for id in EngineId::ALL {
            remove_dir_quiet(&self.root.join(format!(".old-{}", id.key())));
            let Some(current) = manifest.engines.get(id.key()).map(|e| e.dir.clone()) else { continue };
            if current.is_empty() || !self.installed_in(&manifest, id) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(self.base(id)) else { continue };
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy() == current {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() {
                    remove_dir_quiet(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }

    pub fn load_manifest(&self) -> Manifest {
        std::fs::read(self.manifest_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save_manifest(&self, m: &Manifest) -> AppResult<()> {
        write_json_atomic(&self.manifest_path(), m)
    }

    pub fn state(&self) -> EnginesState {
        let manifest = self.load_manifest();
        let engines: Vec<EngineStatus> = EngineId::ALL
            .iter()
            .map(|id| {
                let installed = self.installed_in(&manifest, *id);
                EngineStatus {
                    id: *id,
                    name: id.label(),
                    installed,
                    version: if installed {
                        manifest.engines.get(id.key()).map(|e| e.display_version.clone())
                    } else {
                        None
                    },
                }
            })
            .collect();
        EnginesState { all_installed: engines.iter().all(|e| e.installed), engines }
    }
}

#[derive(Clone, Debug)]
pub struct Programs {
    pub ytdlp: PathBuf,
    pub deno: PathBuf,
    pub ffmpeg_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Artifact {
    pub url: String,
    pub file_name: String,
    pub sha256: String,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub id: EngineId,
    pub version: String,
    pub display_version: String,
    pub channel: Option<String>,
    pub artifacts: Vec<Artifact>,
}

fn deno_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

// Ambar ships for x64 Windows only: Gyan's FFmpeg builds are x64, and the
// ARM yt-dlp build names its program differently (yt-dlp_arm64.exe).
#[cfg(all(windows, not(target_arch = "x86_64")))]
compile_error!("Ambar for Windows is x64-only");

/// yt-dlp's folder build for this system (the program plus its libraries).
fn ytdlp_asset() -> &'static str {
    if cfg!(windows) {
        "yt-dlp_win.zip"
    } else {
        "yt-dlp_macos.zip"
    }
}

fn deno_asset() -> String {
    let system = if cfg!(windows) { "pc-windows-msvc" } else { "apple-darwin" };
    format!("deno-{}-{system}.zip", deno_arch())
}

#[cfg(not(windows))]
fn ffmpeg_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    }
}

/// "https://host/a/b" -> "https://host"
fn origin(url: &str) -> &str {
    match url.find("://") {
        Some(i) => match url[i + 3..].find('/') {
            Some(j) => &url[..i + 3 + j],
            None => url,
        },
        None => url,
    }
}

fn absolute(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        location.to_string()
    } else if location.starts_with('/') {
        format!("{}{}", origin(base), location)
    } else {
        location.to_string()
    }
}

fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

pub fn tag_from_location(location: &str) -> AppResult<String> {
    let tag = location
        .split("/releases/tag/")
        .nth(1)
        .map(|t| t.trim_end_matches('/'))
        .unwrap_or_default();
    if valid_tag(tag) {
        Ok(tag.to_string())
    } else {
        Err(msg("Güncel sürüm bilgisi okunamadı."))
    }
}

/// ".../download/macos/arm64/1789931890_9.0.2/ffmpeg.zip" -> "1789931890_9.0.2"
#[cfg(any(not(windows), test))]
pub fn ffmpeg_build_id(location: &str, name: &str) -> AppResult<String> {
    let suffix = format!("/{name}.zip");
    let without = location
        .strip_suffix(&suffix)
        .ok_or_else(|| msg("FFmpeg sürüm adresi beklenmedik biçimde."))?;
    let build = without.rsplit('/').next().unwrap_or_default();
    if valid_tag(build) {
        Ok(build.to_string())
    } else {
        Err(msg("FFmpeg sürüm adresi beklenmedik biçimde."))
    }
}

/// ".../packages/ffmpeg-9.0.2-essentials_build.zip" -> "9.0.2"
#[cfg(any(windows, test))]
pub fn gyan_version(location: &str) -> AppResult<String> {
    let version = location
        .strip_prefix(GYAN_PACKAGES)
        .and_then(|file| file.strip_prefix("ffmpeg-"))
        .and_then(|file| file.strip_suffix("-essentials_build.zip"))
        .unwrap_or_default();
    if valid_tag(version) {
        Ok(version.to_string())
    } else {
        Err(msg("FFmpeg sürüm adresi beklenmedik biçimde."))
    }
}

/// Finds the hash for `file_name` in a checksum file. Accepts the common
/// "<hash>  <name>" / "<hash> *<name>" layouts, a bare single hash and
/// PowerShell's Get-FileHash output (Deno's Windows builds).
pub fn find_checksum(text: &str, file_name: &str) -> Option<String> {
    let is_hash = |s: &str| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit());
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    for line in &lines {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(name)) = (parts.next(), parts.next()) else { continue };
        if is_hash(hash) && name.trim_start_matches('*') == file_name {
            return Some(hash.to_ascii_lowercase());
        }
    }
    // A file that holds nothing but one bare hash (no file name at all).
    if let [only] = lines.as_slice() {
        let mut tokens = only.split_whitespace();
        if let (Some(hash), None) = (tokens.next(), tokens.next()) {
            if is_hash(hash) {
                return Some(hash.to_ascii_lowercase());
            }
        }
    }
    // "Hash : <hash>" plus "Path : C:\...\<name>". Of the algorithms
    // Get-FileHash offers, only SHA-256 gives 64 hex digits.
    let field = |key: &str| {
        lines.iter().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim()))
    };
    if let Some(hash) = field("Hash").filter(|h| is_hash(h)) {
        let named = match field("Path") {
            Some(path) => path.rsplit(['\\', '/']).next() == Some(file_name),
            None => true,
        };
        if named {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

async fn latest_redirect(net: &Net, url: &str) -> AppResult<String> {
    let resp = net.no_redirect.get(url).send().await?;
    if !resp.status().is_redirection() {
        return Err(msg(format!("Güncel sürüm bilgisi alınamadı (HTTP {}).", resp.status().as_u16())));
    }
    let loc = resp
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| msg("Güncel sürüm bilgisi alınamadı."))?;
    Ok(absolute(url, loc))
}

async fn get_text(net: &Net, url: &str) -> AppResult<String> {
    let resp = net.client.get(url).send().await?.error_for_status()?;
    Ok(resp.text().await?)
}

pub async fn resolve_latest(net: &Net, id: EngineId, channel: Channel) -> AppResult<Release> {
    match id {
        EngineId::YtDlp => {
            let repo = match channel {
                Channel::Nightly => "yt-dlp/yt-dlp-nightly-builds",
                Channel::Stable => "yt-dlp/yt-dlp",
            };
            let tag = tag_from_location(
                &latest_redirect(net, &format!("https://github.com/{repo}/releases/latest")).await?,
            )?;
            let base = format!("https://github.com/{repo}/releases/download/{tag}");
            let file = ytdlp_asset();
            let sums = get_text(net, &format!("{base}/SHA2-256SUMS")).await?;
            let sha256 = find_checksum(&sums, file).ok_or_else(|| msg("yt-dlp doğrulama özeti bulunamadı."))?;
            Ok(Release {
                id,
                version: tag.clone(),
                display_version: tag,
                channel: Some(channel.key().to_string()),
                artifacts: vec![Artifact { url: format!("{base}/{file}"), file_name: file.into(), sha256 }],
            })
        }
        EngineId::Deno => {
            let tag = tag_from_location(
                &latest_redirect(net, "https://github.com/denoland/deno/releases/latest").await?,
            )?;
            let file = deno_asset();
            let url = format!("https://github.com/denoland/deno/releases/download/{tag}/{file}");
            let sums = get_text(net, &format!("{url}.sha256sum")).await?;
            let sha256 = find_checksum(&sums, &file).ok_or_else(|| msg("Deno doğrulama özeti bulunamadı."))?;
            Ok(Release {
                id,
                display_version: tag.trim_start_matches('v').to_string(),
                version: tag,
                channel: None,
                artifacts: vec![Artifact { url, file_name: file, sha256 }],
            })
        }
        EngineId::Ffmpeg => resolve_ffmpeg(net).await,
    }
}

#[cfg(not(windows))]
async fn resolve_ffmpeg(net: &Net) -> AppResult<Release> {
    let mut artifacts = Vec::new();
    let mut build: Option<String> = None;
    for name in ["ffmpeg", "ffprobe"] {
        let redirect = format!(
            "{FFMPEG_HOST}redirect/latest/macos/{}/release/{name}.zip",
            ffmpeg_arch()
        );
        let loc = latest_redirect(net, &redirect).await?;
        if !loc.starts_with(FFMPEG_HOST) {
            return Err(msg("FFmpeg beklenmeyen bir adrese yönlendirdi, kurulum durduruldu."));
        }
        let this_build = ffmpeg_build_id(&loc, name)?;
        match &build {
            None => build = Some(this_build),
            Some(b) if *b != this_build => {
                return Err(msg("FFmpeg sürümleri şu an eşleşmiyor, biraz sonra tekrar dene."))
            }
            Some(_) => {}
        }
        let file = format!("{name}.zip");
        let sums = get_text(net, &format!("{loc}.sha256")).await?;
        let sha256 =
            find_checksum(&sums, &file).ok_or_else(|| msg("FFmpeg doğrulama özeti bulunamadı."))?;
        artifacts.push(Artifact { url: loc, file_name: file, sha256 });
    }
    let version = build.unwrap_or_default();
    let display_version = version.split_once('_').map(|(_, v)| v).unwrap_or(&version).to_string();
    Ok(Release { id: EngineId::Ffmpeg, version, display_version, channel: None, artifacts })
}

/// Gyan's "essentials" release build: one zip with ffmpeg.exe and
/// ffprobe.exe. It is x64 only; ARM PCs run it emulated.
#[cfg(windows)]
async fn resolve_ffmpeg(net: &Net) -> AppResult<Release> {
    let loc = latest_redirect(net, "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip").await?;
    if !loc.starts_with(GYAN_PACKAGES) {
        return Err(msg("FFmpeg beklenmeyen bir adrese yönlendirdi, kurulum durduruldu."));
    }
    let version = gyan_version(&loc)?;
    let file = format!("ffmpeg-{version}-essentials_build.zip");
    let sums = get_text(net, &format!("{loc}.sha256")).await?;
    let sha256 = find_checksum(&sums, &file).ok_or_else(|| msg("FFmpeg doğrulama özeti bulunamadı."))?;
    Ok(Release {
        id: EngineId::Ffmpeg,
        display_version: version.clone(),
        version,
        channel: None,
        artifacts: vec![Artifact { url: loc, file_name: file, sha256 }],
    })
}

async fn download_artifacts(
    net: &Net,
    rel: &Release,
    dir: &Path,
    progress: &ProgressFn,
) -> AppResult<Vec<PathBuf>> {
    let count = rel.artifacts.len().max(1) as f64;
    let mut downloaded = 0u64;
    let mut paths = Vec::new();
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    for (index, artifact) in rel.artifacts.iter().enumerate() {
        let resp = net.client.get(&artifact.url).send().await?.error_for_status()?;
        let total = resp.content_length();
        let path = dir.join(&artifact.file_name);
        let mut file = tokio::fs::File::create(&path).await?;
        let mut hasher = Sha256::new();
        let mut stream = resp.bytes_stream();
        let mut this_file = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            this_file += chunk.len() as u64;
            downloaded += chunk.len() as u64;
            if last_emit.elapsed() >= Duration::from_millis(120) {
                last_emit = Instant::now();
                let within = total.map(|t| (this_file as f64 / t.max(1) as f64).min(1.0)).unwrap_or(0.0);
                progress(EngineProgress {
                    downloaded,
                    fraction: Some((index as f64 + within) / count),
                    ..EngineProgress::new(rel.id, Phase::Downloading)
                });
            }
        }
        file.flush().await?;
        drop(file);
        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(&artifact.sha256) {
            return Err(msg(format!(
                "{} doğrulanamadı: dosya özeti yayınlanan özetle uyuşmuyor. Tekrar dene.",
                artifact.file_name
            )));
        }
        paths.push(path);
    }
    progress(EngineProgress {
        downloaded,
        fraction: Some(1.0),
        ..EngineProgress::new(rel.id, Phase::Downloading)
    });
    Ok(paths)
}

#[cfg(not(windows))]
async fn extract_zip(zip: &Path, dest: &Path) -> AppResult<()> {
    // ditto ships with macOS and keeps symlinks and permissions intact,
    // which the yt-dlp folder build relies on.
    let status = Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(zip)
        .arg(dest)
        .stdin(Stdio::null())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(msg("İndirilen arşiv açılamadı."))
    }
}

/// Windows has no ditto, so archives are unpacked here, off the async
/// workers (FFmpeg's holds over 200 MB).
#[cfg(windows)]
async fn extract_zip(id: EngineId, zip: &Path, dest: &Path) -> AppResult<()> {
    let (zip, dest) = (zip.to_path_buf(), dest.to_path_buf());
    tokio::task::spawn_blocking(move || unzip(id, &zip, &dest))
        .await
        .map_err(|_| msg("İndirilen arşiv açılamadı."))?
}

#[cfg(windows)]
fn unzip(id: EngineId, zip: &Path, dest: &Path) -> AppResult<()> {
    let unreadable = |e: zip::result::ZipError| {
        eprintln!("[kova] zip error: {e}");
        msg("İndirilen arşiv açılamadı.")
    };
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?).map_err(unreadable)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(unreadable)?;
        // enclosed_name() refuses absolute paths and "..", so nothing can
        // be written outside `dest`.
        let name = entry.enclosed_name().ok_or_else(|| msg("İndirilen arşiv açılamadı."))?;
        let target = match id {
            EngineId::Ffmpeg => match ffmpeg_member(&name) {
                Some(file) => dest.join(file),
                None => continue,
            },
            _ => dest.join(&name),
        };
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&target)?;
        std::io::copy(&mut entry, &mut file)?;
    }
    Ok(())
}

/// "ffmpeg-9.0.2-essentials_build/bin/ffprobe.exe" -> "ffprobe.exe". The
/// rest of Gyan's archive (ffplay, documentation, licences) is skipped, and
/// the two programs land straight in the version folder.
#[cfg(any(windows, test))]
fn ffmpeg_member(entry: &Path) -> Option<&str> {
    let name = entry.file_name()?.to_str()?;
    let in_bin = entry.parent()?.file_name()? == "bin";
    (in_bin && EngineId::Ffmpeg.file_names().contains(&name)).then_some(name)
}

#[cfg(not(windows))]
async fn verify_signature(path: &Path, team_id: &str) -> AppResult<()> {
    let requirement = format!("=anchor apple generic and certificate leaf[subject.OU] = \"{team_id}\"");
    let out = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "-R"])
        .arg(&requirement)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .await?;
    if out.status.success() {
        Ok(())
    } else {
        Err(msg(format!(
            "{} imzası doğrulanamadı. Dosya resmi yayıncıdan gelmiyor olabilir, kurulum durduruldu.",
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
        )))
    }
}

async fn run_version(bin: &Path, arg: &str) -> AppResult<String> {
    let name = bin.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut cmd = Command::new(bin);
    cmd.arg(arg).env("PYTHONUTF8", "1").stdin(Stdio::null()).kill_on_drop(true);
    crate::proc::hide_console(&mut cmd);
    let fut = cmd.output();
    // The first launch of a freshly downloaded binary can take several
    // seconds while macOS (or Windows Defender) scans it.
    let out = tokio::time::timeout(Duration::from_secs(120), fut)
        .await
        .map_err(|_| msg(format!("{name} zamanında yanıt vermedi.")))??;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(msg(format!("{name} çalıştırılamadı: {}", err.lines().last().unwrap_or("bilinmeyen hata"))));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().trim().to_string())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> AppResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

/// What to say when a program is missing after unpacking. On Windows the
/// archive was checked, so the likely cause is Defender removing the file.
fn missing_file_text(id: EngineId, name: &str) -> String {
    if cfg!(windows) {
        format!("{} kurulamadı: {name} bulunamadı. Windows Güvenliği dosyayı engellemiş olabilir.", id.label())
    } else {
        format!("{} arşivinde beklenen dosya yok.", id.label())
    }
}

async fn check_installed_files(id: EngineId, out: &Path) -> AppResult<()> {
    let require = |name: &str| -> AppResult<PathBuf> {
        let p = out.join(name);
        if p.is_file() {
            #[cfg(unix)]
            make_executable(&p)?;
            Ok(p)
        } else {
            Err(msg(missing_file_text(id, name)))
        }
    };
    match id {
        EngineId::YtDlp => {
            let bin = require(bin::YTDLP)?;
            run_version(&bin, "--version").await?;
        }
        EngineId::Deno => {
            let bin = require(bin::DENO)?;
            #[cfg(not(windows))]
            verify_signature(&bin, DENO_TEAM_ID).await?;
            run_version(&bin, "--version").await?;
        }
        EngineId::Ffmpeg => {
            for &name in id.file_names() {
                let bin = require(name)?;
                #[cfg(not(windows))]
                verify_signature(&bin, FFMPEG_TEAM_ID).await?;
                run_version(&bin, "-version").await?;
            }
        }
    }
    Ok(())
}

/// Downloads, verifies and swaps one engine into place. The old version is
/// only removed after the new one passed every check.
pub async fn install_release(dirs: &EngineDirs, net: &Net, rel: &Release, progress: &ProgressFn) -> AppResult<InstalledEngine> {
    let stage = dirs.root.join(".staging").join(rel.id.key());
    remove_dir_quiet(&stage);
    let result = async {
        let dl = stage.join("dl");
        let out = stage.join("out");
        std::fs::create_dir_all(&dl)?;
        std::fs::create_dir_all(&out)?;
        let archives = download_artifacts(net, rel, &dl, progress).await?;
        progress(EngineProgress::new(rel.id, Phase::Installing));
        for archive in &archives {
            #[cfg(not(windows))]
            extract_zip(archive, &out).await?;
            #[cfg(windows)]
            extract_zip(rel.id, archive, &out).await?;
        }
        check_installed_files(rel.id, &out).await?;

        // A fresh folder per install: the version in use is never touched.
        let now = now_secs();
        let safe_version: String = rel
            .version
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
            .collect();
        let dir_name = format!("{safe_version}-{now}");
        let base = dirs.base(rel.id);
        std::fs::create_dir_all(&base)?;
        // Defender may still be scanning the programs that were just run.
        crate::util::rename_retrying(&out, &base.join(&dir_name))?;
        Ok(InstalledEngine {
            version: rel.version.clone(),
            display_version: rel.display_version.clone(),
            channel: rel.channel.clone(),
            installed_at: now,
            last_checked: now,
            dir: dir_name,
        })
    }
    .await;
    remove_dir_quiet(&stage);
    result
}

fn record_install(dirs: &EngineDirs, id: EngineId, installed: &InstalledEngine) -> AppResult<()> {
    let mut manifest = dirs.load_manifest();
    manifest.engines.insert(id.key().to_string(), installed.clone());
    dirs.save_manifest(&manifest)
}

fn error_progress(id: EngineId, message: String) -> EngineProgress {
    EngineProgress { message: Some(message), ..EngineProgress::new(id, Phase::Error) }
}

/// First-run setup: installs every engine that is missing. Stops at the
/// first failure so the UI can show what went wrong.
pub async fn install_missing(dirs: &EngineDirs, net: &Net, channel: Channel, progress: &ProgressFn) -> AppResult<()> {
    std::fs::create_dir_all(&dirs.root)?;
    for id in EngineId::ALL {
        if dirs.is_installed(id) {
            let mut done = EngineProgress::new(id, Phase::Done);
            done.version = dirs.load_manifest().engines.get(id.key()).map(|e| e.display_version.clone());
            progress(done);
            continue;
        }
        progress(EngineProgress::new(id, Phase::Checking));
        let outcome = async {
            let rel = resolve_latest(net, id, channel).await?;
            let installed = install_release(dirs, net, &rel, progress).await?;
            record_install(dirs, id, &installed)?;
            Ok::<_, crate::error::AppError>(installed)
        }
        .await;
        match outcome {
            Ok(installed) => progress(EngineProgress {
                version: Some(installed.display_version),
                ..EngineProgress::new(id, Phase::Done)
            }),
            Err(e) => {
                progress(error_progress(id, e.to_string()));
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Checks installed engines for newer releases and installs them. Failures
/// are reported through `progress` but never abort the other engines, so an
/// offline start-up simply keeps the current versions.
pub async fn check_updates(
    dirs: &EngineDirs,
    net: &Net,
    channel: Channel,
    force: bool,
    progress: &ProgressFn,
) -> Vec<UpdateOutcome> {
    let mut outcomes = Vec::new();
    for id in EngineId::ALL {
        if !dirs.is_installed(id) {
            continue;
        }
        let manifest = dirs.load_manifest();
        let entry = manifest.engines.get(id.key()).cloned();
        let channel_changed =
            id == EngineId::YtDlp && entry.as_ref().and_then(|e| e.channel.as_deref()) != Some(channel.key());
        let due = entry
            .as_ref()
            .map(|e| now_secs().saturating_sub(e.last_checked) >= id.check_interval_secs())
            .unwrap_or(true);
        if !(force || due || channel_changed) {
            continue;
        }
        progress(EngineProgress::new(id, Phase::Checking));
        let rel = match resolve_latest(net, id, channel).await {
            Ok(rel) => rel,
            Err(e) => {
                progress(error_progress(id, e.to_string()));
                continue;
            }
        };
        if !channel_changed && entry.as_ref().map(|e| e.version.as_str()) == Some(rel.version.as_str()) {
            let mut manifest = dirs.load_manifest();
            if let Some(e) = manifest.engines.get_mut(id.key()) {
                e.last_checked = now_secs();
            }
            let _ = dirs.save_manifest(&manifest);
            progress(EngineProgress {
                version: Some(rel.display_version.clone()),
                ..EngineProgress::new(id, Phase::UpToDate)
            });
            continue;
        }
        match install_release(dirs, net, &rel, progress).await.and_then(|installed| {
            record_install(dirs, id, &installed)?;
            Ok(installed)
        }) {
            Ok(installed) => {
                progress(EngineProgress {
                    version: Some(installed.display_version.clone()),
                    ..EngineProgress::new(id, Phase::Done)
                });
                outcomes.push(UpdateOutcome {
                    engine: id,
                    from: entry.map(|e| e.display_version),
                    to: installed.display_version,
                });
            }
            Err(e) => progress(error_progress(id, e.to_string())),
        }
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_formats() {
        let sums = "0f19  yt-dlp_macos\n07e54b0865303c864006925913bce2604f8ee8cc6f18699bac9c309f9328a6d8  yt-dlp_macos.zip\n";
        assert_eq!(
            find_checksum(sums, "yt-dlp_macos.zip").as_deref(),
            Some("07e54b0865303c864006925913bce2604f8ee8cc6f18699bac9c309f9328a6d8")
        );
        assert_eq!(find_checksum(sums, "yt-dlp_macos"), None, "short hash must be rejected");
        let star = "C8ED4C4E6978A03C485EDBFE4E0A5DC2380F8A30BBA5150531B31B094492D924 *ffmpeg.zip";
        assert_eq!(
            find_checksum(star, "ffmpeg.zip").as_deref(),
            Some("c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924")
        );
        let bare = "c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924\n";
        assert!(find_checksum(bare, "ffmpeg.zip").is_some());
        assert_eq!(find_checksum("", "ffmpeg.zip"), None);
        let other = "c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924  ffprobe.zip";
        assert_eq!(find_checksum(other, "ffmpeg.zip"), None, "a different file's hash must not match");
    }

    #[test]
    fn powershell_checksums() {
        // Deno v2.9.8's deno-x86_64-pc-windows-msvc.zip.sha256sum, byte for byte.
        let deno = "\r\nAlgorithm : SHA256\r\nHash      : CBA84B26E0DF5AE7F74FC66B86A95B866872A86292EBD84FA1E959927EDD93BF\r\nPath      : C:\\a\\deno\\deno\\target\\release\\deno-x86_64-pc-windows-msvc.zip\r\n";
        assert_eq!(
            find_checksum(deno, "deno-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("cba84b26e0df5ae7f74fc66b86a95b866872a86292ebd84fa1e959927edd93bf")
        );
        assert_eq!(find_checksum(deno, "deno-aarch64-pc-windows-msvc.zip"), None, "the Path line names another file");
        let unnamed = "Algorithm : SHA256\nHash      : CBA84B26E0DF5AE7F74FC66B86A95B866872A86292EBD84FA1E959927EDD93BF\n";
        assert!(find_checksum(unnamed, "deno.zip").is_some());
        let short = "Algorithm : SHA1\r\nHash      : 0F19E8C3B2A1D4E5F60718293A4B5C6D7E8F9012\r\nPath      : C:\\deno.zip\r\n";
        assert_eq!(find_checksum(short, "deno.zip"), None, "a non-SHA-256 hash must be rejected");
        let forward = "Hash : cba84b26e0df5ae7f74fc66b86a95b866872a86292ebd84fa1e959927edd93bf\nPath : /tmp/out/deno.zip";
        assert!(find_checksum(forward, "deno.zip").is_some());
    }

    #[test]
    fn tags_and_build_ids() {
        assert_eq!(
            tag_from_location("https://github.com/yt-dlp/yt-dlp/releases/tag/2026.08.19").unwrap(),
            "2026.08.19"
        );
        assert_eq!(tag_from_location("https://github.com/denoland/deno/releases/tag/v2.9.7").unwrap(), "v2.9.7");
        assert!(tag_from_location("https://github.com/x/y/releases").is_err());
        assert!(tag_from_location("https://github.com/x/y/releases/tag/../../evil").is_err());
        assert_eq!(
            ffmpeg_build_id(
                "https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip",
                "ffmpeg"
            )
            .unwrap(),
            "1789931890_9.0.2"
        );
        assert!(ffmpeg_build_id("https://ffmpeg.martin-riedl.de/download/x/ffprobe.zip", "ffmpeg").is_err());
        assert_eq!(
            gyan_version("https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip").unwrap(),
            "9.0.2"
        );
        assert!(gyan_version("https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-full_build.zip").is_err());
        assert!(gyan_version("https://evil.example/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip").is_err());
        assert!(gyan_version("https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-..%2F..-essentials_build.zip").is_err());
    }

    #[test]
    fn ffmpeg_archive_members() {
        let root = Path::new("ffmpeg-9.0.2-essentials_build");
        assert_eq!(ffmpeg_member(&root.join("bin").join(bin::FFMPEG)), Some(bin::FFMPEG));
        assert_eq!(ffmpeg_member(&root.join("bin").join(bin::FFPROBE)), Some(bin::FFPROBE));
        assert_eq!(ffmpeg_member(&root.join("bin").join("ffplay.exe")), None);
        assert_eq!(ffmpeg_member(&root.join("doc").join(bin::FFMPEG)), None);
        assert_eq!(ffmpeg_member(&root.join("bin")), None);
        assert_eq!(ffmpeg_member(Path::new(bin::FFMPEG)), None);
    }

    /// Folder layout survives (yt-dlp.exe needs "_internal" next to it),
    /// FFmpeg is flattened, and an entry pointing outside is refused.
    #[cfg(windows)]
    #[test]
    fn unzips_engine_archives() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let make = |name: &str, entries: &[&str]| {
            let path = tmp.path().join(name);
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for entry in entries {
                zip.start_file(*entry, options).unwrap();
                zip.write_all(b"x").unwrap();
            }
            zip.finish().unwrap();
            path
        };
        let out = tmp.path().join("yt");
        unzip(EngineId::YtDlp, &make("yt.zip", &["yt-dlp.exe", "_internal/python313.dll"]), &out).unwrap();
        assert!(out.join("yt-dlp.exe").is_file() && out.join("_internal").join("python313.dll").is_file());

        let out = tmp.path().join("ff");
        let entries = ["ffmpeg-9.0.2-essentials_build/bin/ffmpeg.exe", "ffmpeg-9.0.2-essentials_build/bin/ffprobe.exe", "ffmpeg-9.0.2-essentials_build/bin/ffplay.exe", "ffmpeg-9.0.2-essentials_build/LICENSE"];
        unzip(EngineId::Ffmpeg, &make("ff.zip", &entries), &out).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["ffmpeg.exe", "ffprobe.exe"]);

        let evil = make("evil.zip", &["../evil.exe"]);
        assert!(unzip(EngineId::Deno, &evil, &tmp.path().join("deno")).is_err());
        assert!(!tmp.path().join("evil.exe").exists());
    }

    #[test]
    fn relative_redirects() {
        assert_eq!(
            absolute("https://ffmpeg.martin-riedl.de/redirect/latest/x", "/download/a/ffmpeg.zip"),
            "https://ffmpeg.martin-riedl.de/download/a/ffmpeg.zip"
        );
        assert_eq!(absolute("https://a.com/x", "https://b.com/y"), "https://b.com/y");
    }

    /// Real network install into a temp dir. Run with:
    /// cargo test -- --ignored install_all_engines --nocapture
    #[tokio::test]
    #[ignore]
    async fn install_all_engines() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = EngineDirs::new(tmp.path().join("engines with space"));
        let net = Net::new().unwrap();
        let progress = |p: EngineProgress| {
            if p.phase != Phase::Downloading {
                println!("{:?} {:?} {:?} {:?}", p.engine, p.phase, p.version, p.message);
            }
        };
        install_missing(&dirs, &net, Channel::Nightly, &progress).await.unwrap();
        assert!(dirs.all_installed());
        let state = dirs.state();
        assert!(state.engines.iter().all(|e| e.version.is_some()));
        let none = check_updates(&dirs, &net, Channel::Nightly, true, &progress).await;
        assert!(none.is_empty(), "fresh install should already be current: {none:?}");

        // Switching channel installs a second yt-dlp next to the first; the
        // old folder must survive until remove_old_versions runs.
        let before = dirs.current(EngineId::YtDlp);
        let switched = check_updates(&dirs, &net, Channel::Stable, false, &progress).await;
        assert_eq!(switched.len(), 1, "{switched:?}");
        let after = dirs.current(EngineId::YtDlp);
        assert_ne!(before, after);
        assert!(before.join(bin::YTDLP).is_file(), "old version must stay while jobs may use it");
        dirs.remove_old_versions();
        assert!(!before.exists());
        assert!(after.join(bin::YTDLP).is_file());
        assert!(dirs.programs().is_some());
    }
}

use crate::error::AppResult;
use crate::util::write_json_atomic;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Bump this when the usage notice text changes so users see it again.
pub const NOTICE_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Mp3,
    M4a,
    Opus,
    Flac,
    Wav,
    Mp4,
    Mkv,
    Webm,
}

impl Kind {
    pub fn is_audio(self) -> bool {
        matches!(self, Kind::Mp3 | Kind::M4a | Kind::Opus | Kind::Flac | Kind::Wav)
    }

    /// WAV and WEBM cannot carry cover art; asking yt-dlp to embed it there
    /// makes the whole download fail.
    pub fn takes_cover(self) -> bool {
        !matches!(self, Kind::Wav | Kind::Webm)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// yt-dlp's own README recommends nightly for regular users.
    #[default]
    Nightly,
    Stable,
}

impl Channel {
    pub fn key(self) -> &'static str {
        match self {
            Channel::Nightly => "nightly",
            Channel::Stable => "stable",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub download_dir: String,
    pub default_kind: Kind,
    pub default_res: u32,
    pub max_concurrent: u8,
    pub ytdlp_channel: Channel,
    pub accepted_notice: u32,
    pub square_cover: bool,
    /// Ask for a folder every time a download is started.
    pub ask_folder: bool,
    /// The downloads list on the main screen is folded away.
    pub hide_downloads: bool,
    /// Windows: the note that closing the window leaves Ambar running in the
    /// tray was shown. Set by the app only; the UI does not know this field.
    #[serde(default)]
    pub tray_note_shown: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            download_dir: String::new(),
            default_kind: Kind::Mp3,
            default_res: 1080,
            max_concurrent: 2,
            ytdlp_channel: Channel::Nightly,
            accepted_notice: 0,
            square_cover: true,
            ask_folder: false,
            hide_downloads: false,
            tray_note_shown: false,
        }
    }
}

impl Settings {
    pub fn normalized(mut self, default_download_dir: &Path) -> Self {
        if self.download_dir.trim().is_empty() {
            self.download_dir = default_download_dir.to_string_lossy().into_owned();
        }
        self.max_concurrent = self.max_concurrent.clamp(1, 3);
        if !(144..=4320).contains(&self.default_res) {
            self.default_res = 1080;
        }
        self
    }

    pub fn load(path: &Path, default_download_dir: &Path) -> Settings {
        let parsed = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .unwrap_or_default();
        parsed.normalized(default_download_dir)
    }

    pub fn save(&self, path: &Path) -> AppResult<()> {
        write_json_atomic(path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_bad_values() {
        let s = Settings {
            download_dir: "  ".into(),
            max_concurrent: 9,
            default_res: 5,
            ..Settings::default()
        }
        .normalized(Path::new("/tmp/Ambar"));
        assert_eq!(s.download_dir, "/tmp/Ambar");
        assert_eq!(s.max_concurrent, 3);
        assert_eq!(s.default_res, 1080);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let s: Settings = serde_json::from_str(r#"{"defaultKind":"mp4"}"#).unwrap();
        assert_eq!(s.default_kind, Kind::Mp4);
        assert!(!s.ask_folder);
        assert_eq!(s.ytdlp_channel, Channel::Nightly);
        assert!(s.square_cover);
        assert!(!s.tray_note_shown);
    }
}

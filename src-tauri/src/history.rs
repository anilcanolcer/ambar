//! Finished downloads, kept across launches so the list is still there
//! after Ambar is closed and opened again.

use crate::jobs::{JobStatus, JobView};
use crate::settings::Kind;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Older entries beyond this are dropped.
const LIMIT: usize = 300;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub thumbnail: Option<String>,
    pub kind: Kind,
    #[serde(default)]
    pub res: Option<u32>,
    pub file_path: String,
    pub finished_at: u64,
}

impl Entry {
    /// A finished job as a history entry; None for anything not done.
    pub fn from_job(job: &JobView) -> Option<Entry> {
        if job.status != JobStatus::Done {
            return None;
        }
        Some(Entry {
            url: job.url.clone(),
            title: job.title.clone(),
            thumbnail: job.thumbnail.clone(),
            kind: job.kind,
            res: job.res,
            file_path: job.file_path.clone()?,
            finished_at: crate::util::now_secs(),
        })
    }
}

pub struct History {
    path: PathBuf,
    /// Oldest first.
    entries: Mutex<Vec<Entry>>,
    /// The entries whose file was there at start-up.
    present: Vec<Entry>,
}

impl History {
    /// Reads the saved list, forgetting files that were deleted since. A file
    /// whose folder can't be reached (an unplugged disk) is kept for later.
    /// A missing or broken file just means an empty list.
    pub fn load(path: &Path) -> History {
        let mut entries: Vec<Entry> = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let excess = entries.len().saturating_sub(LIMIT);
        entries.drain(..excess);
        // One look at each file.
        let mut present = Vec::new();
        entries.retain(|e| {
            let file = Path::new(&e.file_path);
            if file.is_file() {
                present.push(e.clone());
                return true;
            }
            !file.parent().is_some_and(Path::is_dir)
        });
        History { path: path.to_path_buf(), entries: Mutex::new(entries), present }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn save(&self, entries: &[Entry]) {
        if let Err(e) = crate::util::write_json_atomic(&self.path, &entries) {
            eprintln!("[kova] history not saved: {e}");
        }
    }

    #[cfg(test)]
    pub fn entries(&self) -> Vec<Entry> {
        self.lock().clone()
    }

    /// The entries whose file was there at start-up, for the list.
    pub fn present(&self) -> Vec<Entry> {
        self.present.clone()
    }

    /// Records a finished download. A file name used again (the earlier file
    /// was deleted) replaces the old entry.
    pub fn add(&self, entry: Entry) {
        let mut entries = self.lock();
        entries.retain(|e| e.file_path != entry.file_path);
        entries.push(entry);
        let excess = entries.len().saturating_sub(LIMIT);
        entries.drain(..excess);
        self.save(&entries);
    }

    pub fn remove(&self, file_path: &str) {
        let mut entries = self.lock();
        let before = entries.len();
        entries.retain(|e| e.file_path != file_path);
        if entries.len() != before {
            self.save(&entries);
        }
    }

    /// Forgets everything except these files ("Bitenleri temizle"),
    /// including entries on a disk that isn't plugged in.
    pub fn retain_only(&self, file_paths: &[String]) {
        let mut entries = self.lock();
        let before = entries.len();
        entries.retain(|e| file_paths.contains(&e.file_path));
        if entries.len() != before {
            self.save(&entries);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &Path) -> Entry {
        Entry {
            url: "https://example.com/v".into(),
            title: "Video".into(),
            thumbnail: None,
            kind: Kind::Mp3,
            res: None,
            file_path: file.to_string_lossy().into_owned(),
            finished_at: 1,
        }
    }

    #[test]
    fn keeps_existing_files_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let kept = dir.path().join("a.mp3");
        let gone = dir.path().join("b.mp3");
        std::fs::write(&kept, b"x").unwrap();
        std::fs::write(&gone, b"x").unwrap();

        let history = History::load(&path);
        assert!(history.entries().is_empty());
        history.add(entry(&kept));
        history.add(Entry { title: "Yeni".into(), ..entry(&kept) });
        history.add(entry(&gone));
        assert_eq!(history.entries().len(), 2, "the same file is recorded once");
        assert_eq!(history.entries()[0].title, "Yeni", "a reused name replaces the old entry");

        std::fs::remove_file(&gone).unwrap();
        let again = History::load(&path);
        assert_eq!(again.entries().len(), 1, "deleted files drop out");

        again.remove(&kept.to_string_lossy());
        assert!(History::load(&path).entries().is_empty());
    }

    #[test]
    fn keeps_files_on_unplugged_disks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let history = History::load(&path);
        let away = Path::new("/Volumes/AmbarYokDisk/Videolar/a.mp4");
        history.add(entry(away));
        let again = History::load(&path);
        assert_eq!(again.entries().len(), 1, "kept until the disk is back");
        assert!(again.present().is_empty(), "but not listed while it is away");
        again.retain_only(&[]);
        assert!(History::load(&path).entries().is_empty(), "clearing forgets it too");
    }

    #[test]
    fn broken_file_is_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(History::load(&path).entries().is_empty());
    }
}

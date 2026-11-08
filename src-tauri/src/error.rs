use serde::{Serialize, Serializer};

/// Every error that reaches the UI is shown to the user as plain Turkish text.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Msg(String),
    #[error("Ağ hatası, internet bağlantını kontrol et.")]
    Http(reqwest::Error),
    #[error("Beklenmeyen bir yanıt alındı.")]
    Json(serde_json::Error),
    #[error("{0}")]
    Tauri(#[from] tauri::Error),
}

// The technical (English) details only go to the log; the user sees Turkish.
impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        eprintln!("[kova] network error: {e}");
        AppError::Http(e)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        eprintln!("[kova] json error: {e}");
        AppError::Json(e)
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        eprintln!("[kova] io error: {e}");
        AppError::Msg(crate::util::io_text(&e).to_string())
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;

pub fn msg(s: impl Into<String>) -> AppError {
    AppError::Msg(s.into())
}

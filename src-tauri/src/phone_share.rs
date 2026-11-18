//! Shares one completed download with a phone on the local network.

use crate::commands::AppState;
use crate::error::{msg, AppResult};
use futures_util::stream;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use if_addrs::IfAddr;
use qrcode::{Color, QrCode};
use serde::Serialize;
use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::State;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{self, Instant};

const LIFETIME: Duration = Duration::from_secs(15 * 60);
const MAX_CONNECTIONS: usize = 12;
type Body = UnsyncBoxBody<Bytes, io::Error>;

#[derive(Default)]
pub struct PhoneShare(Mutex<Option<Session>>);

struct Session {
    id: String,
    server: JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareView {
    id: String,
    url: String,
    qr_width: usize,
    qr_modules: Vec<bool>,
    expires_in: u64,
}

#[derive(Clone, Copy)]
struct Lan {
    ip: Ipv4Addr,
    mask: Ipv4Addr,
}

impl Lan {
    fn on_subnet(self, peer: Ipv4Addr) -> bool {
        u32::from(self.ip) & u32::from(self.mask) == u32::from(peer) & u32::from(self.mask)
    }
}

fn local_network() -> Option<Lan> {
    // UDP connect asks the OS for its preferred route without sending any data.
    let preferred = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|socket| {
            socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9))?;
            socket.local_addr().map(|addr| addr.ip())
        })
        .ok();
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .filter_map(|interface| match interface.addr {
            IfAddr::V4(addr)
                if (addr.ip.is_private() || addr.ip.is_link_local())
                    && !addr.netmask.is_unspecified() =>
            {
                Some(Lan {
                    ip: addr.ip,
                    mask: addr.netmask,
                })
            }
            _ => None,
        })
        .max_by_key(|lan| (preferred == Some(IpAddr::V4(lan.ip)), lan.ip.is_private()))
}

#[tauri::command]
pub async fn start_phone_share(
    state: State<'_, AppState>,
    sharing: State<'_, PhoneShare>,
    id: u64,
) -> AppResult<ShareView> {
    let path = state.jobs.file_of(id).ok_or_else(missing_file)?;
    let lan = local_network().ok_or_else(|| {
        msg("Yerel ağ adresi bulunamadı. Bilgisayarın ağ bağlantısını kontrol et.")
    })?;
    sharing.start(&path, lan, LIFETIME).await
}

#[tauri::command]
pub fn stop_phone_share(sharing: State<'_, PhoneShare>, id: String) {
    sharing.stop(&id);
}

fn missing_file() -> crate::error::AppError {
    msg("Dosya bulunamadı. Taşınmış ya da silinmiş olabilir.")
}

impl PhoneShare {
    async fn start(&self, path: &Path, lan: Lan, lifetime: Duration) -> AppResult<ShareView> {
        let name = path
            .file_name()
            .ok_or_else(missing_file)?
            .to_string_lossy()
            .into_owned();
        let kind = tokio::fs::symlink_metadata(path)
            .await
            .map_err(|_| missing_file())?
            .file_type();
        if !kind.is_file() {
            return Err(missing_file());
        }
        let file = File::open(path).await.map_err(|_| missing_file())?;
        let metadata = file.metadata().await.map_err(|_| missing_file())?;
        if !metadata.is_file() {
            return Err(missing_file());
        }
        let listener = TcpListener::bind(SocketAddrV4::new(lan.ip, 0))
            .await
            .map_err(|_| {
                msg("Paylaşım başlatılamadı. Yerel ağ bağlantısını ve izinlerini kontrol et.")
            })?;
        let host = listener.local_addr()?.to_string();
        let mut secret = [0_u8; 16];
        getrandom::fill(&mut secret).map_err(|_| msg("Paylaşım bağlantısı oluşturulamadı."))?;
        let id = hex::encode(secret);
        let page_path = format!("/p/{id}");
        let url = format!("http://{host}{page_path}");
        let qr = QrCode::new(url.as_bytes()).map_err(|_| msg("QR kod oluşturulamadı."))?;
        let view = ShareView {
            id: id.clone(),
            url,
            qr_width: qr.width(),
            qr_modules: qr
                .to_colors()
                .into_iter()
                .map(|color| color == Color::Dark)
                .collect(),
            expires_in: lifetime.as_secs(),
        };
        let download = Arc::new(Download {
            host,
            file_path: format!("{page_path}/file"),
            page_path,
            name,
            size: metadata.len(),
            file: tokio::sync::Mutex::new(file),
        });
        let deadline = Instant::now() + lifetime;
        let mut session = self.0.lock().unwrap_or_else(|p| p.into_inner());
        *session = Some(Session {
            id,
            server: tokio::spawn(serve(listener, lan, download, deadline)),
        });
        Ok(view)
    }

    fn stop(&self, id: &str) {
        let mut session = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if session.as_ref().is_some_and(|active| active.id == id) {
            *session = None;
        }
    }
}

struct Download {
    host: String,
    page_path: String,
    file_path: String,
    name: String,
    size: u64,
    // Keep the selected file open so a later replacement at the same path is
    // never picked up by a still-valid link.
    file: tokio::sync::Mutex<File>,
}

async fn serve(listener: TcpListener, lan: Lan, download: Arc<Download>, deadline: Instant) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = time::sleep_until(deadline) => break,
            _ = connections.join_next(), if !connections.is_empty() => {}
            result = listener.accept(), if connections.len() < MAX_CONNECTIONS => {
                let Ok((socket, peer)) = result else { break };
                if !matches!(peer.ip(), IpAddr::V4(ip) if lan.on_subnet(ip)) {
                    continue;
                }
                let download = Arc::clone(&download);
                connections.spawn(async move {
                    let _ = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(5))
                        .max_headers(48)
                        .max_buf_size(16 * 1024)
                        .keep_alive(false)
                        .serve_connection(TokioIo::new(socket), service_fn(move |request| {
                            let response = Arc::clone(&download).respond(&request);
                            async move { Ok::<_, Infallible>(response) }
                        }))
                        .await;
                });
            }
        }
    }
}

fn full(value: impl Into<Bytes>) -> Body {
    Full::new(value.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

fn response(status: StatusCode, content_type: &'static str, body: Body) -> Response<Body> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
    ));
    response
}

impl Download {
    fn respond(self: Arc<Self>, request: &Request<Incoming>) -> Response<Body> {
        let head = request.method() == Method::HEAD;
        let path = request.uri().path();
        let valid_host = request
            .headers()
            .get(header::HOST)
            .is_some_and(|host| host == self.host.as_str());
        if !valid_host
            || request.uri().authority().is_some()
            || (path != self.page_path && path != self.file_path)
        {
            let body = if head {
                full("")
            } else {
                full("Paylaşım bağlantısı geçersiz.")
            };
            return response(StatusCode::NOT_FOUND, "text/plain; charset=utf-8", body);
        }
        if !head && request.method() != Method::GET {
            let mut reply = response(
                StatusCode::METHOD_NOT_ALLOWED,
                "text/plain; charset=utf-8",
                full(""),
            );
            reply
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
            return reply;
        }
        if path == self.page_path {
            let page = include_str!("phone_share.html")
                .replace("{{DOWNLOAD}}", &self.file_path)
                .replace("{{FILENAME}}", &escape_html(&self.name));
            let len = page.len();
            let mut reply = response(
                StatusCode::OK,
                "text/html; charset=utf-8",
                if head { full("") } else { full(page) },
            );
            reply
                .headers_mut()
                .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            return reply;
        }

        let range = request
            .headers()
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok());
        let range =
            range.filter(|range| !head && range.starts_with("bytes=") && !range.contains(','));
        let (start, end) = match range.map(|range| byte_range(range, self.size)) {
            Some(Some(span)) => span,
            Some(None) => {
                let mut reply = response(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    "text/plain; charset=utf-8",
                    full(""),
                );
                reply.headers_mut().insert(
                    header::CONTENT_RANGE,
                    format!("bytes */{}", self.size).parse().unwrap(),
                );
                return reply;
            }
            None => (0, self.size),
        };
        let body = if head || start == end {
            full("")
        } else {
            self.clone().stream(start, end)
        };
        let status = if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };
        let mut reply = response(status, "application/octet-stream", body);
        let headers = reply.headers_mut();
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(end - start));
        headers.insert(header::CONTENT_DISPOSITION, content_disposition(&self.name));
        if range.is_some() {
            headers.insert(
                header::CONTENT_RANGE,
                format!("bytes {start}-{}/{}", end - 1, self.size)
                    .parse()
                    .unwrap(),
            );
        }
        reply
    }

    fn stream(self: Arc<Self>, start: u64, end: u64) -> Body {
        StreamBody::new(stream::try_unfold(
            (self, start),
            move |(download, offset)| async move {
                if offset == end {
                    return Ok(None);
                }
                let mut bytes = vec![0; (end - offset).min(128 * 1024) as usize];
                {
                    let mut file = download.file.lock().await;
                    file.seek(io::SeekFrom::Start(offset)).await?;
                    file.read_exact(&mut bytes).await?;
                }
                let next = offset + bytes.len() as u64;
                Ok(Some((Frame::data(Bytes::from(bytes)), (download, next))))
            },
        ))
        .boxed_unsync()
    }
}

/// Returns a half-open byte range. HTTP specifies an inclusive last byte.
fn byte_range(range: &str, size: u64) -> Option<(u64, u64)> {
    let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
    if start.is_empty() {
        let count = end.parse::<u64>().ok()?;
        return (count > 0 && size > 0).then_some((size.saturating_sub(count), size));
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        size
    } else {
        end.parse::<u64>().ok()?.saturating_add(1).min(size)
    };
    (start < end).then_some((start, end))
}

fn escape_html(name: &str) -> String {
    name.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn content_disposition(name: &str) -> HeaderValue {
    use std::fmt::Write;
    let mut ascii = String::new();
    let mut encoded = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') {
            ascii.push(char::from(byte));
            encoded.push(char::from(byte));
        } else {
            if byte.is_ascii() {
                ascii.push('_');
            }
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    if ascii.is_empty() || ascii.starts_with('.') {
        ascii.insert_str(0, "Ambar");
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
        .parse()
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOOPBACK: Lan = Lan {
        ip: Ipv4Addr::LOCALHOST,
        mask: Ipv4Addr::new(255, 255, 255, 0),
    };

    #[tokio::test]
    async fn serves_only_the_selected_file_and_supports_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Örnek & video.mp4");
        let original = b"0123456789".repeat(40_000);
        std::fs::write(&path, &original).unwrap();
        let sharing = PhoneShare::default();
        let share = sharing.start(&path, LOOPBACK, LIFETIME).await.unwrap();
        assert_eq!(share.qr_modules.len(), share.qr_width.pow(2));
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let page = client.get(&share.url).send().await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        assert!(page.text().await.unwrap().contains("Örnek &amp; video.mp4"));

        let url = format!("{}/file", share.url);
        let denied = client
            .get(&url)
            .header(header::HOST, "other.test")
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
        let denied = client
            .get(url.replace(&share.id, "invalid"))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::NOT_FOUND);
        let denied = client.post(&url).send().await.unwrap();
        assert_eq!(denied.status(), StatusCode::METHOD_NOT_ALLOWED);

        let file = client.head(&url).send().await.unwrap();
        assert_eq!(
            file.headers()[header::CONTENT_LENGTH],
            original.len().to_string()
        );
        assert!(file.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("%C3%96rnek%20%26%20video.mp4"));
        let invalid = client
            .get(&url)
            .header(header::RANGE, "bytes=700000-")
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        let partial = client
            .get(&url)
            .header(header::RANGE, "bytes=4-7")
            .send()
            .await
            .unwrap();
        assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(partial.headers()[header::CONTENT_RANGE], "bytes 4-7/400000");
        assert_eq!(partial.bytes().await.unwrap(), &b"4567"[..]);
        let full = client.get(&url).send().await.unwrap();
        assert_eq!(full.bytes().await.unwrap(), original);

        sharing.stop("old-session");
        assert_eq!(
            client.get(&share.url).send().await.unwrap().status(),
            StatusCode::OK
        );
        sharing.stop(&share.id);
        wait_until_closed(&client, &share.url).await;
    }

    #[tokio::test]
    async fn closes_the_listener_on_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audio.mp3");
        std::fs::write(&path, "audio").unwrap();
        let sharing = PhoneShare::default();
        let share = sharing
            .start(&path, LOOPBACK, Duration::from_millis(100))
            .await
            .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        wait_until_closed(&client, &share.url).await;
    }

    async fn wait_until_closed(client: &reqwest::Client, url: &str) {
        time::timeout(Duration::from_secs(5), async {
            while client.get(url).send().await.is_ok() {
                time::sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .expect("listener should close");
    }
}

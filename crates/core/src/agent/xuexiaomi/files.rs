//! The gateway's measured attachment protocol, shared by every frontend.
use super::{authorized, error_from, BackendConfig, ComposerAttachment};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

const MAX_IMAGE: usize = 8 * 1024 * 1024;
const MAX_FILE: usize = 24 * 1024 * 1024;
const MAX_DOWNLOAD: usize = 256 * 1024 * 1024;

pub(super) struct Prepared {
    pub name: String,
    pub data: String,
    pub mime: Option<&'static str>,
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

pub(super) fn prepare(attachments: &[ComposerAttachment]) -> Result<Vec<Prepared>> {
    if attachments.len() > 6 {
        return Err(anyhow!("最多同时发送 6 个附件"));
    }
    attachments
        .iter()
        .map(|attachment| {
            let name = attachment.name();
            let file = std::fs::File::open(&attachment.path).map_err(|e| anyhow!("{name}: {e}"))?;
            if !file.metadata()?.is_file() {
                return Err(anyhow!("{name}: 请选择文件，不能上传文件夹"));
            }
            let mut bytes = Vec::new();
            file.take((MAX_FILE + 1) as u64).read_to_end(&mut bytes)?;
            let mime = image_mime(&bytes);
            let limit = if mime.is_some() { MAX_IMAGE } else { MAX_FILE };
            if bytes.len() > limit {
                return Err(anyhow!("{name}: 超过 {} MB 限制", limit / 1024 / 1024));
            }
            Ok(Prepared {
                name,
                data: STANDARD.encode(bytes),
                mime,
            })
        })
        .collect()
}

pub(super) fn from_row(row: &serde_json::Value) -> Option<ComposerAttachment> {
    let source = row.get("url")?.as_str()?.trim();
    if source.is_empty() {
        return None;
    }
    let mut attachment = ComposerAttachment::new(source);
    // A replay of the same file remains the same attachment.
    attachment.id = source.to_string();
    attachment.display_name = Some(
        row.get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                source
                    .split('?')
                    .next()
                    .unwrap_or(source)
                    .rsplit('/')
                    .next()
                    .unwrap_or("附件")
                    .to_string()
            }),
    );
    Some(attachment)
}

fn download_url(config: &BackendConfig, source: &str) -> Result<url::Url> {
    let base = url::Url::parse(&format!("{}/", config.base_url.trim_end_matches('/')))?;
    let target = base.join(source)?;
    if !matches!(target.scheme(), "https" | "http")
        || target.origin() != base.origin()
        || !target.username().is_empty()
        || target.password().is_some()
    {
        return Err(anyhow!("The attachment URL is outside this gateway."));
    }
    Ok(target)
}

/// Redirects are refused: a gateway token must never follow a file to another host.
pub async fn attachment_bytes(
    config: &BackendConfig,
    source: &str,
    preview: bool,
) -> Result<Vec<u8>> {
    let target = download_url(config, source)?;
    // The gateway's links expire and may be single-use. Preview and Save must
    // share one fetch, and the bytes must survive reopening the conversation.
    static DOWNLOAD: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let _guard = DOWNLOAD
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let key = format!(
        "{:x}",
        Sha256::digest(format!("{}\n{}", config.credential, target).as_bytes())
    );
    let dir = crate::logger::dirs::app_dir().join("ReceivedAttachments");
    let cached = dir.join(&key);
    if let Ok(bytes) = std::fs::read(&cached) {
        return preview_bytes(bytes, preview);
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = authorized(http.get(target), config).send().await?;
    if !response.status().is_success() {
        return Err(error_from(response).await);
    }
    let limit = MAX_DOWNLOAD;
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(anyhow!("文件超过 {} MB 下载限制", limit / 1024 / 1024));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > limit {
            return Err(anyhow!("文件超过 {} MB 下载限制", limit / 1024 / 1024));
        }
        bytes.extend_from_slice(&chunk);
    }
    std::fs::create_dir_all(&dir)?;
    let temp = dir.join(format!("{key}.tmp"));
    std::fs::write(&temp, &bytes)?;
    std::fs::rename(temp, cached)?;
    preview_bytes(bytes, preview)
}

fn preview_bytes(bytes: Vec<u8>, preview: bool) -> Result<Vec<u8>> {
    if preview && bytes.len() > MAX_IMAGE {
        return Err(anyhow!(
            "Image exceeds 8 MB preview limit; save the file to view it."
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn preview_and_repeated_saves_share_a_single_use_download() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = BackendConfig {
            base_url: format!("http://{}", listener.local_addr().unwrap()),
            credential: uuid::Uuid::new_v4().to_string(),
            ..Default::default()
        };
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let n = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..n]);
            assert!(request.starts_with("GET /api/download?token=one-use "));
            assert!(request.to_lowercase().contains("authorization: bearer "));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture",
                )
                .await
                .unwrap();
        });
        let source = "/api/download?token=one-use";
        assert_eq!(
            attachment_bytes(&config, source, true).await.unwrap(),
            b"fixture"
        );
        task.await.unwrap();
        // The listener is gone; another network request would fail.
        assert_eq!(
            attachment_bytes(&config, source, false).await.unwrap(),
            b"fixture"
        );
        assert_eq!(
            attachment_bytes(&config, source, false).await.unwrap(),
            b"fixture"
        );
    }
    #[test]
    fn downloads_keep_credentials_on_the_gateway() {
        let config = BackendConfig {
            base_url: "https://example.com/gateway".into(),
            ..Default::default()
        };
        assert_eq!(
            download_url(&config, "api/files/a?key=x").unwrap().path(),
            "/gateway/api/files/a"
        );
        for url in [
            "https://evil.com/file",
            "//evil.com/a",
            "file:///tmp/file",
            "https://user@example.com/a",
        ] {
            assert!(download_url(&config, url).is_err());
        }
    }
    #[test]
    fn validates_before_upload_and_detects_images_from_bytes() {
        let dir = std::env::temp_dir().join(format!("vg-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("image.bin");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nbytes").unwrap();
        let attachment = ComposerAttachment::new(path.to_string_lossy());
        let file = prepare(std::slice::from_ref(&attachment))
            .unwrap()
            .remove(0);
        assert_eq!(file.mime, Some("image/png"));
        assert_eq!(
            STANDARD.decode(file.data).unwrap(),
            b"\x89PNG\r\n\x1a\nbytes"
        );
        assert!(prepare(&vec![attachment.clone(); 7]).is_err());
        assert!(prepare(&[ComposerAttachment::new(dir.to_string_lossy())]).is_err());
        std::fs::write(&path, vec![0; MAX_FILE + 1]).unwrap();
        assert!(prepare(&[attachment]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

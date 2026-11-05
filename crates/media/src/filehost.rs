//! soju.im/FILEHOST: an independent POST provider. No XMPP slots or foreign account.
use crate::Error;
use reqwest::{Client, Url, header};
use std::{path::Path, time::Duration};
use tokio::io::AsyncReadExt;

pub struct FileHost {
    endpoint: Url,
    client: Client,
    auth: Option<(String, String)>,
    tls: bool,
}

impl FileHost {
    pub fn new(
        endpoint: &str,
        tls: bool,
        auth: Option<(String, String)>,
        ca: Option<&[u8]>,
    ) -> Result<Self, Error> {
        let endpoint = Url::parse(endpoint).map_err(|_| Error::Upload("invalid FILEHOST URL".into()))?;
        if !matches!(endpoint.scheme(), "https" | "http")
            || (tls && endpoint.scheme() != "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(Error::Upload(
                "FILEHOST must use HTTPS with a TLS IRC connection and cannot contain credentials".into(),
            ));
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(300))
            .read_timeout(Duration::from_secs(60));
        if let Some(ca) = ca {
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(ca)
                    .map_err(|_| Error::Upload("invalid FILEHOST CA certificate".into()))?,
            );
        }
        let client = builder
            .build()
            .map_err(|_| Error::Upload("cannot create FILEHOST client".into()))?;
        Ok(Self {
            endpoint,
            client,
            auth,
            tls,
        })
    }

    fn authenticate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            Some((user, pass)) => request.basic_auth(user, Some(pass)),
            None => request,
        }
    }

    pub async fn upload(
        &self,
        path: &Path,
        content_type: &str,
        progress: impl Fn(u64, u64) + Send + Sync + 'static,
    ) -> Result<omtalk_core::Attachment, Error> {
        let file = tokio::fs::File::open(path).await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() {
            return Err(Error::Upload("not a regular file".into()));
        }
        let total = metadata.len();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let options = self
            .authenticate(
                self.client
                    .request(reqwest::Method::OPTIONS, self.endpoint.clone()),
            )
            .send()
            .await
            .map_err(http_error)?;
        if matches!(options.status().as_u16(), 401 | 403 | 413 | 415 | 429 | 500..=599) {
            return Err(status_error(options.status()));
        }
        if options.status().is_redirection() {
            return Err(Error::Upload(
                "FILEHOST redirect refused; credentials were not forwarded".into(),
            ));
        }
        if let Some(accepted) = options.headers().get("Accept-Post").and_then(|h| h.to_str().ok())
            && !accepted.split(',').any(|m| {
                let mime = m.trim().split(';').next().unwrap_or_default();
                mime == "*/*"
                    || mime == content_type
                    || mime
                        .strip_suffix("/*")
                        .is_some_and(|prefix| content_type.starts_with(&format!("{prefix}/")))
            })
        {
            return Err(Error::Upload(format!("FILEHOST does not accept {content_type}")));
        }
        let stream = futures_util::stream::try_unfold(
            (file, 0u64, progress),
            move |(mut file, sent, progress)| async move {
                let mut buffer = vec![0u8; 64 * 1024];
                let count = file.read(&mut buffer).await?;
                if count == 0 {
                    if sent != total {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "file changed while uploading",
                        ));
                    }
                    return Ok(None);
                }
                buffer.truncate(count);
                let sent = sent + count as u64;
                progress(sent, total);
                Ok::<_, std::io::Error>(Some((bytes::Bytes::from(buffer), (file, sent, progress))))
            },
        );
        // filename* preserves UTF-8, without letting quotes/newlines add headers.
        let encoded = percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC);
        let response = self
            .authenticate(self.client.post(self.endpoint.clone()))
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, total)
            .header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename*=UTF-8''{encoded}"),
            )
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(http_error)?;
        if response.status() != reqwest::StatusCode::CREATED {
            return Err(status_error(response.status()));
        }
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|h| h.to_str().ok())
            .ok_or_else(|| Error::Upload("FILEHOST returned 201 without a valid Location".into()))?;
        let url = self
            .endpoint
            .join(location)
            .map_err(|_| Error::Upload("FILEHOST returned invalid Location".into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || (self.tls && url.scheme() != "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::Upload("FILEHOST returned an unsafe file URL".into()));
        }
        Ok(omtalk_core::Attachment {
            url: url.into(),
            name,
            content_type: Some(content_type.into()),
            size: Some(total),
            local_path: Some(path.into()),
        })
    }
}

fn http_error(error: reqwest::Error) -> Error {
    // reqwest error Display includes the URL. Keep URLs/tokens and credentials out
    // of logs/status: status and kind are sufficient to recover from the failure.
    Error::Upload(
        if error.is_timeout() {
            "FILEHOST timed out"
        } else if error.is_connect() {
            "cannot connect to FILEHOST (check TLS and address)"
        } else {
            "FILEHOST request failed"
        }
        .into(),
    )
}

fn status_error(status: reqwest::StatusCode) -> Error {
    let reason = match status.as_u16() {
        401 => "authentication required",
        403 => "permission denied",
        413 => "file is too large for this service",
        415 => "file type is not supported",
        429 => "rate limit; retry later",
        300..=399 => "redirect refused; credentials were not forwarded",
        500..=599 => "service unavailable; retry later",
        _ => "unexpected response (expected 201 Created)",
    };
    Error::Upload(format!("FILEHOST HTTP {}: {reason}", status.as_u16()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn relative_location_and_streamed_body_do_not_forward_credentials_to_redirects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/upload", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for upload in [false, true] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).await.unwrap();
                    request.extend(byte);
                }
                let text = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(text.contains("authorization: basic "));
                if upload {
                    assert!(text.starts_with("post "));
                    assert!(text.contains("content-length: 5"));
                    let mut body = [0u8; 5];
                    stream.read_exact(&mut body).await.unwrap();
                    assert_eq!(&body, b"sound");
                    stream.write_all(b"HTTP/1.1 201 Created\r\nLocation: /files/123\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                } else {
                    stream
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nAccept-Post: audio/*\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .unwrap();
                }
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("голос.ogg");
        std::fs::write(&path, b"sound").unwrap();
        let host = FileHost::new(&endpoint, false, Some(("test".into(), "test".into())), None).unwrap();
        let file = host.upload(&path, "audio/ogg", |_, _| {}).await.unwrap();
        assert!(file.url.ends_with("/files/123"));
        assert_eq!(file.name, "голос.ogg");
        server.await.unwrap();
        assert!(FileHost::new(&endpoint, true, None, None).is_err());
    }

    #[tokio::test]
    async fn server_rejections_redirects_and_timeouts_keep_the_local_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.ogg");
        std::fs::write(&path, b"sound").unwrap();
        for status in [401, 413, 415, 302] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/upload", listener.local_addr().unwrap());
            let trap = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let redirect = format!(
                "http://{}/credentials-must-not-arrive",
                trap.local_addr().unwrap()
            );
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut b = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut b).await.unwrap();
                    request.extend(b);
                }
                stream.write_all(format!("HTTP/1.1 {status} Rejected\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            });
            let host = FileHost::new(
                &endpoint,
                false,
                Some(("test-user".into(), "test-secret".into())),
                None,
            )
            .unwrap();
            let error = host
                .upload(&path, "audio/ogg", |_, _| {})
                .await
                .unwrap_err()
                .to_string();
            assert!(!error.contains("test-secret"));
            assert!(!error.contains("credentials-must"));
            assert!(error.contains(&status.to_string()) || error.contains("redirect"));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), trap.accept())
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"sound");
            server.await.unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/upload", listener.local_addr().unwrap());
        let mut host = FileHost::new(&endpoint, false, None, None).unwrap();
        host.client = Client::builder()
            .timeout(Duration::from_millis(20))
            .build()
            .unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(60)).await;
        });
        assert!(
            host.upload(&path, "audio/ogg", |_, _| {})
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(path.exists());
        server.await.unwrap();
    }
}

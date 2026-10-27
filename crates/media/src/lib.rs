//! Files shared in chats: a message whose whole text is a link. `aesgcm://` links point
//! at a file encrypted for the chat (XEP-0454: OMEMO media sharing), with the key in the
//! fragment, which never leaves this machine; `https://` links point at plain files.

use std::path::{Path, PathBuf};

use aes_gcm::aead::AeadInOut;
use aes_gcm::aead::consts::{U12, U16};
use aes_gcm::{AesGcm, KeyInit, aes::Aes256};
use sha2::{Digest, Sha256};

/// Files larger than this are not fetched: a chat is no place for them, and a link from a
/// stranger should not fill the disk.
pub const MAX_SIZE: u64 = 50 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("download failed: {0}")]
    Download(String),
    #[error("download failed: {0}")]
    TemporaryDownload(String),
    #[error("upload failed: {0}")]
    Upload(String),
    #[error("the file is larger than {} MB", MAX_SIZE / 1024 / 1024)]
    TooLarge,
    #[error("the file does not decrypt with the key of the link")]
    Decrypt,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Retrying cannot fix invalid keys, size limits, disk errors or permanent HTTP errors.
    pub fn retryable(&self) -> bool {
        matches!(self, Self::TemporaryDownload(_))
    }
}
fn download_error(error: reqwest::Error) -> Error {
    let transient = error.is_connect()
        || error.is_timeout()
        || error.is_body()
        || error.is_decode()
        || error
            .status()
            .is_some_and(|s| s.is_server_error() || s.as_u16() == 429 || s.as_u16() == 408);
    use std::error::Error as _;
    let mut text = error.to_string();
    let mut source = error.source();
    for _ in 0..3 {
        let Some(cause) = source else {
            break;
        };
        let detail = cause.to_string();
        if !text.ends_with(&detail) {
            text.push_str(": ");
            text.push_str(&detail);
        }
        source = cause.source();
    }
    if transient {
        Error::TemporaryDownload(text)
    } else {
        Error::Download(text)
    }
}

/// A shared file a message points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Media {
    Encrypted { url: String, iv: Vec<u8>, key: [u8; 32] },
    Plain { url: String },
}

impl Media {
    /// The file a message is about, if its whole text is a link that is always one: an
    /// encrypted file (`aesgcm://`). A plain `https://` link is written by hand as often
    /// as shared: see [`Media::shared`].
    pub fn from_text(text: &str) -> Option<Self> {
        Self::parse(text).filter(|m| matches!(m, Media::Encrypted { .. }))
    }

    /// The file a message shares, its sender having said so (XEP-0066 out of band data,
    /// as Conversations, Gajim and Monal mark their uploads): `aesgcm://`, or `https://`
    /// ending in a file's name of a known kind.
    pub fn shared(text: &str) -> Option<Self> {
        Self::parse(text)
    }

    /// An ordinary HTTPS link whose file name identifies a supported image. Opt-in
    /// previews use this, without turning arbitrary links into downloadable files.
    pub fn image_link(text: &str) -> Option<Self> {
        Self::parse(text).filter(|media| matches!(media, Self::Plain { .. }) && media.is_image())
    }

    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.contains(char::is_whitespace) {
            return None;
        }
        if let Some(rest) = text.strip_prefix("aesgcm://") {
            let (location, fragment) = rest.split_once('#')?;
            let secret = hex(fragment)?;
            // 12-byte IVs are the standard; older clients sent 16.
            let iv_len = match secret.len() {
                44 => 12,
                48 => 16,
                _ => return None,
            };
            let (iv, key) = secret.split_at(iv_len);
            // A file whatever its name: one without an extension (a program, say) too.
            return Some(Media::Encrypted {
                url: format!("https://{location}"),
                iv: iv.to_vec(),
                key: key.try_into().ok()?,
            });
        }
        // Only for a link its sender shared as a file (see `Media::shared`), and one that
        // ends in a file's name, as uploads do: not a page ("…/news/", "…?v=…").
        if let Some(rest) = text.strip_prefix("https://")
            && let Some((_, path)) = rest.split_once('/')
            && !path.is_empty()
            && !path.ends_with('/')
            && !text.contains(['?', '#'])
        {
            return Some(Media::Plain { url: text.to_owned() });
        }
        None
    }

    pub fn url(&self) -> &str {
        match self {
            Media::Encrypted { url, .. } | Media::Plain { url } => url,
        }
    }

    pub fn encrypted(&self) -> bool {
        matches!(self, Media::Encrypted { .. })
    }

    /// The file's name as sent: the link's last part, decoded ("%D0%BE…" is "о…"). A
    /// name only, never a path: it names the file on disk.
    pub fn name(&self) -> String {
        let path = self.url().split(['?', '#']).next().unwrap_or_default();
        let last = path.rsplit('/').next().unwrap_or_default();
        let decoded = percent_encoding::percent_decode_str(last).decode_utf8_lossy();
        let name: String = decoded
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        match name.trim() {
            "" | "." | ".." => "file".to_owned(),
            name => name.to_owned(),
        }
    }

    /// Whether the name looks like a picture worth showing inline.
    pub fn is_image(&self) -> bool {
        let name = self.name().to_ascii_lowercase();
        [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"]
            .iter()
            .any(|ext| name.ends_with(ext))
    }

    /// Where the file is kept once fetched: under its own name (what a viewer shows and
    /// a "save as" offers), in a folder named after the link, key included, so a file is
    /// fetched once and two links never share a file.
    pub fn cache_path(&self, cache_dir: &Path) -> PathBuf {
        cache_dir.join(self.digest()).join(self.name())
    }

    /// The file if it is here already. One kept by an older omtalk, named after the link
    /// alone, is moved where it goes now.
    pub fn cached(&self, cache_dir: &Path) -> Option<PathBuf> {
        let path = self.cache_path(cache_dir);
        if path.is_file() {
            return Some(path);
        }
        let name = self.name();
        let extension = name
            .rsplit_once('.')
            .map(|(_, e)| format!(".{e}"))
            .unwrap_or_default();
        let old = cache_dir.join(format!("{}{extension}", self.digest()));
        if old.is_file() {
            // Without an extension the old file has the new folder's name: moved aside
            // first.
            let aside = old.with_extension("moving");
            std::fs::rename(&old, &aside).ok()?;
            std::fs::create_dir_all(path.parent()?).ok()?;
            std::fs::rename(&aside, &path).ok()?;
            return Some(path);
        }
        None
    }

    fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.url().as_bytes());
        if let Media::Encrypted { iv, key, .. } = self {
            hasher.update(iv);
            hasher.update(key);
        }
        hasher
            .finalize()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// The file on disk, fetched and decrypted if it is not there yet.
    pub async fn fetch(&self, cache_dir: &Path) -> Result<PathBuf, Error> {
        self.fetch_with(cache_dir, |_, _| {}).await
    }

    /// Fetch with at most three attempts for transient transport/server failures.
    /// `retrying` is called before the bounded backoff with the next attempt number.
    pub async fn fetch_with_retry(
        &self,
        cache_dir: &Path,
        progress: impl Fn(u64, Option<u64>),
        retrying: impl Fn(u8),
    ) -> Result<PathBuf, Error> {
        for attempt in 1..=3 {
            match self.fetch_with(cache_dir, &progress).await {
                Err(error) if error.retryable() && attempt < 3 => {
                    retrying(attempt + 1);
                    tokio::time::sleep(std::time::Duration::from_secs(1 << (attempt - 1))).await;
                }
                result => return result,
            }
        }
        unreachable!("the final attempt returns its result")
    }

    /// [`Media::fetch`], telling `progress` how much came of how much (if the server says).
    pub async fn fetch_with(
        &self,
        cache_dir: &Path,
        progress: impl Fn(u64, Option<u64>),
    ) -> Result<PathBuf, Error> {
        if let Some(path) = self.cached(cache_dir) {
            return Ok(path);
        }
        let path = self.cache_path(cache_dir);
        let mut data = download(self.url(), progress).await?;
        if let Media::Encrypted { iv, key, .. } = self {
            data = decrypt(&data, iv, key)?;
        }
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        // Written aside and renamed, so a half-written file is never taken for a whole one.
        let partial = path.with_file_name(format!(
            ".{}.part",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        if let Err(error) = async {
            tokio::fs::write(&partial, &data).await?;
            tokio::fs::rename(&partial, &path).await
        }
        .await
        {
            let _ = tokio::fs::remove_file(&partial).await;
            return Err(error.into());
        }
        Ok(path)
    }
}

async fn download(url: &str, progress: impl Fn(u64, Option<u64>)) -> Result<Vec<u8>, Error> {
    // ring, like the XMPP connection; the call fails harmlessly once it is installed.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .https_only(!cfg!(test))
        // A server that stops answering must not leave "downloading…" for ever.
        .connect_timeout(std::time::Duration::from_secs(20))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(download_error)?;
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(download_error)?;
    let total = response.content_length();
    if total.is_some_and(|len| len > MAX_SIZE) {
        return Err(Error::TooLarge);
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(download_error)? {
        data.extend_from_slice(&chunk);
        progress(data.len() as u64, total);
        // The announced length can lie; count what actually arrives.
        if data.len() as u64 > MAX_SIZE {
            return Err(Error::TooLarge);
        }
    }
    Ok(data)
}

/// AES-256-GCM with the tag at the end of the file, as XEP-0454 has it.
pub fn decrypt(data: &[u8], iv: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, Error> {
    if data.len() < 16 {
        return Err(Error::Decrypt);
    }
    let (body, tag) = data.split_at(data.len() - 16);
    let mut plain = body.to_vec();
    let tag = tag.try_into().map_err(|_| Error::Decrypt)?;
    let result = match iv.len() {
        12 => AesGcm::<Aes256, U12>::new(key.into()).decrypt_inout_detached(
            iv.try_into().map_err(|_| Error::Decrypt)?,
            &[],
            plain.as_mut_slice().into(),
            tag,
        ),
        16 => AesGcm::<Aes256, U16>::new(key.into()).decrypt_inout_detached(
            iv.try_into().map_err(|_| Error::Decrypt)?,
            &[],
            plain.as_mut_slice().into(),
            tag,
        ),
        _ => return Err(Error::Decrypt),
    };
    result.map_err(|_| Error::Decrypt)?;
    Ok(plain)
}

/// A file encrypted for sharing in an encrypted chat: AES-256-GCM under a fresh key and a
/// 12-byte IV, the tag at the end, as XEP-0454 has it and every OMEMO client reads.
pub struct Sealed {
    pub data: Vec<u8>,
    pub iv: [u8; 12],
    pub key: [u8; 32],
}

pub fn encrypt(plain: &[u8]) -> Sealed {
    let mut iv = [0u8; 12];
    let mut key = [0u8; 32];
    getrandom::fill(&mut iv).expect("system randomness");
    getrandom::fill(&mut key).expect("system randomness");
    let mut data = plain.to_vec();
    let tag = AesGcm::<Aes256, U12>::new((&key).into())
        .encrypt_inout_detached((&iv).into(), &[], data.as_mut_slice().into())
        .expect("AES-GCM takes any length a file has");
    data.extend_from_slice(&tag);
    Sealed { data, iv, key }
}

/// The link to share for a sealed file put at `url` (https://…): the scheme says it is
/// encrypted, the fragment (never sent to a server) carries IV and key.
pub fn aesgcm_link(url: &str, iv: &[u8; 12], key: &[u8; 32]) -> String {
    let location = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let secret: String = iv.iter().chain(key).map(|b| format!("{b:02x}")).collect();
    format!("aesgcm://{location}#{secret}")
}

/// PUTs `data` to `url`, as an upload slot says; `progress` hears how much went so far.
pub async fn upload(
    url: &str,
    headers: &[(String, String)],
    data: Vec<u8>,
    content_type: Option<&str>,
    progress: impl Fn(u64, u64) + Send + Sync + 'static,
) -> Result<(), Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .https_only(!cfg!(test) && !url.starts_with("http://127.0.0.1"))
        .connect_timeout(std::time::Duration::from_secs(20))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Upload(e.to_string()))?;
    let total = data.len() as u64;
    let data = bytes::Bytes::from(data);
    // In pieces, so that the progress moves as they go out.
    let pieces: Vec<bytes::Bytes> = (0..data.len())
        .step_by(UPLOAD_PIECE)
        .map(|at| data.slice(at..(at + UPLOAD_PIECE).min(data.len())))
        .collect();
    let mut sent = 0u64;
    let stream = futures_util::stream::iter(pieces.into_iter().map(move |piece| {
        sent += piece.len() as u64;
        progress(sent, total);
        Ok::<_, std::io::Error>(piece)
    }));
    let mut request = client
        .put(url)
        .header(reqwest::header::CONTENT_LENGTH, total)
        .body(reqwest::Body::wrap_stream(stream));
    if let Some(content_type) = content_type {
        request = request.header(reqwest::header::CONTENT_TYPE, content_type);
    }
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    request
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Upload(e.to_string()))?;
    Ok(())
}

const UPLOAD_PIECE: usize = 64 * 1024;

fn hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_previews_recognize_only_standalone_https_image_links() {
        for ext in ["png", "jpg", "jpeg", "GIF", "WebP", "bmp"] {
            let link = format!("https://example.org/picture.{ext}");
            assert!(Media::image_link(&link).is_some(), "{link}");
        }
        for text in [
            "https://example.org/file.pdf",
            "https://example.org/page/",
            "look https://example.org/cat.jpg",
            "http://example.org/cat.png",
            "https://example.org",
            "not a URL",
        ] {
            assert!(Media::image_link(text).is_none(), "{text}");
        }
    }

    #[test]
    fn any_encrypted_file_is_a_file_and_a_plain_link_only_when_shared() {
        let secret = "ab".repeat(44);
        for name in ["cat.png", "run.desktop", "omtalk-aarch64"] {
            assert!(
                Media::from_text(&format!("aesgcm://up.example.org/a/{name}#{secret}")).is_some(),
                "an encrypted {name} is a file"
            );
        }
        assert!(
            Media::from_text("https://up.example.org/a/cat.png").is_none(),
            "plain: only when shared"
        );
        assert!(Media::shared("https://up.example.org/a/cat.png").is_some());
    }

    fn encrypt(plain: &[u8], iv: &[u8; 12], key: &[u8; 32]) -> Vec<u8> {
        let mut data = plain.to_vec();
        let tag = AesGcm::<Aes256, U12>::new(key.into())
            .encrypt_inout_detached(iv.into(), &[], data.as_mut_slice().into())
            .unwrap();
        data.extend_from_slice(&tag);
        data
    }

    #[test]
    fn links_are_read_as_gajim_and_conversations_write_them() {
        // The link from the live test with Gajim on 22.10.2026, key changed.
        let link = "aesgcm://upload.example.org/01a1/feb0.png#3b78aa076c56fa93bab4bb7e75794bf3369293c49bee6dcd924132244217e9c10dfcb540a3a36418de5a95e2";
        let media = Media::from_text(link).unwrap();
        let Media::Encrypted { url, iv, key } = &media else {
            panic!("encrypted")
        };
        assert_eq!(url, "https://upload.example.org/01a1/feb0.png");
        assert_eq!(iv.len(), 12);
        assert_eq!(key[0], 0x75, "the key follows the 24 hex digits of the IV");
        assert!(media.is_image());
        assert_eq!(media.name(), "feb0.png");

        assert_eq!(
            Media::shared("https://example.org/a.jpg"),
            Some(Media::Plain {
                url: "https://example.org/a.jpg".into()
            })
        );
        assert_eq!(
            Media::shared("https://example.org/news/2026/10/22/"),
            None,
            "a page is a link, not a file"
        );
        assert_eq!(Media::shared("https://www.youtube.com/watch?v=abc"), None);
        assert!(Media::shared("https://up.example.org/x/report.PDF").is_some());
        assert_eq!(
            Media::shared("посмотри https://example.org/a.jpg"),
            None,
            "only a message that is just the link"
        );
        assert_eq!(
            Media::from_text("aesgcm://example.org/a.png#abc"),
            None,
            "a fragment that is no key"
        );
        assert_eq!(
            Media::from_text("http://example.org/a.png"),
            None,
            "no plain http"
        );
    }

    #[test]
    fn decryption_checks_the_tag() {
        let (iv, key) = ([7u8; 12], [9u8; 32]);
        let data = encrypt(b"picture bytes", &iv, &key);
        assert_eq!(decrypt(&data, &iv, &key).unwrap(), b"picture bytes");
        let mut tampered = data.clone();
        tampered[0] ^= 1;
        assert!(matches!(decrypt(&tampered, &iv, &key), Err(Error::Decrypt)));
        assert!(
            matches!(decrypt(&data, &iv, &[1u8; 32]), Err(Error::Decrypt)),
            "the wrong key"
        );
    }

    #[test]
    fn cache_names_differ_by_key_and_keep_the_extension() {
        let dir = Path::new("/cache");
        let a = Media::Encrypted {
            url: "https://x/a.png".into(),
            iv: vec![1; 12],
            key: [1; 32],
        };
        let b = Media::Encrypted {
            url: "https://x/a.png".into(),
            iv: vec![1; 12],
            key: [2; 32],
        };
        assert_ne!(a.cache_path(dir), b.cache_path(dir));
        assert_eq!(
            a.cache_path(dir).file_name().unwrap(),
            "a.png",
            "kept under its name"
        );
    }

    #[test]
    fn a_name_is_decoded_and_never_a_path() {
        let name = |url: &str| Media::Plain { url: url.into() }.name();
        assert_eq!(
            name("https://x/up/%D0%BE%D1%82%D1%87%D1%91%D1%82.pdf"),
            "отчёт.pdf"
        );
        assert_eq!(name("https://x/up/omtalk-aarch64"), "omtalk-aarch64");
        assert_eq!(name("https://x/up/a%20b.txt"), "a b.txt");
        assert_eq!(name("https://x/up/..%2F..%2F.bashrc"), ".bashrc");
        assert_eq!(name("https://x/up/%2E%2E"), "file");
        assert_eq!(name("https://x/up/"), "file");
    }

    #[test]
    fn a_file_kept_by_an_older_omtalk_is_moved_not_fetched_again() {
        let dir = tempfile::tempdir().unwrap();
        let media = Media::Plain {
            url: "https://x/up/omtalk-aarch64".into(),
        };
        let old = dir.path().join(media.digest());
        std::fs::write(&old, b"bin").unwrap();
        let found = media.cached(dir.path()).expect("found where it was");
        assert_eq!(found, media.cache_path(dir.path()));
        assert_eq!(std::fs::read(&found).unwrap(), b"bin");
        assert!(!old.is_file(), "the old name is the folder now");
        assert_eq!(media.cached(dir.path()), Some(found), "and found there next time");
    }

    /// Serves `body` once over plain HTTP on localhost; tests allow http.
    async fn serve_once(body: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        });
        format!("{addr}")
    }

    #[tokio::test]
    async fn fetch_decrypts_and_caches() {
        let (iv, key) = ([3u8; 12], [5u8; 32]);
        let addr = serve_once(encrypt(b"\x89PNG fake", &iv, &key)).await;
        let media = Media::Encrypted {
            url: format!("http://{addr}/x/p.png"),
            iv: iv.to_vec(),
            key,
        };
        let dir = tempfile::tempdir().unwrap();
        let path = media.fetch(dir.path()).await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG fake");
        // The server is gone; a second fetch must come from the cache.
        assert_eq!(media.fetch(dir.path()).await.unwrap(), path);
    }

    #[tokio::test]
    async fn a_fetch_tells_how_far_it_came() {
        let body: Vec<u8> = (0..400_000u32).map(|i| i as u8).collect();
        let addr = serve_once(body.clone()).await;
        let media = Media::Plain {
            url: format!("http://{addr}/x/big.bin"),
        };
        let dir = tempfile::tempdir().unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        media
            .fetch_with(dir.path(), |got, total| seen.lock().unwrap().push((got, total)))
            .await
            .unwrap();
        let seen = seen.into_inner().unwrap();
        assert!(seen.len() > 1, "in steps: {seen:?}");
        assert_eq!(seen.last(), Some(&(body.len() as u64, Some(body.len() as u64))));
    }
}

#[cfg(test)]
mod interrupted_download_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn interrupted_response_is_retryable_and_never_becomes_a_cached_file() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for complete in [false, true] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0; 2048];
                let _ = socket.read(&mut buf).await.unwrap();
                let body = if complete { "abcdef" } else { "abc" };
                socket
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n{body}")
                            .as_bytes(),
                    )
                    .await
                    .unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        let cache = tempfile::tempdir().unwrap();
        let media = Media::Plain {
            url: format!("http://{address}/voice.part"),
        };
        let error = media.fetch(cache.path()).await.unwrap_err();
        assert!(error.retryable(), "{error}");
        assert!(media.cached(cache.path()).is_none());
        let path = media.fetch(cache.path()).await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"abcdef");
        server.await.unwrap();
    }
    #[tokio::test]
    async fn permanent_http_errors_are_not_retried_but_server_errors_are_retryable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for code in [404, 503] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0; 2048];
                socket.read(&mut buf).await.unwrap();
                socket
                    .write_all(
                        format!("HTTP/1.1 {code} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        });
        assert!(
            !download(&format!("http://{address}/file"), |_, _| {})
                .await
                .unwrap_err()
                .retryable()
        );
        assert!(
            download(&format!("http://{address}/file"), |_, _| {})
                .await
                .unwrap_err()
                .retryable()
        );
        assert!(!Error::Decrypt.retryable());
        assert!(!Error::TooLarge.retryable());
        server.await.unwrap();
    }
}

#[cfg(test)]
mod automatic_retry_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn automatic_retry_recovers_a_truncated_audio_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for complete in [false, true] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = [0; 2048];
                socket.read(&mut buf).await.unwrap();
                let body = if complete { "abcdef" } else { "abc" };
                socket
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n{body}")
                            .as_bytes(),
                    )
                    .await
                    .unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        let cache = tempfile::tempdir().unwrap();
        let media = Media::Plain {
            url: format!("http://{address}/voice.ogg"),
        };
        let retries = std::sync::Mutex::new(Vec::new());
        let path = media
            .fetch_with_retry(cache.path(), |_, _| {}, |n| retries.lock().unwrap().push(n))
            .await
            .unwrap();
        assert_eq!(*retries.lock().unwrap(), [2]);
        assert_eq!(std::fs::read(path).unwrap(), b"abcdef");
        server.await.unwrap();
    }
}

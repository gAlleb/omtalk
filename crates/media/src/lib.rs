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
    #[error("upload failed: {0}")]
    Upload(String),
    #[error("the file is larger than {} MB", MAX_SIZE / 1024 / 1024)]
    TooLarge,
    #[error("the file does not decrypt with the key of the link")]
    Decrypt,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A shared file a message points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Media {
    Encrypted { url: String, iv: Vec<u8>, key: [u8; 32] },
    Plain { url: String },
}

impl Media {
    /// The file a message is about, if its whole text is one link.
    pub fn from_text(text: &str) -> Option<Self> {
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
            return Some(Media::Encrypted {
                url: format!("https://{location}"),
                iv: iv.to_vec(),
                key: key.try_into().ok()?,
            });
        }
        if text.starts_with("https://") && text.len() > "https://".len() {
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

    /// The file name the link ends in, for showing and for the extension.
    pub fn name(&self) -> String {
        let path = self.url().split(['?', '#']).next().unwrap_or_default();
        path.rsplit('/')
            .next()
            .filter(|n| !n.is_empty())
            .unwrap_or("file")
            .to_owned()
    }

    /// Whether the name looks like a picture worth showing inline.
    pub fn is_image(&self) -> bool {
        let name = self.name().to_ascii_lowercase();
        [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"]
            .iter()
            .any(|ext| name.ends_with(ext))
    }

    /// Where the file is kept once fetched: named after the link (key included), so a
    /// file is fetched once and two links never share a file.
    pub fn cache_path(&self, cache_dir: &Path) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(self.url().as_bytes());
        if let Media::Encrypted { iv, key, .. } = self {
            hasher.update(iv);
            hasher.update(key);
        }
        let digest: String = hasher
            .finalize()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect();
        let name = self.name();
        let extension = name
            .rsplit_once('.')
            .map(|(_, e)| format!(".{e}"))
            .unwrap_or_default();
        cache_dir.join(format!("{digest}{extension}"))
    }

    /// The file on disk, fetched and decrypted if it is not there yet.
    pub async fn fetch(&self, cache_dir: &Path) -> Result<PathBuf, Error> {
        let path = self.cache_path(cache_dir);
        if tokio::fs::try_exists(&path).await? {
            return Ok(path);
        }
        let mut data = download(self.url()).await?;
        if let Media::Encrypted { iv, key, .. } = self {
            data = decrypt(&data, iv, key)?;
        }
        tokio::fs::create_dir_all(cache_dir).await?;
        // Written aside and renamed, so a half-written file is never taken for a whole one.
        let partial = path.with_extension("part");
        tokio::fs::write(&partial, &data).await?;
        tokio::fs::rename(&partial, &path).await?;
        Ok(path)
    }
}

async fn download(url: &str) -> Result<Vec<u8>, Error> {
    // ring, like the XMPP connection; the call fails harmlessly once it is installed.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .https_only(!cfg!(test))
        .build()
        .map_err(|e| Error::Download(e.to_string()))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Download(e.to_string()))?;
    if response.content_length().is_some_and(|len| len > MAX_SIZE) {
        return Err(Error::TooLarge);
    }
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::Download(e.to_string()))?
    {
        data.extend_from_slice(&chunk);
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
            Media::from_text("https://example.org/a.jpg"),
            Some(Media::Plain {
                url: "https://example.org/a.jpg".into()
            })
        );
        assert_eq!(
            Media::from_text("посмотри https://example.org/a.jpg"),
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
        assert!(a.cache_path(dir).to_string_lossy().ends_with(".png"));
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
}

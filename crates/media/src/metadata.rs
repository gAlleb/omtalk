//! Metadata is learned only during an authorized download, never by probing links.
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE, HeaderMap};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Metadata {
    pub name: String,
    pub content_type: Option<String>,
}

impl Metadata {
    pub(crate) fn response(headers: &HeaderMap, fallback: String) -> Self {
        let content_type = headers.get(CONTENT_TYPE).and_then(|s| s.to_str().ok()).map(|s| {
            s.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        });
        let disposition = headers
            .get(CONTENT_DISPOSITION)
            .and_then(|s| s.to_str().ok())
            .unwrap_or_default();
        let value = |key: &str| {
            disposition.split(';').skip(1).find_map(|p| {
                let (name, value) = p.trim().split_once('=')?;
                name.eq_ignore_ascii_case(key)
                    .then(|| value.trim().trim_matches('"'))
            })
        };
        let name = value("filename*")
            .and_then(|s| {
                s.split_once("''")
                    .filter(|(enc, _)| enc.eq_ignore_ascii_case("UTF-8"))
            })
            .map(|(_, s)| {
                percent_encoding::percent_decode_str(s)
                    .decode_utf8_lossy()
                    .into_owned()
            })
            .or_else(|| value("filename").map(str::to_owned))
            .unwrap_or(fallback);
        let mut name: String = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect();
        if name.is_empty() || name == "." || name == ".." || name.starts_with('.') {
            name = "file".into();
        }
        let extension = match content_type.as_deref() {
            Some("image/png") => Some("png"),
            Some("image/jpeg") => Some("jpg"),
            Some("image/webp") => Some("webp"),
            Some("image/gif") => Some("gif"),
            Some("audio/ogg" | "application/ogg") => Some("ogg"),
            Some("audio/opus") => Some("opus"),
            Some("audio/mpeg") => Some("mp3"),
            Some("audio/flac" | "audio/x-flac") => Some("flac"),
            Some("audio/wav" | "audio/x-wav") => Some("wav"),
            Some("audio/mp4") => Some("m4a"),
            Some("video/mp4") => Some("mp4"),
            Some("application/pdf") => Some("pdf"),
            _ => None,
        };
        if let Some(ext) = extension
            && !known_file(&name)
        {
            name.push('.');
            name.push_str(ext);
        }
        Self { name, content_type }
    }
}

pub fn known_file(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        matches!(
            ext.to_ascii_lowercase().as_str(),
            "png"
                | "jpg"
                | "jpeg"
                | "webp"
                | "gif"
                | "bmp"
                | "ogg"
                | "opus"
                | "mp3"
                | "wav"
                | "flac"
                | "m4a"
                | "mp4"
                | "webm"
                | "pdf"
                | "zip"
                | "gz"
                | "xz"
                | "txt"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extensionless_audio_uses_metadata_and_cannot_escape_cache() {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, "audio/ogg".parse().unwrap());
        headers.insert(
            CONTENT_DISPOSITION,
            "attachment; filename*=UTF-8''..%2Fvoice".parse().unwrap(),
        );
        let m = Metadata::response(&headers, "opaque".into());
        assert_eq!(m.name, "voice.ogg");
    }
}

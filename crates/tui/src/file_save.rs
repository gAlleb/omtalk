//! Save a cached attachment without handing it to an external viewer.
use crate::app::{Action, App};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};

pub struct SaveFile {
    pub id: String,
    pub source: PathBuf,
    pub target: String,
    pub cursor: usize,
    pub confirm: bool,
    pub busy: bool,
    pub error: Option<String>,
}
fn downloads() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let configured = std::env::var("XDG_DOWNLOAD_DIR").ok().or_else(|| {
        std::fs::read_to_string(crate::config::config_dir().join("user-dirs.dirs"))
            .ok()
            .and_then(|text| {
                text.lines().find_map(|l| {
                    l.trim()
                        .strip_prefix("XDG_DOWNLOAD_DIR=")
                        .map(|s| s.trim_matches('"').to_owned())
                })
            })
    });
    configured
        .map(|p| {
            PathBuf::from(
                p.replace("${HOME}", &home.to_string_lossy())
                    .replace("$HOME", &home.to_string_lossy()),
            )
        })
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join("Downloads"))
}
fn expand(text: &str) -> PathBuf {
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(text)
}
impl App {
    pub fn save_file(&mut self, source: PathBuf) -> Vec<Action> {
        let target = downloads()
            .join(source.file_name().unwrap_or_default())
            .to_string_lossy()
            .into_owned();
        self.file_save = Some(SaveFile {
            id: omtalk_xmpp::receipts::new_id(),
            source,
            target: target.clone(),
            cursor: target.len(),
            confirm: false,
            busy: false,
            error: None,
        });
        Vec::new()
    }
    pub fn save_file_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.code == KeyCode::Esc {
            self.file_save = None;
            return Vec::new();
        }
        let s = self.file_save.as_mut().unwrap();
        if s.busy {
            return Vec::new();
        }
        match key.code {
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return vec![Action::PasteClipboard];
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                s.target.clear();
                s.cursor = 0;
                s.confirm = false;
            }
            KeyCode::Tab => {
                if let Some(path) = crate::complete::paths(&s.target[..s.cursor]).into_iter().next() {
                    s.target = path;
                    s.cursor = s.target.len();
                    s.confirm = false;
                }
            }
            KeyCode::Enter => return self.save_file_submit(),
            KeyCode::Left => {
                s.cursor = s.target[..s.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i)
            }
            KeyCode::Right => s.cursor += s.target[s.cursor..].chars().next().map_or(0, char::len_utf8),
            KeyCode::Home => s.cursor = 0,
            KeyCode::End => s.cursor = s.target.len(),
            KeyCode::Backspace => {
                if s.cursor > 0 {
                    let previous = s.target[..s.cursor].char_indices().next_back().unwrap().0;
                    s.target.drain(previous..s.cursor);
                    s.cursor = previous;
                    s.confirm = false;
                }
            }
            KeyCode::Delete => {
                if s.cursor < s.target.len() {
                    let end = s.cursor + s.target[s.cursor..].chars().next().unwrap().len_utf8();
                    s.target.drain(s.cursor..end);
                    s.confirm = false;
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                    && !c.is_control() =>
            {
                s.target.insert(s.cursor, c);
                s.cursor += c.len_utf8();
                s.confirm = false;
            }
            _ => {}
        }
        s.error = None;
        Vec::new()
    }
    pub fn save_file_paste(&mut self, text: &str) {
        let Some(s) = self.file_save.as_mut().filter(|s| !s.busy) else {
            return;
        };
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        s.target.insert_str(s.cursor, &text);
        s.cursor += text.len();
        s.confirm = false;
        s.error = None;
    }
    pub fn save_file_submit(&mut self) -> Vec<Action> {
        let Some(s) = self.file_save.as_mut().filter(|s| !s.busy) else {
            return Vec::new();
        };
        let mut target = expand(s.target.trim());
        if target.is_dir() {
            target = target.join(s.source.file_name().unwrap_or_default());
        }
        if target.as_os_str().is_empty() {
            s.error = Some(tr!("enter a file path", "введите путь к файлу").into());
            return Vec::new();
        }
        if target == s.source {
            s.error = Some(tr!("choose a path outside the cache", "выберите путь вне кэша").into());
            return Vec::new();
        }
        if target.exists() && !s.confirm {
            s.confirm = true;
            s.error = Some(
                tr!(
                    "File exists. Enter again to replace it; Esc cancels.",
                    "Файл существует. Ещё раз Enter — заменить; Esc — отмена."
                )
                .into(),
            );
            return Vec::new();
        }
        s.busy = true;
        vec![Action::SaveFile {
            id: s.id.clone(),
            source: s.source.clone(),
            target,
            overwrite: s.confirm,
        }]
    }
}

/// New files are created exclusively. Replacement is explicit and atomic; a
/// failed copy preserves the old destination and removes its temporary file.
pub async fn copy(source: &Path, target: &Path, overwrite: bool) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| e.to_string())?;
    let output = if overwrite {
        parent.join(format!(".omtalk-save-{}.part", omtalk_xmpp::receipts::new_id()))
    } else {
        target.to_owned()
    };
    struct Partial(PathBuf, bool);
    impl Drop for Partial {
        fn drop(&mut self) {
            if !self.1 {
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }
    let mut input = tokio::fs::File::open(source).await.map_err(|e| e.to_string())?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)
        .await
        .map_err(|e| e.to_string())?;
    let mut partial = Partial(output.clone(), false);
    let copied = async {
        tokio::io::copy(&mut input, &mut file).await?;
        file.flush().await?;
        file.sync_all().await
    }
    .await;
    drop(file);
    if let Err(e) = copied {
        let _ = tokio::fs::remove_file(&output).await;
        return Err(e.to_string());
    }
    if overwrite && let Err(e) = tokio::fs::rename(&output, target).await {
        let _ = tokio::fs::remove_file(&output).await;
        return Err(e.to_string());
    }
    partial.1 = true;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn saving_never_overwrites_without_confirmation_and_replacement_is_complete() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.ogg");
        let target = dir.path().join("voice.ogg");
        std::fs::write(&source, b"new voice").unwrap();
        std::fs::write(&target, b"old voice").unwrap();
        assert!(copy(&source, &target, false).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old voice");
        copy(&source, &target, true).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new voice");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}

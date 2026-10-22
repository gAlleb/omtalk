//! The system clipboards: the ordinary one (Ctrl+C/Ctrl+V elsewhere) and primary (what is
//! selected; the middle button pastes it). A program inside a terminal cannot reach them
//! itself: on Wayland wl-clipboard can, and anywhere else the terminal can be asked to
//! copy (OSC 52), also through tmux and SSH. Reading back works only with wl-paste.

use std::io::Write;
use std::process::{Command, Stdio};

use base64::Engine;

/// Copies `text` into both clipboards. `Ok` names how.
pub fn copy(text: &str) -> Result<&'static str, String> {
    if wayland() && wl_copy(text, false).is_ok() && wl_copy(text, true).is_ok() {
        return Ok("wl-copy");
    }
    let mut out = std::io::stdout();
    out.write_all(osc52(text, std::env::var_os("TMUX").is_some()).as_bytes())
        .and_then(|_| out.flush())
        .map_err(|e| e.to_string())?;
    Ok("OSC 52")
}

/// What is selected anywhere on the desktop (primary), as text.
pub fn primary() -> Result<String, String> {
    paste(&["--primary", "--no-newline", "--type", "text"])
}

/// The ordinary clipboard's text.
pub fn clipboard() -> Result<String, String> {
    paste(&["--no-newline", "--type", "text"])
}

/// The ordinary clipboard holds a picture, not text.
pub fn holds_image() -> bool {
    wayland()
        && Command::new("wl-paste")
            .arg("--list-types")
            .output()
            .is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|t| t.starts_with("image/"))
            })
}

/// Saves the picture in the ordinary clipboard as a PNG file in `folder`.
pub fn image_to(folder: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let output = Command::new("wl-paste")
        .args(["--type", "image/png"])
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("wl-paste: {e}"))?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err(tr!(
            "cannot get the picture from the clipboard",
            "картинку из буфера не получить"
        )
        .into());
    }
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let path = folder.join(trf!("picture-{stamp}.png", "картинка-{stamp}.png"));
    std::fs::write(&path, &output.stdout).map_err(|e| e.to_string())?;
    Ok(path)
}

fn paste(args: &[&str]) -> Result<String, String> {
    if !wayland() {
        return Err(tr!(
            "no Wayland: paste through the terminal (Shift+middle button)",
            "нет Wayland: вставка из буфера только через терминал (Shift+средняя кнопка)"
        )
        .into());
    }
    let output = Command::new("wl-paste")
        .args(args)
        .stderr(Stdio::null())
        .output()
        .map_err(|_| {
            tr!(
                "no wl-paste (package wl-clipboard): paste through the terminal, Shift+middle button",
                "нет wl-paste (пакет wl-clipboard): вставка через терминал — Shift+средняя кнопка"
            )
            .to_string()
        })?;
    if !output.status.success() {
        return Err(tr!("the clipboard is empty", "буфер пуст").into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn wl_copy(text: &str, primary: bool) -> Result<(), String> {
    let mut command = Command::new("wl-copy");
    if primary {
        command.arg("--primary");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    // It forks a copy that stays to serve the clipboard and exits itself: wait for that,
    // so it leaves no zombie.
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(status.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// The terminal's copy request for both clipboards. Inside tmux it is wrapped so that
/// tmux hands it on to the terminal (with `allow-passthrough on`), whatever tmux's own
/// `set-clipboard` says.
pub fn osc52(text: &str, tmux: bool) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(text);
    let request = format!("\x1b]52;cp;{data}\x07");
    if tmux {
        format!("\x1bPtmux;{}\x1b\\", request.replace('\x1b', "\x1b\x1b"))
    } else {
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copy_request_names_both_clipboards_and_passes_through_tmux() {
        assert_eq!(osc52("hi", false), "\x1b]52;cp;aGk=\x07");
        assert_eq!(osc52("hi", true), "\x1bPtmux;\x1b\x1b]52;cp;aGk=\x07\x1b\\");
    }
}

//! The system clipboards: the ordinary one (Ctrl+C/Ctrl+V elsewhere) and primary (what is
//! selected; the middle button pastes it). A program inside a terminal cannot reach them
//! itself: on Wayland wl-clipboard can, on X11 xclip (or xsel, text only), and anywhere
//! else the terminal can be asked to copy (OSC 52), also through tmux and SSH. Reading
//! back needs one of those programs.

use std::io::Write;
use std::process::{Command, Stdio};

use base64::Engine;

/// Who reaches the desktop's clipboards from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Wayland,
    Xclip,
    Xsel,
}

impl Tool {
    /// Wayland first (wl-clipboard), then X11 (xclip, which also does pictures, else xsel):
    /// whichever the session has and is installed.
    fn found() -> Option<Self> {
        // Looked for on PATH, not run: run with any word, xsel may take it for
        // "set the clipboard".
        let installed = |program: &str| {
            std::env::var_os("PATH")
                .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
        };
        Self::pick(
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
            std::env::var_os("DISPLAY").is_some(),
            installed,
        )
    }

    fn pick(wayland: bool, x11: bool, installed: impl Fn(&str) -> bool) -> Option<Self> {
        if wayland {
            return Some(Tool::Wayland);
        }
        if !x11 {
            return None;
        }
        [(Tool::Xclip, "xclip"), (Tool::Xsel, "xsel")]
            .into_iter()
            .find(|(_, program)| installed(program))
            .map(|(tool, _)| tool)
    }

    /// The command writing the clipboard (or primary) from its input.
    fn write(self, primary: bool) -> Vec<&'static str> {
        match (self, primary) {
            (Tool::Wayland, false) => vec!["wl-copy"],
            (Tool::Wayland, true) => vec!["wl-copy", "--primary"],
            (Tool::Xclip, false) => vec!["xclip", "-selection", "clipboard", "-in"],
            (Tool::Xclip, true) => vec!["xclip", "-selection", "primary", "-in"],
            (Tool::Xsel, false) => vec!["xsel", "--clipboard", "--input"],
            (Tool::Xsel, true) => vec!["xsel", "--primary", "--input"],
        }
    }

    /// The command printing the clipboard's (or primary's) text.
    fn read(self, primary: bool) -> Vec<&'static str> {
        match (self, primary) {
            (Tool::Wayland, false) => vec!["wl-paste", "--no-newline", "--type", "text"],
            (Tool::Wayland, true) => vec!["wl-paste", "--primary", "--no-newline", "--type", "text"],
            (Tool::Xclip, false) => vec!["xclip", "-selection", "clipboard", "-out"],
            (Tool::Xclip, true) => vec!["xclip", "-selection", "primary", "-out"],
            (Tool::Xsel, false) => vec!["xsel", "--clipboard", "--output"],
            (Tool::Xsel, true) => vec!["xsel", "--primary", "--output"],
        }
    }

    /// The commands listing the clipboard's types and printing it as a PNG; xsel has
    /// none, it deals in text only.
    fn picture(self) -> Option<(Vec<&'static str>, Vec<&'static str>)> {
        match self {
            Tool::Wayland => Some((
                vec!["wl-paste", "--list-types"],
                vec!["wl-paste", "--type", "image/png"],
            )),
            Tool::Xclip => Some((
                vec!["xclip", "-selection", "clipboard", "-target", "TARGETS", "-out"],
                vec!["xclip", "-selection", "clipboard", "-target", "image/png", "-out"],
            )),
            Tool::Xsel => None,
        }
    }
}

/// Runs `command` and takes what it prints, if it succeeds.
fn output(command: &[&str]) -> Option<Vec<u8>> {
    let (program, args) = command.split_first()?;
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// Copies `text` into both clipboards. `Ok` names how.
pub fn copy(text: &str) -> Result<&'static str, String> {
    if let Some(tool) = Tool::found()
        && write(tool, text, false).is_ok()
        && write(tool, text, true).is_ok()
    {
        return Ok(tool.write(false)[0]);
    }
    let mut out = std::io::stdout();
    out.write_all(osc52(text, std::env::var_os("TMUX").is_some()).as_bytes())
        .and_then(|_| out.flush())
        .map_err(|e| e.to_string())?;
    Ok("OSC 52")
}

/// What is selected anywhere on the desktop (primary), as text.
pub fn primary() -> Result<String, String> {
    paste(true)
}

/// The ordinary clipboard's text.
pub fn clipboard() -> Result<String, String> {
    paste(false)
}

/// The ordinary clipboard holds a picture, not text.
pub fn holds_image() -> bool {
    Tool::found()
        .and_then(Tool::picture)
        .and_then(|(types, _)| output(&types))
        .is_some_and(|types| {
            String::from_utf8_lossy(&types)
                .lines()
                .any(|t| t.starts_with("image/"))
        })
}

/// Saves the picture in the ordinary clipboard as a PNG file in `folder`.
pub fn image_to(folder: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let png = Tool::found()
        .and_then(Tool::picture)
        .and_then(|(_, png)| output(&png))
        .filter(|png| !png.is_empty())
        .ok_or_else(|| {
            tr!(
                "cannot get the picture from the clipboard",
                "картинку из буфера не получить"
            )
            .to_owned()
        })?;
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    // The name goes to the server and the other side: plain Latin, whatever the
    // interface's language.
    let path = folder.join(format!("picture-{stamp}.png"));
    std::fs::write(&path, &png).map_err(|e| e.to_string())?;
    Ok(path)
}

fn paste(primary: bool) -> Result<String, String> {
    let Some(tool) = Tool::found() else {
        return Err(tr!(
            "no wl-paste, xclip or xsel: paste through the terminal (Shift+middle button)",
            "нет wl-paste, xclip или xsel: вставка через терминал (Shift+средняя кнопка)"
        )
        .into());
    };
    let text =
        output(&tool.read(primary)).ok_or_else(|| tr!("the clipboard is empty", "буфер пуст").to_owned())?;
    Ok(String::from_utf8_lossy(&text).into_owned())
}

/// Hands `text` to `tool` for the clipboard (or primary).
fn write(tool: Tool, text: &str, primary: bool) -> Result<(), String> {
    let command = tool.write(primary);
    let (program, args) = command.split_first().expect("a program");
    let mut child = Command::new(program)
        .args(args)
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
    // Each forks a copy that stays to serve the clipboard and exits itself: wait for
    // that, so it leaves no zombie.
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
    fn wayland_first_then_xclip_then_xsel_and_each_is_called_as_it_wants() {
        let both = |_: &str| true;
        let only_xsel = |p: &str| p == "xsel";
        let none = |_: &str| false;
        assert_eq!(Tool::pick(true, true, none), Some(Tool::Wayland), "Wayland's own");
        assert_eq!(
            Tool::pick(false, true, both),
            Some(Tool::Xclip),
            "xclip does pictures too"
        );
        assert_eq!(Tool::pick(false, true, only_xsel), Some(Tool::Xsel));
        assert_eq!(Tool::pick(false, true, none), None);
        assert_eq!(Tool::pick(false, false, both), None, "no desktop: the terminal");

        assert_eq!(
            Tool::Xclip.read(false),
            ["xclip", "-selection", "clipboard", "-out"]
        );
        assert_eq!(Tool::Xclip.write(true), ["xclip", "-selection", "primary", "-in"]);
        assert_eq!(Tool::Xsel.read(true), ["xsel", "--primary", "--output"]);
        assert_eq!(Tool::Xsel.write(false), ["xsel", "--clipboard", "--input"]);
        assert_eq!(
            Tool::Wayland.read(true),
            ["wl-paste", "--primary", "--no-newline", "--type", "text"]
        );
        assert!(Tool::Xsel.picture().is_none(), "xsel deals in text only");
        let (_, png) = Tool::Xclip.picture().unwrap();
        assert_eq!(
            png,
            ["xclip", "-selection", "clipboard", "-target", "image/png", "-out"]
        );
    }

    #[test]
    fn the_copy_request_names_both_clipboards_and_passes_through_tmux() {
        assert_eq!(osc52("hi", false), "\x1b]52;cp;aGk=\x07");
        assert_eq!(osc52("hi", true), "\x1bPtmux;\x1b\x1b]52;cp;aGk=\x07\x1b\\");
    }
}

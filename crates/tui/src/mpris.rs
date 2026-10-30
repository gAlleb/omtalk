//! MPRIS through playerctl; arguments are passed directly, never through a shell.
use std::time::Duration;
use tokio::process::Command;

async fn query(args: &[&str]) -> Result<String, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(3),
        Command::new("playerctl").args(args).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| tr!("MPRIS: playerctl timed out", "MPRIS: playerctl не ответил").to_owned())?
    .map_err(|e| format!("MPRIS (playerctl): {e}"))?;
    if !output.status.success() {
        return Err(tr!("MPRIS: no playing track", "MPRIS: нет играющего трека").into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub async fn now_playing() -> Result<String, String> {
    let names = query(&["--list-all"]).await?;
    let mut names: Vec<_> = names.lines().collect();
    names.sort_unstable();
    for name in names {
        if query(&["--player", name, "status"]).await.as_deref() != Ok("Playing") {
            continue;
        }
        let title = query(&["--player", name, "metadata", "--format", "{{title}}"]).await?;
        let artist = query(&["--player", name, "metadata", "--format", "{{artist}}"]).await?;
        return format_track(&artist, &title).ok_or_else(|| {
            tr!(
                "MPRIS: track title unavailable",
                "MPRIS: название трека недоступно"
            )
            .into()
        });
    }
    Err(tr!("MPRIS: no playing track", "MPRIS: нет играющего трека").into())
}

fn format_track(artist: &str, title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    Some(if artist.trim().is_empty() {
        format!("/me np: {title}")
    } else {
        format!("/me np: {} — {title}", artist.trim())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn metadata_requires_a_title_and_allows_no_artist() {
        assert_eq!(
            super::format_track(" Artist ", " Title ").as_deref(),
            Some("/me np: Artist — Title")
        );
        assert_eq!(super::format_track("", "Title").as_deref(), Some("/me np: Title"));
        assert_eq!(super::format_track("Artist", " "), None);
    }
}

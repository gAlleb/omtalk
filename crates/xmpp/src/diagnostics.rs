//! Opt-in, bounded presence diagnostics shared by the wire and TUI layers.
//! Callers log routing/roster metadata only, never message bodies or input keys.
use crate::BareJid;
use std::{fs::OpenOptions, io::Write, path::Path, sync::Mutex, time::SystemTime};

pub fn room_presence(stage: &str, account: &BareJid, details: impl FnOnce() -> String) {
    let Some(path) = std::env::var_os("OMTALK_PRESENCE_LOG") else {
        return;
    };
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let line = format!(
        "{millis} pid={} stage={stage} account={account} {}\n",
        std::process::id(),
        details()
    );
    let _ = append(Path::new(&path), &line, 1024 * 1024);
}
fn append(path: &Path, line: &str, limit: u64) -> std::io::Result<()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let Ok(_guard) = LOCK.lock() else {
        return Ok(());
    };
    if std::fs::metadata(path).is_ok_and(|m| m.len() + line.len() as u64 > limit) {
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        std::fs::rename(path, old)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(line.as_bytes())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presence_trace_rotates_and_remains_private() {
        let path = std::env::temp_dir().join(format!(
            "omtalk-presence-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        append(&path, "first event\n", 20).unwrap();
        append(&path, "second event\n", 20).unwrap();
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second event\n");
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "first event\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(old).unwrap();
    }
}

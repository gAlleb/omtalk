//! Audio controls and a recording draft bound to the chat where recording began.
use crate::address::Address;
use crate::{
    app::{Action, App, ChatKind},
    audio,
};
use crossterm::event::{KeyCode, KeyEvent};
use std::path::PathBuf;

pub struct Recorder {
    pub account: Address,
    pub chat: Address,
    pub kind: ChatKind,
    pub path: PathBuf,
    pub seconds: f64,
    pub level: u8,
    pub recording: bool,
    pub busy: bool,
    pub ready: bool,
    pub error: Option<String>,
    pub button: usize,
}
impl App {
    pub fn audio_command(&mut self, seconds: Option<f64>) -> Vec<Action> {
        let Some(chat) = self.current() else {
            return Vec::new();
        };
        let under = self.selection.and_then(|(a, _)| {
            self.areas
                .message_lines
                .iter()
                .find(|(r, _)| r.y == a.1)
                .map(|(_, line)| *line)
        });
        let slot = under
            .and_then(|i| chat.lines.get(i))
            .and_then(|l| l.media.as_ref())
            .or_else(|| {
                chat.lines
                    .iter()
                    .rev()
                    .filter_map(|l| l.media.as_ref())
                    .find(|m| crate::audio::supported(std::path::Path::new(&m.name)))
            });
        if let Some(slot) = slot
            && let crate::app::MediaState::Ready(path) = &slot.state
            && audio::supported(path)
        {
            return vec![Action::Audio(match seconds {
                Some(s) => audio::Command::Seek(
                    path.clone(),
                    self.audio_info
                        .get(path)
                        .and_then(|i| i.as_ref().ok())
                        .map_or(s, |i| s.min(i.seconds)),
                ),
                None => audio::Command::Toggle(path.clone()),
            })];
        }
        self.status = tr!(
            "no downloaded audio selected",
            "нет загруженного аудио для проигрывания"
        )
        .into();
        Vec::new()
    }
    pub fn record_voice(&mut self) -> Vec<Action> {
        if self.recorder.is_some() {
            return Vec::new();
        }
        if let Some(error) = self.selected.and_then(|i| self.irc_upload_error(i)) {
            self.status = error.into();
            return Vec::new();
        }
        let Some(chat) = self.current().filter(|c| c.kind != ChatKind::Server) else {
            self.status = tr!(
                "open a chat to record a voice message",
                "открой чат для записи голосового"
            )
            .into();
            return Vec::new();
        };
        let path = crate::config::cache_dir().join(format!("voice-{}.ogg", omtalk_xmpp::receipts::new_id()));
        self.recorder = Some(Recorder {
            account: chat.account.clone(),
            chat: chat.jid.clone(),
            kind: chat.kind.clone(),
            path: path.clone(),
            seconds: 0.0,
            level: 0,
            recording: true,
            busy: false,
            ready: false,
            error: None,
            button: 0,
        });
        vec![Action::Audio(audio::Command::Record(path))]
    }
    pub fn recorder_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let r = self.recorder.as_mut().unwrap();
        match key.code {
            KeyCode::Esc => self.recorder_button(2),
            KeyCode::Left | KeyCode::BackTab => {
                r.button = (r.button + 2) % 3;
                Vec::new()
            }
            KeyCode::Right | KeyCode::Tab => {
                r.button = (r.button + 1) % 3;
                Vec::new()
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let button = r.button;
                self.recorder_button(button)
            }
            _ => Vec::new(),
        }
    }
    pub fn recorder_button(&mut self, button: usize) -> Vec<Action> {
        let Some(r) = self.recorder.as_mut() else {
            return Vec::new();
        };
        if button == 2 {
            let r = self.recorder.take().unwrap();
            return vec![Action::Audio(audio::Command::Cancel(r.path))];
        }
        if r.busy {
            return Vec::new();
        }
        if button == 0 {
            if r.recording {
                r.busy = true;
                return vec![Action::Audio(audio::Command::Finish)];
            }
            if r.ready {
                return vec![Action::Audio(audio::Command::Toggle(r.path.clone()))];
            }
        }
        if button == 1 && r.ready {
            if r.chat.is_irc() {
                let available = self
                    .chats
                    .iter()
                    .any(|c| c.account == r.account && c.jid == r.chat)
                    && r.chat.conversation().is_some_and(|id| {
                        self.irc_conversations.get(id).is_some_and(|s| {
                            !s.hidden && (!matches!(r.kind, ChatKind::Room { .. }) || s.joined)
                        })
                    });
                if !available {
                    r.error = Some(
                        tr!(
                            "original IRC chat is closed or left; recording kept",
                            "исходный IRC-чат закрыт или покинут; запись сохранена"
                        )
                        .into(),
                    );
                    return Vec::new();
                }
            }
            // Check before consuming the recording. A disconnected FILEHOST or
            // a capability removed mid-recording leaves the preview draft intact.
            let original = self.recorder.as_ref().unwrap();
            if let Some(i) = self
                .chats
                .iter()
                .position(|c| c.account == original.account && c.jid == original.chat)
                && let Some(error) = self.irc_upload_error(i)
            {
                self.recorder.as_mut().unwrap().error = Some(error.into());
                return Vec::new();
            }
            let r = self.recorder.take().unwrap();
            let index = self.chat(&r.account, &r.chat, r.kind);
            let mut actions = vec![Action::Audio(audio::Command::Stop)];
            actions.extend(self.upload(index, r.path));
            return actions;
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_stays_bound_to_its_original_room_and_cancel_never_sends() {
        let me: Address = "me@example.org".parse().unwrap();
        let room: Address = "room@muc.example.org".parse().unwrap();
        let mut app = App::new(vec![me.clone()], crate::config::Modifier::Ctrl, 26, 30);
        let index = app.chat(&me, &room, ChatKind::Room { nick: "me".into() });
        app.select(index);
        assert!(matches!(
            app.record_voice().as_slice(),
            [Action::Audio(audio::Command::Record(_))]
        ));
        let target = app.recorder.as_mut().unwrap();
        target.recording = false;
        target.ready = true;
        let private = app.chat(&me, &"other@example.org".parse().unwrap(), ChatKind::Direct);
        app.select(private);
        let actions = app.recorder_button(1);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a,Action::Upload{account,to,room:true,..}if account==&me&&to==&room))
        );
        app.record_voice();
        let actions = app.recorder_button(2);
        assert!(matches!(
            actions.as_slice(),
            [Action::Audio(audio::Command::Cancel(_))]
        ));
        assert!(app.recorder.is_none());
    }
}

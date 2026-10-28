//! A draft position is bound to the playing slot, independently of list selection.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use radiko::{
    api::stream::ProbeMode,
    domain::ProgramKey,
    player::{seek::Command, state::PlaybackSnapshot},
};
use std::time::Duration;

pub struct Editor {
    key: ProgramKey,
    length: Duration,
    pub input: String,
    pub error: String,
}
pub enum Action {
    None,
    Close,
    Submit(Command),
}
impl Editor {
    pub fn new(snapshot: &PlaybackSnapshot) -> Result<Self, &'static str> {
        if snapshot.info.mode != ProbeMode::TimeFree {
            return Err("ライブは位置指定できません（現在の再生を継続）");
        }
        let length = (snapshot.info.ends_at - snapshot.info.starts_at)
            .to_std()
            .map_err(|_| "放送枠の長さを確認できません")?;
        Ok(Self {
            key: ProgramKey {
                station_id: snapshot.info.station.clone(),
                starts_at: snapshot.info.starts_at,
            },
            length,
            input: String::new(),
            error: String::new(),
        })
    }
    pub fn prompt(&self) -> String {
        format!(
            "位置指定: {} / {} JST / 枠内の秒数（0以上{}未満）: {}\nEnter 確定 / Esc 取消 / Ctrl+U 消去 / {}",
            self.key.station_id,
            super::local(self.key.starts_at).format("%m/%d %H:%M %:z"),
            self.length.as_secs(),
            self.input,
            self.error
        )
    }
    pub fn handle(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => return Action::Close,
            KeyCode::Enter => {
                let position = self
                    .input
                    .parse::<u64>()
                    .ok()
                    .map(Duration::from_secs)
                    .filter(|p| *p < self.length);
                if let Some(position) = position {
                    return Action::Submit(Command::SeekSlot {
                        key: self.key.clone(),
                        position,
                    });
                }
                self.error = "放送枠内の整数秒を入力してください".into();
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.error.clear();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.clear();
                self.error.clear();
            }
            KeyCode::Char(c)
                if c.is_ascii_digit()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.input.len() < 20 =>
            {
                self.input.push(c);
                self.error.clear();
            }
            _ => (),
        }
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use radiko::{api::stream::playback::StreamInfo, player::state::PlaybackState};
    fn snapshot() -> PlaybackSnapshot {
        let start = chrono::DateTime::parse_from_rfc3339("2026-10-16T12:00:00+09:00").unwrap();
        PlaybackSnapshot {
            pipeline: None,
            info: StreamInfo {
                station: "TBS".into(),
                title: None,
                mode: ProbeMode::TimeFree,
                starts_at: start,
                ends_at: start + chrono::Duration::hours(1),
                position: Duration::ZERO,
            },
            state: PlaybackState::Playing,
            pending: None,
            volume: 25,
            position: Some(Duration::from_secs(25)),
            output: None,
            segments: 1,
            receiving_for: None,
            reconnecting_for: None,
        }
    }
    #[test]
    fn draft_validates_boundaries_overflow_and_preserves_slot_identity() {
        let snapshot = snapshot();
        let mut editor = Editor::new(&snapshot).unwrap();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        for text in ["", "-1", "3600", "18446744073709551616"] {
            editor.input = text.into();
            assert!(matches!(editor.handle(enter), Action::None));
            assert!(!editor.error.is_empty());
        }
        for seconds in [0, 3599] {
            editor.input = seconds.to_string();
            let Action::Submit(Command::SeekSlot { key, position }) = editor.handle(enter) else {
                panic!("valid position")
            };
            assert_eq!(key.station_id, snapshot.info.station);
            assert_eq!(key.starts_at, snapshot.info.starts_at);
            assert_eq!(position.as_secs(), seconds);
        }
        assert!(matches!(
            editor.handle(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Action::Close
        ));
    }
    #[test]
    fn live_is_rejected_and_input_has_a_fixed_bound() {
        let mut snapshot = snapshot();
        snapshot.info.mode = ProbeMode::Live;
        assert!(Editor::new(&snapshot).is_err());
        snapshot.info.mode = ProbeMode::TimeFree;
        let mut editor = Editor::new(&snapshot).unwrap();
        for _ in 0..40 {
            editor.handle(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::NONE));
        }
        assert_eq!(editor.input.len(), 20);
        editor.handle(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(editor.input.is_empty());
        for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            editor.handle(KeyEvent::new(KeyCode::Char('1'), modifier));
        }
        assert!(editor.input.is_empty());
        editor.handle(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        assert_eq!(editor.input, "1");
    }
}

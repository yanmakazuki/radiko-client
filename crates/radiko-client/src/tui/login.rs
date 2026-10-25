//! Local, zeroized credential draft. Rendering never exposes the password.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use radiko::app::auth::Credentials;
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Paragraph},
};
use zeroize::Zeroizing;
#[derive(Default)]
pub struct Editor {
    mail: Zeroizing<String>,
    password: Zeroizing<String>,
    password_focus: bool,
    error: String,
}
pub enum Action {
    None,
    Close,
    Submit(Credentials),
}
impl Editor {
    fn text(&self) -> String {
        format!(
            "{} メールアドレス: {}\n{} パスワード: {}\n\n{}",
            if self.password_focus { " " } else { ">" },
            self.mail.as_str(),
            if self.password_focus { ">" } else { " " },
            if self.password.is_empty() {
                "未入力"
            } else {
                "入力済み（非表示）"
            },
            self.error
        )
    }
    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(self.text()).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("ログイン（パスワード非表示）"),
            ),
            area,
        );
    }
    pub fn handle(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => return Action::Close,
            KeyCode::Tab | KeyCode::BackTab => self.password_focus = !self.password_focus,
            KeyCode::Enter if !self.password_focus => self.password_focus = true,
            KeyCode::Enter => {
                if self.mail.trim().is_empty() || self.password.is_empty() {
                    self.error = "メールアドレスとパスワードを入力してください".into();
                } else {
                    match Credentials::new(
                        std::mem::take(&mut self.mail),
                        std::mem::take(&mut self.password),
                    ) {
                        Ok(credentials) => return Action::Submit(credentials),
                        Err(reason) => self.error = reason.into(),
                    }
                }
            }
            KeyCode::Backspace => {
                self.current().pop();
                self.error.clear();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.current().clear();
                self.error.clear();
            }
            KeyCode::Char(c)
                if !c.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let limit = if self.password_focus { 4096 } else { 1024 };
                if self.current().len() + c.len_utf8() <= limit {
                    self.current().push(c);
                    self.error.clear();
                }
            }
            _ => (),
        }
        Action::None
    }
    fn current(&mut self) -> &mut Zeroizing<String> {
        if self.password_focus {
            &mut self.password
        } else {
            &mut self.mail
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn typing_and_focus_never_submit_or_render_the_password() {
        let mut editor = Editor::default();
        for c in "user@example.invalid".chars() {
            assert!(matches!(editor.handle(key(KeyCode::Char(c))), Action::None));
        }
        assert!(matches!(editor.handle(key(KeyCode::Enter)), Action::None));
        for c in "SYNTHETIC_PASSWORD_99".chars() {
            assert!(matches!(editor.handle(key(KeyCode::Char(c))), Action::None));
            assert!(!editor.text().contains("SYNTHETIC"));
        }
        assert!(editor.text().contains("入力済み（非表示）"));
        assert!(matches!(
            editor.handle(key(KeyCode::Enter)),
            Action::Submit(_)
        ));
        assert!(editor.password.is_empty());
        assert!(editor.mail.is_empty());
    }
    #[test]
    fn missing_values_cancel_unicode_and_limits_are_local_only() {
        let mut editor = Editor::default();
        editor.handle(key(KeyCode::Enter));
        assert!(matches!(editor.handle(key(KeyCode::Enter)), Action::None));
        assert!(!editor.error.is_empty());
        editor.handle(key(KeyCode::Char('あ')));
        editor.handle(key(KeyCode::Backspace));
        assert!(editor.password.is_empty());
        for _ in 0..4100 {
            editor.handle(key(KeyCode::Char('x')));
        }
        assert_eq!(editor.password.len(), 4096);
        editor.handle(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert!(editor.password.is_empty());
        assert!(matches!(
            editor.handle(key(KeyCode::Char('q'))),
            Action::None
        ));
        assert!(matches!(editor.handle(key(KeyCode::Esc)), Action::Close));
    }
}

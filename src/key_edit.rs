//! Normalize terminal-specific key codes for line editing.
//!
//! Many Linux/SSH terminals send Backspace as DEL (`\x7f`) or BS (`\x08`) as `KeyCode::Char`
//! instead of `KeyCode::Backspace`. Emacs-style Ctrl+B/F/A/E bindings are common fallbacks.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditAction {
    Insert(char),
    InsertStr(String),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
}

/// Returns `true` when the key requests a clipboard paste (caller should read clipboard / handle `Event::Paste`).
pub fn is_paste_key(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Insert if key.modifiers.contains(KeyModifiers::SHIFT) => true,
        KeyCode::Char('v') | KeyCode::Char('V')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            true
        }
        _ => false,
    }
}

pub fn map_key_to_edit(key: &KeyEvent) -> Option<EditAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        KeyCode::Backspace => Some(EditAction::Backspace),
        KeyCode::Delete => Some(EditAction::Delete),
        KeyCode::Left => Some(EditAction::Left),
        KeyCode::Right => Some(EditAction::Right),
        KeyCode::Home => Some(EditAction::Home),
        KeyCode::End => Some(EditAction::End),
        KeyCode::Char(c) => match c {
            '\x08' | '\x7f' => Some(EditAction::Backspace),
            '\x02' if ctrl => Some(EditAction::Left),
            '\x06' if ctrl => Some(EditAction::Right),
            '\x01' if ctrl => Some(EditAction::Home),
            '\x05' if ctrl => Some(EditAction::End),
            '\t' | '\n' | '\r' => None,
            c if c.is_control() => None,
            c => Some(EditAction::Insert(c)),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn del_char_is_backspace() {
        assert_eq!(
            map_key_to_edit(&key(KeyCode::Char('\x7f'), KeyModifiers::NONE)),
            Some(EditAction::Backspace)
        );
    }

    #[test]
    fn ctrl_b_is_left() {
        assert_eq!(
            map_key_to_edit(&key(KeyCode::Char('\x02'), KeyModifiers::CONTROL)),
            Some(EditAction::Left)
        );
    }

    #[test]
    fn printable_char_inserts() {
        assert_eq!(
            map_key_to_edit(&key(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(EditAction::Insert('x'))
        );
    }
}

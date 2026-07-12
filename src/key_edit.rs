//! Normalize terminal-specific key codes for line editing.
//!
//! Linux/SSH terminals disagree on Backspace:
//! - Many send DEL (`\x7f`) or map it to [`KeyCode::Backspace`]
//! - Others (erase = `^H`) send BS as **`Char('h')` + CONTROL**, not `KeyCode::Backspace`
//! - Ctrl+Backspace often arrives as `KeyCode::Backspace` + CONTROL (works if we ignore modifiers)
//!
//! Emacs-style Ctrl+B/F/A/E/D are common fallbacks.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditAction {
    Insert(char),
    #[allow(dead_code)]
    InsertStr(String),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
}

/// Ignore key-release / other non-press events (enhanced keyboard protocols).
pub fn is_key_press_or_repeat(key: &KeyEvent) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
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

/// True for any common "delete previous character" encoding.
pub fn is_backspace_key(key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        // Named Backspace (with or without Ctrl/Alt — Ctrl+Backspace is still delete-left)
        KeyCode::Backspace => true,
        // Raw BS / DEL as character codes
        KeyCode::Char('\x08' | '\x7f') => true,
        // termios erase = ^H: crossterm often reports this as Ctrl+H, not KeyCode::Backspace
        KeyCode::Char('h' | 'H') if ctrl => true,
        _ => false,
    }
}

pub fn map_key_to_edit(key: &KeyEvent) -> Option<EditAction> {
    if !is_key_press_or_repeat(key) {
        return None;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    if is_backspace_key(key) {
        return Some(EditAction::Backspace);
    }

    match key.code {
        KeyCode::Delete => Some(EditAction::Delete),
        // Emacs: Ctrl+D forward-delete
        KeyCode::Char('d' | 'D') if ctrl => Some(EditAction::Delete),
        KeyCode::Left => Some(EditAction::Left),
        KeyCode::Right => Some(EditAction::Right),
        KeyCode::Home => Some(EditAction::Home),
        KeyCode::End => Some(EditAction::End),
        KeyCode::Char(c) => match c {
            '\x02' if ctrl => Some(EditAction::Left),  // Ctrl+B
            '\x06' if ctrl => Some(EditAction::Right), // Ctrl+F
            '\x01' if ctrl => Some(EditAction::Home),  // Ctrl+A
            '\x05' if ctrl => Some(EditAction::End),   // Ctrl+E
            '\t' | '\n' | '\r' => None,
            c if c.is_control() => None,
            // Don't insert when Control/Alt is held (except bindings above)
            _ if ctrl || key.modifiers.contains(KeyModifiers::ALT) => None,
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
    fn named_backspace() {
        assert_eq!(
            map_key_to_edit(&key(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(EditAction::Backspace)
        );
    }

    #[test]
    fn ctrl_backspace_named() {
        assert_eq!(
            map_key_to_edit(&key(KeyCode::Backspace, KeyModifiers::CONTROL)),
            Some(EditAction::Backspace)
        );
    }

    #[test]
    fn del_char_is_backspace() {
        assert_eq!(
            map_key_to_edit(&key(KeyCode::Char('\x7f'), KeyModifiers::NONE)),
            Some(EditAction::Backspace)
        );
    }

    #[test]
    fn bs_char_is_backspace() {
        assert_eq!(
            map_key_to_edit(&key(KeyCode::Char('\x08'), KeyModifiers::NONE)),
            Some(EditAction::Backspace)
        );
    }

    /// Local Linux often reports erase=^H as Ctrl+H, not KeyCode::Backspace.
    #[test]
    fn ctrl_h_is_backspace() {
        assert_eq!(
            map_key_to_edit(&key(KeyCode::Char('h'), KeyModifiers::CONTROL)),
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

    #[test]
    fn key_release_ignored() {
        let mut k = key(KeyCode::Backspace, KeyModifiers::NONE);
        k.kind = KeyEventKind::Release;
        assert_eq!(map_key_to_edit(&k), None);
    }

    #[test]
    fn ctrl_letter_does_not_insert() {
        assert_eq!(
            map_key_to_edit(&key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            None
        );
    }
}

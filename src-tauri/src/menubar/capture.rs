//! Turning handy-keys recording events into one shortcut.
//!
//! Same rules as the settings window's `useShortcutCapture` hook: a keyed
//! combo is committed when its key comes up, a modifier-only combo when every
//! modifier is released, and bare modifiers never overwrite a keyed combo.

use serde::Deserialize;

/// One `handy-keys-event` payload (see `FrontendKeyEvent` in handy_keys.rs).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct KeyEvent {
    #[serde(default)]
    pub modifiers: Vec<String>,
    pub key: Option<String>,
    pub is_key_down: bool,
    #[serde(default)]
    pub hotkey_string: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Still capturing; this is what's held so far.
    Partial(String),
    /// Capture finished with this binding.
    Commit(String),
    /// Escape alone: give up without changing anything.
    Cancel,
    /// Nothing to report.
    Ignore,
}

#[derive(Default)]
pub struct Capture {
    keyed: String,
    modifier_only: String,
    done: bool,
}

impl Capture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, e: &KeyEvent) -> Step {
        if self.done {
            return Step::Ignore;
        }
        let key = e.key.as_deref();
        if e.is_key_down && key == Some("escape") && e.modifiers.is_empty() {
            self.done = true;
            return Step::Cancel;
        }
        if e.is_key_down && !e.hotkey_string.is_empty() {
            if key.is_some() {
                self.keyed = e.hotkey_string.clone();
            } else {
                self.modifier_only = e.hotkey_string.clone();
            }
            return Step::Partial(e.hotkey_string.clone());
        }
        if !e.is_key_down && key.is_some() {
            let binding = if self.keyed.is_empty() {
                e.hotkey_string.clone()
            } else {
                self.keyed.clone()
            };
            if !binding.is_empty() {
                self.done = true;
                return Step::Commit(binding);
            }
        } else if !e.is_key_down
            && key.is_none()
            && e.modifiers.is_empty()
            && self.keyed.is_empty()
            && !self.modifier_only.is_empty()
        {
            self.done = true;
            return Step::Commit(self.modifier_only.clone());
        }
        Step::Ignore
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(mods: &[&str], key: Option<&str>, hotkey: &str) -> KeyEvent {
        KeyEvent {
            modifiers: mods.iter().map(|m| m.to_string()).collect(),
            key: key.map(str::to_string),
            is_key_down: true,
            hotkey_string: hotkey.to_string(),
        }
    }

    fn up(mods: &[&str], key: Option<&str>, hotkey: &str) -> KeyEvent {
        KeyEvent {
            is_key_down: false,
            ..down(mods, key, hotkey)
        }
    }

    #[test]
    fn a_keyed_combo_commits_when_its_key_comes_up() {
        let mut c = Capture::new();
        assert_eq!(
            c.feed(&down(&["option"], None, "option")),
            Step::Partial("option".into())
        );
        assert_eq!(
            c.feed(&down(&["option"], Some("space"), "option+space")),
            Step::Partial("option+space".into())
        );
        assert_eq!(
            c.feed(&up(&["option"], Some("space"), "option")),
            Step::Commit("option+space".into())
        );
        // Releasing the modifier afterwards changes nothing.
        assert_eq!(c.feed(&up(&[], None, "")), Step::Ignore);
    }

    #[test]
    fn a_modifier_only_combo_commits_when_all_are_released() {
        let mut c = Capture::new();
        c.feed(&down(&["command"], None, "command"));
        c.feed(&down(&["command", "shift"], None, "command+shift"));
        assert_eq!(c.feed(&up(&["command"], None, "command")), Step::Ignore);
        assert_eq!(
            c.feed(&up(&[], None, "")),
            Step::Commit("command+shift".into())
        );
    }

    #[test]
    fn bare_modifiers_never_replace_a_keyed_combo() {
        let mut c = Capture::new();
        c.feed(&down(&["ctrl"], Some("d"), "ctrl+d"));
        // Modifier released before the key: still nothing committed.
        assert_eq!(c.feed(&up(&[], None, "")), Step::Ignore);
        assert_eq!(
            c.feed(&up(&[], Some("d"), "d")),
            Step::Commit("ctrl+d".into())
        );
    }

    #[test]
    fn escape_alone_cancels() {
        let mut c = Capture::new();
        assert_eq!(c.feed(&down(&[], Some("escape"), "escape")), Step::Cancel);
        assert_eq!(
            c.feed(&down(&[], Some("a"), "a")),
            Step::Ignore,
            "nothing after cancel"
        );
    }

    #[test]
    fn a_single_function_key_is_allowed() {
        let mut c = Capture::new();
        c.feed(&down(&[], Some("f13"), "f13"));
        assert_eq!(
            c.feed(&up(&[], Some("f13"), "")),
            Step::Commit("f13".into())
        );
    }

    #[test]
    fn parses_the_backend_payload() {
        let json = r#"{"modifiers":["option"],"key":"space","is_key_down":true,"hotkey_string":"option+space"}"#;
        let e: KeyEvent = serde_json::from_str(json).unwrap();
        assert_eq!(e, down(&["option"], Some("space"), "option+space"));
    }
}

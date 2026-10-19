//! Persisted theme preference and the active choice for the current UI thread.

use std::cell::Cell;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
}

thread_local! {
    static ACTIVE_THEME_MODE: Cell<ThemeMode> = const { Cell::new(ThemeMode::Light) };
}

pub fn theme_mode() -> ThemeMode {
    ACTIVE_THEME_MODE.get()
}

pub fn set_theme_mode(theme: ThemeMode) {
    ACTIVE_THEME_MODE.set(theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_theme_is_local_to_the_ui_thread() {
        let original = theme_mode();
        set_theme_mode(ThemeMode::Dark);
        assert_eq!(theme_mode(), ThemeMode::Dark);
        std::thread::spawn(|| {
            assert_eq!(theme_mode(), ThemeMode::Light);
            set_theme_mode(ThemeMode::Light);
        })
        .join()
        .unwrap();
        assert_eq!(theme_mode(), ThemeMode::Dark);
        set_theme_mode(original);
    }
}

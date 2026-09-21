//! The two supported UI languages. Persist the choice in Config; keep the
//! active language local to the UI thread so independent windows/tests do not
//! change one another's text. Technical diagnostics remain language-neutral.

use std::cell::Cell;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "en")]
    English,
}

impl Language {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SimplifiedChinese => "zh-CN",
            Self::English => "en",
        }
    }

    fn from_windows_ui_language(language_id: u16) -> Self {
        // All Chinese regional variants use our Simplified Chinese interface.
        // Other system languages fall back to the supported English interface.
        if language_id & 0x03ff == 0x0004 {
            Self::SimplifiedChinese
        } else {
            Self::English
        }
    }
}

impl Default for Language {
    fn default() -> Self {
        #[cfg(windows)]
        {
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetUserDefaultUILanguage() -> u16;
            }
            Self::from_windows_ui_language(unsafe { GetUserDefaultUILanguage() })
        }
        #[cfg(not(windows))]
        {
            Self::from_windows_ui_language(0x0409)
        }
    }
}

thread_local! {
    static ACTIVE_LANGUAGE: Cell<Language> = Cell::new(Language::default());
}

pub fn language() -> Language {
    ACTIVE_LANGUAGE.get()
}

pub fn set_language(language: Language) {
    ACTIVE_LANGUAGE.set(language);
}

/// Keeping each pair together makes missing translations a compile-time error.
pub fn tr(chinese: &'static str, english: &'static str) -> &'static str {
    match language() {
        Language::SimplifiedChinese => chinese,
        Language::English => english,
    }
}

/// Format only the selected translation. Both format strings are compiler
/// checked, including positional, named and implicitly captured arguments.
#[macro_export]
macro_rules! tr_format {
    ($chinese:literal, $english:literal $(, $($args:tt)*)?) => {
        match $crate::app::i18n::language() {
            $crate::app::i18n::Language::SimplifiedChinese => format!($chinese $(, $($args)*)?),
            $crate::app::i18n::Language::English => format!($english $(, $($args)*)?),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_language_maps_to_one_of_the_two_supported_interfaces() {
        for id in [0x0804, 0x0404, 0x0c04, 0x1004] {
            assert_eq!(
                Language::from_windows_ui_language(id),
                Language::SimplifiedChinese
            );
        }
        for id in [0x0409, 0x0809, 0x040c, 0x0411, 0] {
            assert_eq!(Language::from_windows_ui_language(id), Language::English);
        }
    }

    #[test]
    fn translations_follow_the_thread_choice_and_support_formatted_values() {
        let original = language();
        let value = 42;
        set_language(Language::English);
        assert_eq!(tr("复制", "Copy"), "Copy");
        assert_eq!(crate::tr_format!("值 {value}", "Value {value}"), "Value 42");
        assert_eq!(
            crate::tr_format!("{name}：{}", "{name}: {}", value, name = "HEX"),
            "HEX: 42"
        );
        std::thread::spawn(|| {
            set_language(Language::SimplifiedChinese);
            assert_eq!(tr("复制", "Copy"), "复制");
        })
        .join()
        .unwrap();
        assert_eq!(language(), Language::English);
        set_language(original);
    }
}

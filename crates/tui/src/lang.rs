//! The interface's language: English unless `[ui] language = "ru"`. Every text is
//! written once in both, side by side, with [`tr!`] (a fixed text) or [`trf!`] (one
//! with `{values}` in it), so the two cannot drift apart.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

/// Tests read the interface in Russian, as it was written first; one that wants
/// English says so for its own thread.
static RUSSIAN: AtomicBool = AtomicBool::new(cfg!(test));

thread_local! {
    static THIS_THREAD: Cell<Option<bool>> = const { Cell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "ru")]
    Russian,
}

pub fn set(language: Language) {
    // Tests run side by side: one that switches the language switches its own.
    if cfg!(test) {
        THIS_THREAD.with(|t| t.set(Some(language == Language::Russian)));
    } else {
        RUSSIAN.store(language == Language::Russian, Ordering::Relaxed);
    }
}

pub fn current() -> Language {
    if russian() {
        Language::Russian
    } else {
        Language::English
    }
}

pub fn russian() -> bool {
    THIS_THREAD
        .with(Cell::get)
        .unwrap_or_else(|| RUSSIAN.load(Ordering::Relaxed))
}

/// For a test: this thread reads `language`, whatever the others do.
#[cfg(test)]
pub fn for_this_thread(language: Language) {
    set(language);
}

/// A fixed text: `tr!("English", "Русский")`.
macro_rules! tr {
    ($en:literal, $ru:literal $(,)?) => {
        if $crate::lang::russian() { $ru } else { $en }
    };
}

/// A text with values in it, as `format!` takes them: `trf!("{n} new", "{n} новых")`.
macro_rules! trf {
    ($en:literal, $ru:literal $(, $($arg:tt)*)?) => {
        if $crate::lang::russian() {
            format!($ru $(, $($arg)*)?)
        } else {
            format!($en $(, $($arg)*)?)
        }
    };
}

/// Why a connection broke, as the XMPP side says it (in English), in the interface's
/// language: the reasons omtalk itself gives are known; others are the libraries'.
pub fn reason(text: &str) -> String {
    if !russian() {
        return text.to_owned();
    }
    match text {
        "the server does not answer" => "сервер не отвечает".into(),
        "the connection task ended" => "соединение завершилось".into(),
        other => other.to_owned(),
    }
}

/// Russian plurals: the form for `n` of "одно сообщение / два сообщения / пять сообщений".
pub fn ru_plural<'a>(n: usize, one: &'a str, few: &'a str, many: &'a str) -> &'a str {
    match (n % 10, n % 100) {
        (1, m) if m != 11 => one,
        (2..=4, m) if !(12..=14).contains(&m) => few,
        _ => many,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_text_is_said_in_the_language_chosen() {
        let n = 3;
        for_this_thread(Language::English);
        assert_eq!(tr!("chats", "чаты"), "chats");
        assert_eq!(trf!("{n} new", "{n} новых"), "3 new");
        for_this_thread(Language::Russian);
        assert_eq!(trf!("{} new", "{} новых", n + 1), "4 новых");
        assert_eq!(ru_plural(21, "сообщение", "сообщения", "сообщений"), "сообщение");
        assert_eq!(ru_plural(12, "сообщение", "сообщения", "сообщений"), "сообщений");
        assert_eq!(ru_plural(3, "сообщение", "сообщения", "сообщений"), "сообщения");
    }
}

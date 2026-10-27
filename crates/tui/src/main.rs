//! omtalk: a terminal XMPP client with OMEMO.
//!
//!     omtalk                 accounts from ~/.config/omtalk/config.toml
//!     omtalk me@example.org  one account, without a config file

#[macro_use]
mod lang;
mod app;
mod audio;
mod audio_ui;
mod avatar;
mod clipboard;
mod complete;
mod config;
mod emoji;
mod keys;
mod links;
mod mpris;
mod notify;
mod read_sync;
mod room_admin;
mod runtime;
mod services;
mod settings;
mod styling;
mod terminal_backend;
mod theme;
mod view;

use std::io;

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

fn main() {
    let options = match runtime::Options::from_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    crate::lang::set(options.config.ui.language);
    // One omtalk per account and database: said before anything else, the screen still
    // the shell's.
    let _accounts = match runtime::lock_accounts(&options) {
        Ok(lock) => lock,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    // A mistake in the colours is said before the screen is taken over, as one in the
    // accounts is.
    match options.config.theme.palette() {
        Ok(palette) => theme::set(palette),
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    }
    // Passwords are asked for before the screen is taken over.
    let accounts = match runtime::prepare_accounts(&options) {
        Ok(accounts) => accounts,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    // Whatever happens, give the terminal back as it was.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
        // A task that panicked would leave the interface running on a terminal given
        // back: leave, with the message above.
        std::process::exit(101);
    }));

    if let Err(e) = enable_raw_mode().and_then(|_| {
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            // Whether the window is in front: read markers wait, a bell rings.
            EnableFocusChange
        )
    }) {
        eprintln!("cannot take over the terminal: {e}");
        std::process::exit(1);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let result = runtime.block_on(runtime::run(options, accounts));
    // Give the terminal back before runtime teardown can wait on background work.
    restore_terminal();
    runtime.shutdown_timeout(std::time::Duration::from_millis(200));
    if let Err(message) = result {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn restore_terminal() {
    // Stop mouse reports while raw mode still consumes them as terminal input.
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

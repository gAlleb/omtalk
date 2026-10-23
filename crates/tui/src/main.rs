//! omtalk: a terminal XMPP client with OMEMO.
//!
//!     omtalk                 accounts from ~/.config/omtalk/config.toml
//!     omtalk me@example.org  one account, without a config file

#[macro_use]
mod lang;
mod app;
mod avatar;
mod clipboard;
mod complete;
mod config;
mod keys;
mod notify;
mod runtime;
mod services;
mod settings;
mod styling;
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
    // One omtalk per database: said before anything else, the screen still the shell's.
    let _database = match runtime::lock_database(&options) {
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
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(runtime::run(options, accounts));
    restore_terminal();
    if let Err(message) = result {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
}

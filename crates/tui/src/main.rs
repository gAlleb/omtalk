//! omtalk: a terminal XMPP client with OMEMO.
//!
//!     omtalk                 accounts from ~/.config/omtalk/config.toml
//!     omtalk me@example.org  one account, without a config file

mod app;
mod complete;
mod config;
mod runtime;
mod services;
mod theme;
mod view;

use std::io;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
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
    }));

    if let Err(e) =
        enable_raw_mode().and_then(|_| execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture))
    {
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
    let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
}

//! herdr-ssh-manager — save SSH hosts and reconnect to them from Herdr.

mod cli;
mod config;
mod herdr;
mod import;
mod model;
mod picker;
mod ssh;
mod store;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    if let Err(err) = run() {
        eprintln!("herdr-ssh-manager: {err:#}");
        // Popups vanish the moment the process exits, taking the message with them.
        // Hold the frame so the user can actually read what went wrong.
        if is_popup() {
            eprintln!("\nPress Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Add(args) => cli::add(args),
        Command::List(args) => cli::list(args),
        Command::Edit(args) => cli::edit(args),
        Command::Remove(args) => cli::remove(args),
        Command::Import(args) => cli::import(args),
        Command::Pick => picker::run(),
        Command::Connect(args) => cli::connect(args),
        Command::Where => cli::where_cmd(),
        Command::OpenPicker => cli::open_pane("picker"),
        Command::OpenAdd => cli::open_pane("add"),
    }
}

/// True when Herdr launched us as one of the declared popup panes.
fn is_popup() -> bool {
    std::env::var("HERDR_PLUGIN_ENTRYPOINT_ID").is_ok_and(|v| !v.is_empty())
}

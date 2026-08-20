//! herdr-ssh-manager — save SSH hosts and reconnect to them from Herdr.

mod cli;
mod config;
mod herdr;
mod import;
mod model;
mod picker;
mod setup;
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
        Command::Setup(args) => setup::run(args),
        Command::OpenPicker => cli::open_pane("picker"),
        Command::OpenAdd => cli::open_pane("add"),
    }
}

/// True when Herdr launched us as one of the declared popup panes.
fn is_popup() -> bool {
    std::env::var("HERDR_PLUGIN_ENTRYPOINT_ID").is_ok_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    /// The plugin manifest carries its own `version`, and Herdr shows *that* one while the
    /// binary reports Cargo's. Bumping one and forgetting the other ships a release whose
    /// advertised version does not match what it installs, which nothing else would catch.
    /// Herdr reports the plugin root as a `\\?\` verbatim path, which the Windows launchers
    /// have to strip. Getting that expression subtly wrong is invisible: `StartsWith('\?')`
    /// is valid PowerShell and parses cleanly, so neither CI's syntax check nor anything
    /// running on Linux would notice. It simply stops stripping — and only on the fallback
    /// path, so it survives a successful install. This caught exactly that in 0.7.0.
    #[test]
    fn every_windows_launcher_strips_the_verbatim_prefix_the_same_way() {
        const STRIP: &str = r#"if($p.StartsWith('\\?\')){$p=$p.Substring(4)}"#;
        let manifest: toml::Value = toml::from_str(include_str!("../herdr-plugin.toml"))
            .expect("herdr-plugin.toml does not parse");

        let mut checked = 0;
        for section in ["panes", "actions"] {
            for item in manifest[section].as_array().expect("array of tables") {
                let windows = item
                    .get("platforms")
                    .and_then(|p| p.as_array())
                    .is_some_and(|p| p.iter().any(|v| v.as_str() == Some("windows")));
                let command = item["command"].as_array().expect("command array");
                let last = command.last().and_then(|v| v.as_str()).unwrap_or_default();
                // Only the launchers that resolve the plugin root have to strip it.
                if !windows || !last.contains("HERDR_PLUGIN_ROOT") {
                    continue;
                }
                let id = item["id"].as_str().unwrap_or("?");
                assert!(
                    last.contains(STRIP),
                    "{id} does not strip the verbatim prefix the same way as the others"
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 4,
            "expected several Windows launchers, saw {checked}"
        );
    }

    #[test]
    fn cargo_and_plugin_manifest_versions_agree() {
        let manifest = include_str!("../herdr-plugin.toml");
        let declared = manifest
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("version = "))
            .map(|v| v.trim().trim_matches('"'))
            .expect("herdr-plugin.toml declares no top-level version");
        assert_eq!(
            declared,
            env!("CARGO_PKG_VERSION"),
            "herdr-plugin.toml says {declared}, Cargo.toml says {}",
            env!("CARGO_PKG_VERSION")
        );
    }
}

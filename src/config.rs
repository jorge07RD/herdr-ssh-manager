//! Where the plugin keeps its files.
//!
//! Under Herdr this is `$HERDR_PLUGIN_CONFIG_DIR`, which Herdr guarantees is outside the
//! managed plugin checkout. Run from a plain shell (`cargo run`, or the CLI subcommands
//! used standalone) there is no such variable, so we fall back to the platform's usual
//! per-user config location. `$HERDR_PLUGIN_ROOT` is deliberately never used: it is a
//! managed checkout that `herdr plugin install` overwrites on update.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

pub const CONNECTIONS_FILE: &str = "connections.toml";

/// Resolve the directory holding `connections.toml`, creating it if needed.
pub fn config_dir() -> Result<PathBuf> {
    let dir = config_dir_path()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("could not create config directory {}", dir.display()))?;
    Ok(dir)
}

fn config_dir_path() -> Result<PathBuf> {
    if let Some(dir) = non_empty_env("HERDR_PLUGIN_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    fallback_config_dir()
}

#[cfg(windows)]
fn fallback_config_dir() -> Result<PathBuf> {
    let base = non_empty_env("APPDATA")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join("AppData").join("Roaming")))
        .ok_or_else(|| anyhow!("cannot locate a config directory: set APPDATA"))?;
    Ok(base.join("herdr-ssh-manager"))
}

#[cfg(not(windows))]
fn fallback_config_dir() -> Result<PathBuf> {
    let base = non_empty_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|h| h.join(".config")))
        .ok_or_else(|| anyhow!("cannot locate a config directory: set HOME or XDG_CONFIG_HOME"))?;
    Ok(base.join("herdr-ssh-manager"))
}

pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    non_empty_env(key).map(PathBuf::from)
}

/// Expand a leading `~` against the user's home directory.
pub fn expand_tilde(path: &str) -> String {
    let Some(rest) = path.strip_prefix('~') else {
        return path.to_string();
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        // `~user/...` — not ours to expand; leave it for ssh.
        return path.to_string();
    }
    match home_dir() {
        Some(home) => format!("{}{}", home.display(), rest),
        None => path.to_string(),
    }
}

pub fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_leaves_other_paths_alone() {
        assert_eq!(expand_tilde("/etc/ssh/key"), "/etc/ssh/key");
        assert_eq!(expand_tilde("relative/key"), "relative/key");
        // `~otheruser` is ssh's business, not ours.
        assert_eq!(expand_tilde("~root/.ssh/id"), "~root/.ssh/id");
    }

    #[test]
    fn expand_tilde_uses_home_when_set() {
        if let Some(home) = home_dir() {
            let expanded = expand_tilde("~/.ssh/id_ed25519");
            assert_eq!(expanded, format!("{}/.ssh/id_ed25519", home.display()));
            assert!(!expanded.starts_with('~'));
        }
    }
}

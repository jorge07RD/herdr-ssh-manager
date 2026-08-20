//! `setup` — put the picker on a key without hand-editing Herdr's config.
//!
//! Doing this by hand means getting four separate things right, and each one fails the
//! same silent way: you press the key and nothing happens.
//!
//!  - Herdr's `config.toml` may not exist at all yet, so there is nothing to edit.
//!  - Where it lives differs per platform.
//!  - Windows needs the `-windows` action id. Herdr rejects duplicate action ids even
//!    when they are platform-gated, so the two platforms cannot share one — and binding
//!    the wrong one answers `platform_unsupported` at the keyboard, which looks like
//!    nothing at all.
//!  - On Windows the obvious ways to write the file (`Set-Content`, `-Encoding utf8`)
//!    produce UTF-16 or a BOM, either of which the TOML parser can choke on.
//!
//! This appends to the config rather than rewriting it, so hand-written comments and
//! keybindings survive, and it backs the file up first.

use anyhow::{bail, Context, Result};
use clap::Args;
use std::path::PathBuf;

pub const DEFAULT_KEY: &str = "prefix+shift+s";

/// The action id that works on *this* platform. See the module docs for why they differ.
pub fn action_id() -> &'static str {
    if cfg!(windows) {
        "herdr-ssh-manager.open-picker-windows"
    } else {
        "herdr-ssh-manager.open-picker"
    }
}

#[derive(Args, Debug, Default)]
pub struct SetupArgs {
    /// Key combination to bind, e.g. `prefix+shift+s`
    #[arg(long, default_value = DEFAULT_KEY)]
    pub key: String,
    /// Show what would change without writing anything
    #[arg(long)]
    pub dry_run: bool,
    /// Take the binding back out again
    ///
    /// Herdr has no uninstall hook, so a plugin cannot clean up after itself — run this
    /// before `herdr plugin uninstall`, while the binary still exists.
    #[arg(long)]
    pub remove: bool,
}

/// Herdr's own config file — not this plugin's. Deliberately separate from
/// [`crate::config`], which resolves where *our* connections live.
pub fn herdr_config_path() -> Result<PathBuf> {
    let dir = herdr_config_dir()?;
    Ok(dir.join("config.toml"))
}

#[cfg(windows)]
fn herdr_config_dir() -> Result<PathBuf> {
    let base = crate::config::non_empty_env("APPDATA")
        .map(PathBuf::from)
        .or_else(|| crate::config::home_dir().map(|h| h.join("AppData").join("Roaming")))
        .context("cannot locate Herdr's config directory: set APPDATA")?;
    Ok(base.join("herdr"))
}

#[cfg(not(windows))]
fn herdr_config_dir() -> Result<PathBuf> {
    let base = crate::config::non_empty_env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| crate::config::home_dir().map(|h| h.join(".config")))
        .context("cannot locate Herdr's config directory: set HOME or XDG_CONFIG_HOME")?;
    Ok(base.join("herdr"))
}

/// What a `[[keys.command]]` entry in Herdr's config says.
#[derive(Debug, PartialEq)]
pub struct Binding {
    pub key: String,
    pub command: String,
}

/// Read the `[[keys.command]]` entries out of a Herdr config.
///
/// Parsed rather than pattern-matched, so a binding written across odd whitespace or with
/// its fields in another order is still recognised — but the file is only ever *appended*
/// to, which is what keeps comments and layout intact.
pub fn bindings(config: &str) -> Result<Vec<Binding>> {
    let doc: toml::Value = toml::from_str(config).context(
        "Herdr's config.toml is not valid TOML. Fix it first — refusing to append to a \
         file Herdr itself cannot read",
    )?;
    let Some(entries) = doc
        .get("keys")
        .and_then(|k| k.get("command"))
        .and_then(|c| c.as_array())
    else {
        return Ok(Vec::new());
    };
    Ok(entries
        .iter()
        .filter_map(|e| {
            Some(Binding {
                key: e.get("key")?.as_str()?.to_string(),
                command: e.get("command")?.as_str()?.to_string(),
            })
        })
        .collect())
}

/// The block appended to the config.
pub fn block(key: &str) -> String {
    format!(
        "[[keys.command]]\nkey = {key:?}\ntype = \"plugin_action\"\ncommand = {:?}\n\
         description = \"SSH connections\"\n",
        action_id()
    )
}

/// What `setup` decided to do, so the caller can report it without re-deriving it.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// Already bound correctly; nothing to write.
    AlreadyDone { key: String },
    /// Bound to this plugin, but to an action id that does not exist on this platform.
    WrongPlatformId { key: String, command: String },
    /// The requested key is taken by something else.
    KeyTaken { key: String, command: String },
    /// Append the block.
    Append,
}

pub fn plan(existing: &[Binding], key: &str) -> Plan {
    for b in existing {
        if b.command == action_id() {
            return Plan::AlreadyDone { key: b.key.clone() };
        }
        if b.command.starts_with("herdr-ssh-manager.open-picker") {
            return Plan::WrongPlatformId {
                key: b.key.clone(),
                command: b.command.clone(),
            };
        }
    }
    if let Some(b) = existing.iter().find(|b| b.key.eq_ignore_ascii_case(key)) {
        return Plan::KeyTaken {
            key: b.key.clone(),
            command: b.command.clone(),
        };
    }
    Plan::Append
}

/// Drop every `[[keys.command]]` block bound to this plugin, leaving the rest byte for byte.
///
/// Line-based on purpose: round-tripping through a TOML serialiser would reformat the whole
/// file and throw away the user's comments.
pub fn without_our_bindings(config: &str) -> (String, usize) {
    let ends_with_newline = config.ends_with('\n');
    let mut kept: Vec<&str> = Vec::new();
    let mut removed = 0;
    let lines: Vec<&str> = config.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() != "[[keys.command]]" {
            kept.push(lines[i]);
            i += 1;
            continue;
        }
        // Take the whole block: everything up to the next table header.
        let start = i;
        let mut end = i + 1;
        while end < lines.len() && !lines[end].trim_start().starts_with('[') {
            end += 1;
        }
        // Blank lines after the block separate it from what follows; they belong to the
        // file, not to us. Leaving them in place is what keeps exactly one blank line
        // between the neighbours once the block is gone.
        while end > start + 1 && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        let block = &lines[start..end];
        if block
            .iter()
            .any(|l| l.contains("\"herdr-ssh-manager.") || l.contains("'herdr-ssh-manager."))
        {
            removed += 1;
            // Comments glued to the block describe the block. Leaving them behind strands
            // them above whatever table comes next, where they now appear to describe
            // *that* — worse than removing them. A comment separated by a blank line is
            // somebody else's and stays put.
            while kept.last().is_some_and(|l| l.trim_start().starts_with('#')) {
                kept.pop();
            }
            // Then the blank line that separated the stanza, so repeated add/remove cycles
            // cannot pile up whitespace.
            while kept.last().is_some_and(|l| l.trim().is_empty()) {
                kept.pop();
            }
        } else {
            kept.extend_from_slice(block);
        }
        i = end;
    }
    let mut out = kept.join("\n");
    if ends_with_newline && !out.is_empty() {
        out.push('\n');
    }
    (out, removed)
}

fn remove(args: &SetupArgs, path: &std::path::Path, existing: &str) -> Result<()> {
    // Parse first: refuse to rewrite a file Herdr itself cannot read.
    bindings(existing)?;
    let (updated, removed) = without_our_bindings(existing);
    if removed == 0 {
        println!("No SSH Manager keybinding in {}.", path.display());
        return Ok(());
    }
    if args.dry_run {
        println!("Would remove {removed} binding(s) from {}.", path.display());
        return Ok(());
    }
    let backup = path.with_extension("toml.bak");
    std::fs::write(&backup, existing)
        .with_context(|| format!("could not back up to {}", backup.display()))?;
    std::fs::write(path, updated.as_bytes())
        .with_context(|| format!("could not write {}", path.display()))?;
    println!(
        "Removed {removed} binding(s) from {} (backup at {}).",
        path.display(),
        backup.display()
    );
    let _ = crate::herdr::reload_config();
    Ok(())
}

pub fn run(args: SetupArgs) -> Result<()> {
    let path = herdr_config_path()?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("could not read {}", path.display()));
        }
    };

    if args.remove {
        return remove(&args, &path, &existing);
    }

    match plan(&bindings(&existing)?, &args.key) {
        Plan::AlreadyDone { key } => {
            println!(
                "Already bound: {key} opens the SSH picker ({}).",
                path.display()
            );
            return Ok(());
        }
        Plan::WrongPlatformId { key, command } => {
            bail!(
                "{} binds {key} to `{command}`, which does not exist on this platform — Herdr \
                 answers `platform_unsupported` and the key appears dead. Change that line to \
                 `command = \"{}\"`.",
                path.display(),
                action_id()
            );
        }
        Plan::KeyTaken { key, command } => {
            bail!(
                "{key} is already bound to `{command}` in {}. Pick another with \
                 `--key`, e.g. `--key prefix+shift+h`.",
                path.display()
            );
        }
        Plan::Append => {}
    }

    let addition = block(&args.key);
    if args.dry_run {
        println!("Would append to {}:\n{addition}", path.display());
        return Ok(());
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    // Never be the reason someone loses a hand-written Herdr config.
    if !existing.is_empty() {
        let backup = path.with_extension("toml.bak");
        std::fs::write(&backup, &existing)
            .with_context(|| format!("could not back up to {}", backup.display()))?;
        println!("Backed up your previous config to {}", backup.display());
    }

    let mut updated = existing;
    if !updated.is_empty() {
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        // A blank line so the block reads as its own stanza, but not at the top of a file
        // we just created.
        updated.push('\n');
    }
    updated.push_str(&addition);
    // Rust writes the bytes it is given: UTF-8, no BOM, no CRLF rewriting.
    std::fs::write(&path, updated.as_bytes())
        .with_context(|| format!("could not write {}", path.display()))?;

    println!("Bound {} to the SSH picker in {}", args.key, path.display());
    match crate::herdr::reload_config() {
        Ok(()) => println!("Herdr reloaded its config — press {} to open it.", args.key),
        Err(_) => println!(
            "Now run `herdr server reload-config` (or restart Herdr) and press {}.",
            args.key
        ),
    }
    let _ = crate::herdr::notify(
        "SSH Manager",
        &format!("{} now opens the connection picker.", args.key),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_without_keybindings_yields_none() {
        assert!(bindings("").unwrap().is_empty());
        assert!(bindings("[ui]\ntheme = \"dark\"\n").unwrap().is_empty());
    }

    #[test]
    fn bindings_are_read_whatever_the_field_order() {
        let cfg = "[[keys.command]]\ncommand = \"other.thing\"\nkey = \"prefix+z\"\n";
        assert_eq!(
            bindings(cfg).unwrap(),
            vec![Binding {
                key: "prefix+z".into(),
                command: "other.thing".into()
            }]
        );
    }

    #[test]
    fn broken_toml_is_refused_rather_than_appended_to() {
        let err = bindings("[[keys.command]\nkey =").unwrap_err().to_string();
        assert!(err.contains("not valid TOML"), "{err}");
    }

    #[test]
    fn running_twice_changes_nothing_the_second_time() {
        let cfg = block("prefix+shift+s");
        let plan = plan(&bindings(&cfg).unwrap(), "prefix+shift+s");
        assert_eq!(
            plan,
            Plan::AlreadyDone {
                key: "prefix+shift+s".into()
            }
        );
    }

    #[test]
    fn a_binding_for_the_other_platform_is_named_as_the_problem() {
        // Exactly the trap: the id from the other platform is accepted by the TOML parser
        // and by `config check`, and fails only at the keyboard, silently.
        let other = if cfg!(windows) {
            "herdr-ssh-manager.open-picker"
        } else {
            "herdr-ssh-manager.open-picker-windows"
        };
        let cfg = format!(
            "[[keys.command]]\nkey = \"prefix+shift+s\"\ntype = \"plugin_action\"\ncommand = \"{other}\"\n"
        );
        assert_eq!(
            plan(&bindings(&cfg).unwrap(), "prefix+shift+s"),
            Plan::WrongPlatformId {
                key: "prefix+shift+s".into(),
                command: other.into()
            }
        );
    }

    #[test]
    fn someone_elses_binding_on_that_key_is_not_stolen() {
        let cfg = "[[keys.command]]\nkey = \"prefix+shift+s\"\ncommand = \"other.thing\"\n";
        assert_eq!(
            plan(&bindings(cfg).unwrap(), "prefix+shift+s"),
            Plan::KeyTaken {
                key: "prefix+shift+s".into(),
                command: "other.thing".into()
            }
        );
    }

    #[test]
    fn removing_takes_out_our_block_and_nothing_else() {
        let cfg = format!(
            "# keep this comment\n[ui]\ntheme = \"dark\"\n\n\
             [[keys.command]]\nkey = \"prefix+g\"\ncommand = \"other.thing\"\n\n{}",
            block("prefix+shift+s")
        );
        let (out, removed) = without_our_bindings(&cfg);
        assert_eq!(removed, 1);
        assert!(out.contains("# keep this comment"), "{out}");
        assert!(out.contains("other.thing"), "{out}");
        assert!(!out.contains("herdr-ssh-manager"), "{out}");
        // Still valid TOML, and the untouched binding survives intact.
        assert_eq!(bindings(&out).unwrap().len(), 1);
    }

    #[test]
    fn add_then_remove_returns_the_file_to_where_it_started() {
        let original = "[ui]\ntheme = \"dark\"\n";
        let mut with = original.to_string();
        with.push('\n');
        with.push_str(&block("prefix+shift+s"));
        let (back, removed) = without_our_bindings(&with);
        assert_eq!(removed, 1);
        assert_eq!(back, original);
    }

    #[test]
    fn comments_describing_the_block_leave_with_it() {
        // Found the hard way: left behind, these end up above the next table and read as
        // if they described that instead.
        let cfg = format!(
            "onboarding = false\n\n# Opens the picker.\n# prefix+s is taken.\n{}\n[ui]\nx = 1\n",
            block("prefix+shift+s")
        );
        let (out, removed) = without_our_bindings(&cfg);
        assert_eq!(removed, 1);
        assert_eq!(out, "onboarding = false\n\n[ui]\nx = 1\n", "{out}");
    }

    #[test]
    fn a_comment_separated_by_a_blank_line_belongs_to_someone_else() {
        let cfg = format!("# unrelated note\n\n{}", block("prefix+shift+s"));
        let (out, _) = without_our_bindings(&cfg);
        assert_eq!(out, "# unrelated note\n", "{out}");
    }

    #[test]
    fn removing_from_a_config_without_our_binding_is_a_no_op() {
        let cfg = "[[keys.command]]\nkey = \"prefix+g\"\ncommand = \"other.thing\"\n";
        let (out, removed) = without_our_bindings(cfg);
        assert_eq!(removed, 0);
        assert_eq!(out, cfg);
    }

    #[test]
    fn the_appended_block_parses_and_targets_this_platform() {
        let parsed = bindings(&block("prefix+shift+s")).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].command, action_id());
    }
}

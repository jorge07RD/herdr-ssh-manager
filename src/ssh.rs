//! Building the `ssh` command line and handing the process over to it.

use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use std::process::Command;

use crate::config;
use crate::model::Connection;

/// Build the argv passed to `ssh`, excluding argv[0].
///
/// Order is `[-p] [-i] [-J] [extra…] destination`, with the destination last so a
/// stray user-supplied flag in `extra_ssh_args` cannot swallow it.
pub fn build_args(conn: &Connection) -> Result<Vec<String>> {
    validate(conn)?;

    let mut args = Vec::new();
    if conn.port != 22 {
        args.push("-p".to_string());
        args.push(conn.port.to_string());
    }
    if let Some(identity) = non_blank(&conn.identity_file) {
        args.push("-i".to_string());
        args.push(config::expand_tilde(identity));
    }
    if let Some(jump) = non_blank(&conn.jump_host) {
        args.push("-J".to_string());
        args.push(jump.to_string());
    }
    for extra in &conn.extra_ssh_args {
        if !extra.trim().is_empty() {
            args.push(extra.clone());
        }
    }

    let host = conn.host.trim();
    args.push(match non_blank(&conn.user) {
        Some(user) => format!("{user}@{host}"),
        None => host.to_string(),
    });
    Ok(args)
}

/// Reject entries that would produce a nonsensical or unsafe command line.
fn validate(conn: &Connection) -> Result<()> {
    let host = conn.host.trim();
    if host.is_empty() {
        bail!(
            "connection `{}` has no host; fix it with `herdr-ssh-manager edit {}`",
            conn.name,
            conn.id
        );
    }
    // A host starting with `-` would be read by ssh as a flag.
    if host.starts_with('-') {
        bail!(
            "connection `{}` has an invalid host {host:?}: it must not start with `-`",
            conn.name
        );
    }
    if host.contains(char::is_whitespace) {
        bail!(
            "connection `{}` has an invalid host {host:?}: it must not contain whitespace",
            conn.name
        );
    }
    if let Some(user) = non_blank(&conn.user) {
        if user.contains('@') || user.contains(char::is_whitespace) {
            bail!("connection `{}` has an invalid user {user:?}", conn.name);
        }
    }
    Ok(())
}

fn non_blank(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

/// A human-readable rendering of the command, for `--dry-run` and error messages.
pub fn command_line(conn: &Connection) -> Result<String> {
    let args = build_args(conn)?;
    let mut out = String::from("ssh");
    for arg in args {
        out.push(' ');
        if arg.contains(char::is_whitespace) {
            out.push_str(&format!("{arg:?}"));
        } else {
            out.push_str(&arg);
        }
    }
    Ok(out)
}

/// Locate the `ssh` binary on PATH so a missing client fails with a clear message
/// rather than a bare "No such file or directory" from exec.
pub fn find_ssh() -> Result<PathBuf> {
    let exe = if cfg!(windows) { "ssh.exe" } else { "ssh" };
    let path = std::env::var_os("PATH")
        .ok_or_else(|| anyhow!("PATH is not set, so `ssh` cannot be located"))?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(exe);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    bail!(
        "`{exe}` was not found on PATH. Install an OpenSSH client \
         (Debian/Ubuntu: `sudo apt install openssh-client`; macOS: bundled; \
         Windows: Settings > Optional features > OpenSSH Client)."
    )
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Replace this process with the ssh session — on Unix literally, via `execvp`, so the
/// popup pane *becomes* the connection and closes when the session ends.
///
/// The terminal must already be back in its normal mode before calling this: on Unix
/// nothing after the `exec` runs, so there is no chance to clean up afterwards.
pub fn connect(conn: &Connection) -> Result<std::convert::Infallible> {
    let program = find_ssh()?;
    let args = build_args(conn)?;
    exec_replacing(&program, &args)
}

#[cfg(unix)]
fn exec_replacing(program: &std::path::Path, args: &[String]) -> Result<std::convert::Infallible> {
    use std::os::unix::process::CommandExt;
    // `exec` only returns on failure.
    let err = Command::new(program).args(args).exec();
    Err(anyhow!("could not start {}: {err}", program.display()))
}

#[cfg(not(unix))]
fn exec_replacing(program: &std::path::Path, args: &[String]) -> Result<std::convert::Infallible> {
    // Windows has no execvp: run ssh as a child, then exit with its status so the
    // caller still sees the session's exit code.
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| anyhow!("could not start {}: {e}", program.display()))?;
    std::process::exit(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        Connection::new("web", "example.com")
    }

    #[test]
    fn plain_host_needs_no_flags() {
        assert_eq!(build_args(&conn()).unwrap(), vec!["example.com"]);
    }

    #[test]
    fn user_is_folded_into_the_destination() {
        let mut c = conn();
        c.user = Some("deploy".into());
        assert_eq!(build_args(&c).unwrap(), vec!["deploy@example.com"]);
    }

    #[test]
    fn default_port_is_omitted_but_others_are_passed() {
        let mut c = conn();
        c.port = 22;
        assert_eq!(build_args(&c).unwrap(), vec!["example.com"]);
        c.port = 2222;
        assert_eq!(build_args(&c).unwrap(), vec!["-p", "2222", "example.com"]);
    }

    #[test]
    fn identity_jump_and_extras_appear_in_order_before_the_destination() {
        let mut c = conn();
        c.user = Some("deploy".into());
        c.port = 2222;
        c.identity_file = Some("/keys/id_ed25519".into());
        c.jump_host = Some("bastion.example.com".into());
        c.extra_ssh_args = vec!["-o".into(), "ServerAliveInterval=30".into()];
        assert_eq!(
            build_args(&c).unwrap(),
            vec![
                "-p",
                "2222",
                "-i",
                "/keys/id_ed25519",
                "-J",
                "bastion.example.com",
                "-o",
                "ServerAliveInterval=30",
                "deploy@example.com",
            ]
        );
    }

    #[test]
    fn identity_file_tilde_is_expanded() {
        let mut c = conn();
        c.identity_file = Some("~/.ssh/id_ed25519".into());
        let args = build_args(&c).unwrap();
        let identity = &args[args.iter().position(|a| a == "-i").unwrap() + 1];
        assert!(!identity.starts_with('~'), "tilde survived in {identity}");
        assert!(identity.ends_with("/.ssh/id_ed25519"));
    }

    #[test]
    fn blank_optional_fields_are_treated_as_absent() {
        let mut c = conn();
        c.user = Some("  ".into());
        c.identity_file = Some(String::new());
        c.jump_host = Some("   ".into());
        c.extra_ssh_args = vec![String::new(), "  ".into()];
        assert_eq!(build_args(&c).unwrap(), vec!["example.com"]);
    }

    #[test]
    fn a_host_that_looks_like_a_flag_is_rejected() {
        let mut c = conn();
        c.host = "-oProxyCommand=touch /tmp/pwned".into();
        let err = build_args(&c).unwrap_err().to_string();
        assert!(err.contains("must not start with `-`"), "got: {err}");
    }

    #[test]
    fn empty_or_whitespace_hosts_are_rejected() {
        let mut c = conn();
        c.host = "   ".into();
        assert!(build_args(&c).unwrap_err().to_string().contains("no host"));

        c.host = "a b".into();
        assert!(build_args(&c)
            .unwrap_err()
            .to_string()
            .contains("must not contain whitespace"));
    }

    #[test]
    fn a_user_containing_an_at_sign_is_rejected() {
        let mut c = conn();
        c.user = Some("deploy@evil".into());
        assert!(build_args(&c)
            .unwrap_err()
            .to_string()
            .contains("invalid user"));
    }

    #[test]
    fn command_line_is_readable() {
        let mut c = conn();
        c.port = 2222;
        c.user = Some("deploy".into());
        assert_eq!(command_line(&c).unwrap(), "ssh -p 2222 deploy@example.com");
    }
}

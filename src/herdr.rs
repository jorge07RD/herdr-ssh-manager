//! A thin client over the `herdr` CLI, used to place the SSH session somewhere the
//! user can keep it.
//!
//! A popup is session-modal and dies with its process, which makes it the wrong home
//! for a long-lived SSH session. So instead of `exec`ing inside the popup, the picker
//! asks Herdr to run `ssh` in the pane the user came from — or, when that pane is
//! busy, in a new tab.

use anyhow::{anyhow, bail, Context as _, Result};
use serde::Deserialize;
use std::process::Command;

/// The slice of `HERDR_PLUGIN_CONTEXT_JSON` this plugin cares about.
#[derive(Debug, Clone, Deserialize)]
pub struct Context {
    pub workspace_id: Option<String>,
    /// The tab holding the focused pane.
    pub tab_id: Option<String>,
    /// The pane that was focused when the popup opened — where the user "is".
    pub focused_pane_id: Option<String>,
}

impl Context {
    /// Present only when Herdr launched us; absent from a plain shell.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?;
        serde_json::from_str(&raw).ok()
    }
}

fn bin_path() -> Result<String> {
    std::env::var("HERDR_BIN_PATH")
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("HERDR_BIN_PATH is not set"))
}

/// Run a `herdr` subcommand and return its `result` object.
///
/// Herdr's CLI is not uniform here: query commands answer with a JSON envelope, while
/// commands that just do something (`pane run`) print nothing at all and report success
/// through the exit status. Failures print a JSON `error` and exit non-zero.
fn call(args: &[&str]) -> Result<serde_json::Value> {
    let bin = bin_path()?;
    let output = Command::new(&bin)
        .args(args)
        .output()
        .with_context(|| format!("could not run {bin} {}", args.join(" ")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();

    if stdout.is_empty() {
        if output.status.success() {
            // Nothing to report is how the action commands say "done".
            return Ok(serde_json::Value::Null);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        bail!(
            "`herdr {}` failed{}",
            args.join(" "),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }

    let value: serde_json::Value = serde_json::from_str(stdout)
        .map_err(|_| anyhow!("`herdr {}` did not return JSON: {stdout}", args.join(" ")))?;

    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        bail!("`herdr {}` failed: {message}", args.join(" "));
    }
    Ok(value
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

/// Is the pane sitting at an idle shell prompt?
///
/// A shell waiting for input owns the foreground process group itself; anything else
/// running in there (an agent, an editor, a build) takes it over. That comparison is
/// what tells "free to reuse" from "busy, leave it alone".
pub fn pane_is_free(pane_id: &str) -> Result<bool> {
    let result = call(&["pane", "process-info", "--pane", pane_id])?;
    let info = result
        .get("process_info")
        .ok_or_else(|| anyhow!("pane {pane_id} reported no process info"))?;
    let foreground = info
        .get("foreground_process_group_id")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("pane {pane_id} reported no foreground process group"))?;
    let shell = info
        .get("shell_pid")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("pane {pane_id} reported no shell pid"))?;
    Ok(foreground == shell)
}

/// Create a focused tab and return its root pane id.
pub fn create_tab(workspace_id: Option<&str>, label: &str) -> Result<String> {
    let mut args: Vec<&str> = vec!["tab", "create", "--focus", "--label", label];
    if let Some(ws) = workspace_id {
        args.push("--workspace");
        args.push(ws);
    }
    let result = call(&args)?;
    result
        .get("root_pane")
        .and_then(|p| p.get("pane_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("herdr created a tab but reported no root pane"))
}

/// Give a tab a new label.
///
/// Note there is no way to clear it back to the automatic title: unlike `pane rename`,
/// `tab rename` takes a label and nothing else.
pub fn rename_tab(tab_id: &str, label: &str) -> Result<()> {
    call(&["tab", "rename", tab_id, label]).map(|_| ())
}

/// Type a command line into a pane's shell and run it.
///
/// Note that Herdr joins the argv it is given with plain spaces and feeds the result
/// to the shell — it does no quoting of its own. The command must therefore arrive
/// already quoted, as a single argument.
pub fn pane_run(pane_id: &str, command_line: &str) -> Result<()> {
    call(&["pane", "run", pane_id, command_line]).map(|_| ())
}

/// Quote one argument for a POSIX shell.
pub fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-')
        });
    if safe {
        return arg.to_string();
    }
    // Single quotes protect everything except a single quote, which has to be
    // closed, escaped and reopened.
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// Build one shell-safe command line from a program and its arguments.
pub fn quote_command(program: &str, args: &[String]) -> String {
    let mut out = shell_quote(program);
    for arg in args {
        out.push(' ');
        out.push_str(&shell_quote(arg));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_arguments_are_left_alone() {
        for arg in [
            "ssh",
            "-p",
            "2222",
            "deploy@example.com",
            "/home/j/.ssh/id_ed25519",
            "-oX=1",
        ] {
            assert_eq!(shell_quote(arg), arg, "{arg} should not need quoting");
        }
    }

    #[test]
    fn arguments_with_spaces_are_quoted() {
        assert_eq!(
            shell_quote("/home/jorge/mi clave.pem"),
            "'/home/jorge/mi clave.pem'"
        );
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn shell_metacharacters_cannot_escape_the_quotes() {
        assert_eq!(shell_quote("a;rm -rf /"), "'a;rm -rf /'");
        assert_eq!(shell_quote("$(id)"), "'$(id)'");
        assert_eq!(shell_quote("`id`"), "'`id`'");
        assert_eq!(shell_quote("a&&b"), "'a&&b'");
        assert_eq!(shell_quote("a\nb"), "'a\nb'");
    }

    #[test]
    fn embedded_single_quotes_are_closed_and_reopened() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        // The classic break-out attempt stays inert.
        assert_eq!(shell_quote("'; id; '"), r"''\''; id; '\'''");
    }

    #[test]
    fn quote_command_joins_into_one_safe_line() {
        let args = vec![
            "-p".to_string(),
            "2222".to_string(),
            "-i".to_string(),
            "/home/jorge/mi clave.pem".to_string(),
            "deploy@example.com".to_string(),
        ];
        assert_eq!(
            quote_command("ssh", &args),
            "ssh -p 2222 -i '/home/jorge/mi clave.pem' deploy@example.com"
        );
    }

    #[test]
    fn context_parses_the_fields_we_use_and_ignores_the_rest() {
        let raw = r#"{"workspace_id":"w1","workspace_label":"odoo15","tab_id":"w1:t1",
                      "focused_pane_id":"w1:p1","focused_pane_agent":"claude",
                      "invocation_source":"api"}"#;
        let ctx: Context = serde_json::from_str(raw).unwrap();
        assert_eq!(ctx.workspace_id.as_deref(), Some("w1"));
        assert_eq!(ctx.tab_id.as_deref(), Some("w1:t1"));
        assert_eq!(ctx.focused_pane_id.as_deref(), Some("w1:p1"));
    }

    #[test]
    fn a_context_without_a_focused_pane_still_parses() {
        let ctx: Context = serde_json::from_str(r#"{"workspace_id":"w1"}"#).unwrap();
        assert_eq!(ctx.focused_pane_id, None);
        assert_eq!(ctx.tab_id, None);
    }
}

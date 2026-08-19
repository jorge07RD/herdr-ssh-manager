//! Command-line surface: everything except the picker TUI.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use inquire::{Confirm, MultiSelect, Select, Text};

use crate::model::{Connection, Store};
use crate::store::StoreFile;
use crate::{import, ssh};

#[derive(Parser, Debug)]
#[command(
    name = "herdr-ssh-manager",
    about = "Save and reconnect to SSH hosts from Herdr",
    version,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Save a new connection (interactive unless --name and --host are given)
    Add(AddArgs),
    /// List saved connections
    List(ListArgs),
    /// Change a saved connection
    Edit(EditArgs),
    /// Delete a saved connection
    Remove(RemoveArgs),
    /// Import Host entries from ~/.ssh/config
    Import(ImportArgs),
    /// Open the fuzzy picker (this is what the Herdr popup pane runs)
    Pick,
    /// Connect to a saved connection by id
    Connect(ConnectArgs),
    /// Print where connections.toml lives
    Where,
    /// Ask Herdr to open the picker popup (bound to the open-picker action)
    #[command(hide = true)]
    OpenPicker,
    /// Ask Herdr to open the add popup (bound to the open-add action)
    #[command(hide = true)]
    OpenAdd,
}

#[derive(Args, Debug, Default)]
pub struct AddArgs {
    /// Label shown in the picker; also seeds the id
    #[arg(long)]
    pub name: Option<String>,
    /// Hostname or IP to connect to
    #[arg(long)]
    pub host: Option<String>,
    #[arg(long)]
    pub user: Option<String>,
    #[arg(long)]
    pub port: Option<u16>,
    /// Private key passed as `ssh -i`
    #[arg(long)]
    pub identity_file: Option<String>,
    /// Bastion passed as `ssh -J`
    #[arg(long)]
    pub jump_host: Option<String>,
    /// Repeatable
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// Extra flag appended verbatim to the ssh command; repeatable.
    /// Takes leading-hyphen values, e.g. `--extra-ssh-arg=-C`.
    #[arg(long = "extra-ssh-arg", allow_hyphen_values = true)]
    pub extra_ssh_args: Vec<String>,
    #[arg(long)]
    pub notes: Option<String>,
}

/// Carry over the parts of an entry that an edit must not disturb.
///
/// The id is the user's handle for this connection — scripts and `connect <id>` use it — so
/// renaming must not invalidate it. The timestamp is history, not something being edited.
pub fn carry_over_identity(updated: &mut Connection, existing: &Connection) {
    updated.id = existing.id.clone();
    updated.last_connected_at = existing.last_connected_at;
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct EditArgs {
    pub id: String,
    #[command(flatten)]
    pub fields: AddArgs,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    pub id: String,
    /// Skip the confirmation prompt
    #[arg(long, short)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct ImportArgs {
    /// Read from this file instead of ~/.ssh/config
    #[arg(long)]
    pub path: Option<std::path::PathBuf>,
    /// Show what would be imported and exit
    #[arg(long)]
    pub dry_run: bool,
    /// Import everything without prompting
    #[arg(long, short)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct ConnectArgs {
    pub id: String,
    /// Print the ssh command instead of running it
    #[arg(long)]
    pub print: bool,
}

// ---------------------------------------------------------------- add

pub fn add(args: AddArgs) -> Result<()> {
    let file = StoreFile::discover()?;
    let mut store = file.load_reporting()?;

    let conn = if args.name.is_some() && args.host.is_some() {
        from_flags(args)?
    } else {
        prompt_for_connection(args)?
    };

    let id = store.insert_unique(conn);
    file.save(&store)?;
    let saved = store.get(&id).expect("just inserted");
    println!("Saved `{id}` — {}", ssh::command_line(saved)?);
    Ok(())
}

fn from_flags(args: AddArgs) -> Result<Connection> {
    let name = args.name.unwrap_or_default();
    let host = args.host.unwrap_or_default();
    if name.trim().is_empty() {
        bail!("--name must not be empty");
    }
    if host.trim().is_empty() {
        bail!("--host must not be empty");
    }
    let mut conn = Connection::new(name.trim(), host.trim());
    apply_overrides(
        &mut conn,
        &args.user,
        &args.port,
        &args.identity_file,
        &args.jump_host,
        &args.notes,
    );
    conn.tags = args.tags;
    conn.extra_ssh_args = args.extra_ssh_args;
    // Surface a bad host now rather than at connect time.
    ssh::build_args(&conn)?;
    Ok(conn)
}

fn apply_overrides(
    conn: &mut Connection,
    user: &Option<String>,
    port: &Option<u16>,
    identity_file: &Option<String>,
    jump_host: &Option<String>,
    notes: &Option<String>,
) {
    if let Some(v) = user {
        conn.user = blank_to_none(v);
    }
    if let Some(v) = port {
        conn.port = *v;
    }
    if let Some(v) = identity_file {
        conn.identity_file = blank_to_none(v);
    }
    if let Some(v) = jump_host {
        conn.jump_host = blank_to_none(v);
    }
    if let Some(v) = notes {
        conn.notes = blank_to_none(v);
    }
}

fn blank_to_none(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// One editable field of a saved connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Name,
    Host,
    User,
    Port,
    IdentityFile,
    JumpHost,
    Tags,
    ExtraSshArgs,
    Notes,
}

impl Field {
    const ALL: [Field; 9] = [
        Field::Name,
        Field::Host,
        Field::User,
        Field::Port,
        Field::IdentityFile,
        Field::JumpHost,
        Field::Tags,
        Field::ExtraSshArgs,
        Field::Notes,
    ];

    fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Host => "Host",
            Field::User => "User",
            Field::Port => "Port",
            Field::IdentityFile => "Identity file",
            Field::JumpHost => "Jump host",
            Field::Tags => "Tags",
            Field::ExtraSshArgs => "Extra ssh args",
            Field::Notes => "Notes",
        }
    }

    /// The field's current value, or an em dash when it is unset.
    fn value_of(self, conn: &Connection) -> String {
        let raw = match self {
            Field::Name => conn.name.clone(),
            Field::Host => conn.host.clone(),
            Field::User => conn.user.clone().unwrap_or_default(),
            Field::Port => conn.port.to_string(),
            Field::IdentityFile => conn.identity_file.clone().unwrap_or_default(),
            Field::JumpHost => conn.jump_host.clone().unwrap_or_default(),
            Field::Tags => conn.tags.join(", "),
            Field::ExtraSshArgs => conn.extra_ssh_args.join(" "),
            Field::Notes => conn.notes.clone().unwrap_or_default(),
        };
        if raw.is_empty() {
            "—".to_string()
        } else {
            raw
        }
    }
}

/// Edit a saved connection by picking fields off a view of the whole record.
///
/// Walking every field in sequence — the way `add` does — is wrong for editing: it hides
/// the record behind one prompt at a time and makes you confirm values you never intended
/// to touch. Here the whole entry stays on screen, and only what you select changes.
///
/// Returns `None` when the edit was discarded or cancelled.
pub fn edit_form(existing: &Connection) -> Result<Option<Connection>> {
    let mut draft = existing.clone();
    // Coming back from a field should leave the cursor where it was, not at the top:
    // fixing three fields in a row should not mean scrolling down from Name each time.
    let mut cursor = 0usize;

    loop {
        let mut options: Vec<String> = Field::ALL
            .iter()
            .map(|f| format!("{:<16} {}", f.label(), f.value_of(&draft)))
            .collect();
        let save_idx = options.len();
        options.push("Save changes".to_string());
        options.push("Discard".to_string());

        // Showing the command the entry resolves to makes the effect of an edit concrete;
        // an entry too broken to render one is reported at save time, not here.
        let help = ssh::command_line(&draft)
            .unwrap_or_else(|_| "this entry cannot be turned into an ssh command yet".into());

        let choice = Select::new(&format!("Edit `{}`", existing.name), options)
            .with_help_message(&help)
            .with_page_size(11)
            .with_starting_cursor(cursor)
            .raw_prompt();

        // Esc out of the menu discards, the same as picking Discard.
        let Ok(choice) = choice else {
            return Ok(None);
        };

        if choice.index == save_idx + 1 {
            return Ok(None);
        }
        if choice.index == save_idx {
            // Refuse to save something that could never connect, but keep the draft so the
            // work is not thrown away.
            match ssh::build_args(&draft) {
                Ok(_) => return Ok(Some(draft)),
                Err(e) => {
                    eprintln!("  cannot save: {e:#}");
                    continue;
                }
            }
        }

        cursor = choice.index;
        edit_one_field(Field::ALL[choice.index], &mut draft, existing)?;
    }
}

/// Prompt for a single field, seeded with its current value.
fn edit_one_field(field: Field, draft: &mut Connection, existing: &Connection) -> Result<()> {
    let current = match field {
        Field::Tags => draft.tags.join(", "),
        Field::ExtraSshArgs => draft.extra_ssh_args.join(" "),
        Field::Port => draft.port.to_string(),
        Field::Name => draft.name.clone(),
        Field::Host => draft.host.clone(),
        Field::User => draft.user.clone().unwrap_or_default(),
        Field::IdentityFile => draft.identity_file.clone().unwrap_or_default(),
        Field::JumpHost => draft.jump_host.clone().unwrap_or_default(),
        Field::Notes => draft.notes.clone().unwrap_or_default(),
    };

    let prompt = match field {
        Field::Tags => "Tags (comma separated)".to_string(),
        Field::ExtraSshArgs => "Extra ssh args (space separated)".to_string(),
        other => other.label().to_string(),
    };

    // Cancelling a single field leaves the draft alone rather than aborting the whole edit.
    let mut text = Text::new(&prompt);
    if !current.is_empty() {
        text = text.with_initial_value(&current);
    }
    let Ok(value) = text.prompt() else {
        return Ok(());
    };
    let value = value.trim().to_string();

    match field {
        Field::Name => {
            if value.is_empty() {
                eprintln!("  name is required; left unchanged");
            } else {
                draft.name = value;
            }
        }
        Field::Host => {
            if value.is_empty() {
                eprintln!("  host is required; left unchanged");
            } else {
                draft.host = value;
            }
        }
        Field::Port => match value.parse::<u16>() {
            Ok(p) if p > 0 => draft.port = p,
            _ => eprintln!("  not a valid port (1-65535); left unchanged"),
        },
        Field::User => draft.user = blank_to_none(&value),
        Field::IdentityFile => draft.identity_file = blank_to_none(&value),
        Field::JumpHost => draft.jump_host = blank_to_none(&value),
        Field::Notes => draft.notes = blank_to_none(&value),
        Field::Tags => {
            draft.tags = value.split(',').filter_map(blank_to_none).collect();
        }
        Field::ExtraSshArgs => {
            // Splitting on whitespace cannot express an argument that contains a space, so
            // an untouched line keeps the original vector verbatim — editing other fields
            // can never quietly flatten `-o "ProxyCommand=ssh -W %h:%p bastion"`.
            if value == existing.extra_ssh_args.join(" ") {
                draft.extra_ssh_args = existing.extra_ssh_args.clone();
            } else {
                draft.extra_ssh_args = value.split_whitespace().map(str::to_string).collect();
            }
        }
    }
    Ok(())
}

/// The interactive form used by `add` and by Ctrl-A inside the picker.
pub fn prompt_for_connection(defaults: AddArgs) -> Result<Connection> {
    let name = prompt_text("Name", defaults.name.as_deref(), true)?;
    let host = prompt_text("Host", defaults.host.as_deref(), true)?;
    let user = prompt_text("User (optional)", defaults.user.as_deref(), false)?;
    let port = loop {
        let raw = prompt_text(
            "Port",
            Some(&defaults.port.unwrap_or(22).to_string()),
            false,
        )?;
        if raw.trim().is_empty() {
            break 22;
        }
        match raw.trim().parse::<u16>() {
            Ok(p) if p > 0 => break p,
            _ => eprintln!("  not a valid port (1-65535), try again"),
        }
    };
    let identity = prompt_text(
        "Identity file (optional, e.g. ~/.ssh/id_ed25519)",
        defaults.identity_file.as_deref(),
        false,
    )?;
    let jump = prompt_text(
        "Jump host / ProxyJump (optional)",
        defaults.jump_host.as_deref(),
        false,
    )?;
    let tags_default = defaults.tags.join(", ");
    let tags = prompt_text(
        "Tags (comma separated, optional)",
        Some(&tags_default),
        false,
    )?;
    let notes = prompt_text("Notes (optional)", defaults.notes.as_deref(), false)?;

    let mut conn = Connection::new(name.trim(), host.trim());
    conn.user = blank_to_none(&user);
    conn.port = port;
    conn.identity_file = blank_to_none(&identity);
    conn.jump_host = blank_to_none(&jump);
    conn.tags = tags.split(',').filter_map(blank_to_none).collect();
    conn.extra_ssh_args = defaults.extra_ssh_args;
    conn.notes = blank_to_none(&notes);
    ssh::build_args(&conn)?;
    Ok(conn)
}

fn prompt_text(message: &str, default: Option<&str>, required: bool) -> Result<String> {
    loop {
        let mut prompt = Text::new(message);
        let default = default.map(str::to_string).filter(|d| !d.is_empty());
        if let Some(d) = &default {
            prompt = prompt.with_default(d);
        }
        let value = prompt.prompt().context("input cancelled")?;
        if required && value.trim().is_empty() {
            eprintln!("  {message} is required");
            continue;
        }
        return Ok(value);
    }
}

// ---------------------------------------------------------------- list

pub fn list(args: ListArgs) -> Result<()> {
    let file = StoreFile::discover()?;
    let store = file.load_reporting()?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&store.connections)?);
        return Ok(());
    }

    if store.connections.is_empty() {
        println!("No saved connections yet.");
        println!("Add one with `herdr-ssh-manager add`, or import your ssh config with `herdr-ssh-manager import`.");
        return Ok(());
    }

    let rows: Vec<[String; 4]> = store
        .sorted_by_recency()
        .iter()
        .map(|c| {
            [
                c.id.clone(),
                c.name.clone(),
                c.destination(),
                match c.last_connected_at {
                    Some(ts) => crate::picker::humanize_since(ts),
                    None => "never".to_string(),
                },
            ]
        })
        .collect();

    let headers = ["ID", "NAME", "DESTINATION", "LAST USED"];
    let widths: Vec<usize> = (0..4)
        .map(|i| {
            rows.iter()
                .map(|r| r[i].chars().count())
                .chain(std::iter::once(headers[i].len()))
                .max()
                .unwrap_or(0)
        })
        .collect();

    let line = |cells: [&str; 4]| {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i == 3 {
                out.push_str(cell);
            } else {
                out.push_str(&format!("{:<width$}  ", cell, width = widths[i]));
            }
        }
        out.trim_end().to_string()
    };

    println!("{}", line(headers));
    for row in &rows {
        println!("{}", line([&row[0], &row[1], &row[2], &row[3]]));
    }
    Ok(())
}

// ---------------------------------------------------------------- edit / remove

pub fn edit(args: EditArgs) -> Result<()> {
    let file = StoreFile::discover()?;
    let mut store = file.load_reporting()?;
    let existing = store
        .get(&args.id)
        .cloned()
        .ok_or_else(|| unknown_id(&store, &args.id))?;

    let f = args.fields;
    let any_flag = f.name.is_some()
        || f.host.is_some()
        || f.user.is_some()
        || f.port.is_some()
        || f.identity_file.is_some()
        || f.jump_host.is_some()
        || f.notes.is_some()
        || !f.tags.is_empty()
        || !f.extra_ssh_args.is_empty();

    let mut updated = if any_flag {
        let mut c = existing.clone();
        if let Some(name) = &f.name {
            if name.trim().is_empty() {
                bail!("--name must not be empty");
            }
            c.name = name.trim().to_string();
        }
        if let Some(host) = &f.host {
            if host.trim().is_empty() {
                bail!("--host must not be empty");
            }
            c.host = host.trim().to_string();
        }
        apply_overrides(
            &mut c,
            &f.user,
            &f.port,
            &f.identity_file,
            &f.jump_host,
            &f.notes,
        );
        if !f.tags.is_empty() {
            c.tags = f.tags.clone();
        }
        if !f.extra_ssh_args.is_empty() {
            c.extra_ssh_args = f.extra_ssh_args.clone();
        }
        ssh::build_args(&c)?;
        c
    } else {
        match edit_form(&existing)? {
            Some(updated) => updated,
            None => {
                println!("No changes to `{}`.", existing.id);
                return Ok(());
            }
        }
    };

    carry_over_identity(&mut updated, &existing);
    *store.get_mut(&args.id).expect("checked above") = updated;
    file.save(&store)?;
    println!(
        "Updated `{}` — {}",
        args.id,
        ssh::command_line(store.get(&args.id).unwrap())?
    );
    Ok(())
}

pub fn remove(args: RemoveArgs) -> Result<()> {
    let file = StoreFile::discover()?;
    let mut store = file.load_reporting()?;
    let target = store
        .get(&args.id)
        .cloned()
        .ok_or_else(|| unknown_id(&store, &args.id))?;

    if !args.yes {
        let ok = Confirm::new(&format!(
            "Delete `{}` ({})?",
            target.id,
            target.destination()
        ))
        .with_default(false)
        .prompt()
        .context("confirmation cancelled")?;
        if !ok {
            println!("Kept `{}`.", target.id);
            return Ok(());
        }
    }

    store.remove(&args.id);
    file.save(&store)?;
    println!("Deleted `{}`.", args.id);
    Ok(())
}

/// Suggest near matches so a typo does not read as "your data is gone".
fn unknown_id(store: &Store, id: &str) -> anyhow::Error {
    if store.connections.is_empty() {
        return anyhow::anyhow!("no connection `{id}`: the store is empty");
    }
    let mut known: Vec<&str> = store.connections.iter().map(|c| c.id.as_str()).collect();
    known.sort_unstable();
    anyhow::anyhow!("no connection `{id}`. Known ids: {}", known.join(", "))
}

// ---------------------------------------------------------------- import

pub fn import(args: ImportArgs) -> Result<()> {
    let path = match args.path {
        Some(p) => p,
        None => import::default_ssh_config_path()
            .context("cannot locate ~/.ssh/config: set HOME or pass --path")?,
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read {}", path.display()))?;

    let file = StoreFile::discover()?;
    let mut store = file.load_reporting()?;

    let parsed = import::parse(&text);
    let candidates: Vec<Connection> = parsed
        .iter()
        .map(|h| h.to_connection())
        .filter(|c| !already_saved(&store, c))
        .filter(|c| ssh::build_args(c).is_ok())
        .collect();

    if candidates.is_empty() {
        println!(
            "Nothing to import from {}: {} host entr{} already saved or unusable.",
            path.display(),
            parsed.len(),
            if parsed.len() == 1 { "y is" } else { "ies are" }
        );
        return Ok(());
    }

    println!("Importable from {}:", path.display());
    for c in &candidates {
        println!("  {:<20} {}", c.name, c.destination());
    }

    if args.dry_run {
        println!("\n--dry-run: nothing was saved.");
        return Ok(());
    }

    let chosen: Vec<Connection> = if args.yes {
        candidates
    } else {
        let labels: Vec<String> = candidates
            .iter()
            .map(|c| format!("{} ({})", c.name, c.destination()))
            .collect();
        let picked = MultiSelect::new("Import which hosts?", labels.clone())
            .with_all_selected_by_default()
            .prompt()
            .context("import cancelled")?;
        candidates
            .into_iter()
            .zip(labels)
            .filter(|(_, label)| picked.contains(label))
            .map(|(c, _)| c)
            .collect()
    };

    if chosen.is_empty() {
        println!("Nothing selected.");
        return Ok(());
    }

    let count = chosen.len();
    for conn in chosen {
        store.insert_unique(conn);
    }
    file.save(&store)?;
    println!(
        "Imported {count} connection{}.",
        if count == 1 { "" } else { "s" }
    );
    Ok(())
}

/// Treat host+user+port as the identity of a destination for dedup purposes.
fn already_saved(store: &Store, candidate: &Connection) -> bool {
    store.connections.iter().any(|c| {
        c.host.eq_ignore_ascii_case(&candidate.host)
            && c.port == candidate.port
            && c.user.as_deref() == candidate.user.as_deref()
    })
}

// ---------------------------------------------------------------- connect / where

pub fn connect(args: ConnectArgs) -> Result<()> {
    let file = StoreFile::discover()?;
    let mut store = file.load_reporting()?;
    let conn = store
        .get(&args.id)
        .cloned()
        .ok_or_else(|| unknown_id(&store, &args.id))?;

    if args.print {
        println!("{}", ssh::command_line(&conn)?);
        return Ok(());
    }

    // Fail before touching the store if ssh or the entry is unusable.
    ssh::find_ssh()?;
    ssh::build_args(&conn)?;

    // Persist first: after exec this process no longer exists.
    if let Some(entry) = store.get_mut(&args.id) {
        entry.last_connected_at = Some(chrono::Utc::now());
    }
    if let Err(e) = file.save(&store) {
        eprintln!("herdr-ssh-manager: could not record the connection time: {e:#}");
    }

    ssh::connect(&conn)?;
    unreachable!("exec replaces the process")
}

pub fn where_cmd() -> Result<()> {
    let file = StoreFile::discover()?;
    println!("{}", file.path().display());
    Ok(())
}

// ---------------------------------------------------------------- popup launchers

/// Ask Herdr to open one of the popup panes declared in herdr-plugin.toml.
pub fn open_pane(entrypoint: &str) -> Result<()> {
    let herdr = std::env::var("HERDR_BIN_PATH")
        .ok()
        .filter(|v| !v.is_empty())
        .context(
            "HERDR_BIN_PATH is not set. This subcommand is meant to be run by Herdr as a \
         plugin action; from a shell, run `herdr-ssh-manager pick` directly instead.",
        )?;
    let status = std::process::Command::new(&herdr)
        .args([
            "plugin",
            "pane",
            "open",
            "--plugin",
            "herdr-ssh-manager",
            "--entrypoint",
            entrypoint,
            "--placement",
            "popup",
        ])
        .status()
        .with_context(|| format!("could not run {herdr}"))?;
    if !status.success() {
        bail!("`herdr plugin pane open --entrypoint {entrypoint}` failed with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_flags_requires_a_non_empty_name_and_host() {
        let args = AddArgs {
            name: Some("  ".into()),
            host: Some("h".into()),
            ..Default::default()
        };
        assert!(from_flags(args).unwrap_err().to_string().contains("--name"));

        let args = AddArgs {
            name: Some("n".into()),
            host: Some(" ".into()),
            ..Default::default()
        };
        assert!(from_flags(args).unwrap_err().to_string().contains("--host"));
    }

    #[test]
    fn from_flags_carries_every_field() {
        let args = AddArgs {
            name: Some("Prod DB".into()),
            host: Some("db.example.com".into()),
            user: Some("deploy".into()),
            port: Some(2222),
            identity_file: Some("~/.ssh/id".into()),
            jump_host: Some("bastion".into()),
            tags: vec!["prod".into()],
            extra_ssh_args: vec!["-C".into()],
            notes: Some("primary".into()),
        };
        let c = from_flags(args).unwrap();
        assert_eq!(c.id, "prod-db");
        assert_eq!(c.name, "Prod DB");
        assert_eq!(c.port, 2222);
        assert_eq!(c.user.as_deref(), Some("deploy"));
        assert_eq!(c.jump_host.as_deref(), Some("bastion"));
        assert_eq!(c.tags, vec!["prod".to_string()]);
        assert_eq!(c.extra_ssh_args, vec!["-C".to_string()]);
    }

    #[test]
    fn from_flags_rejects_a_host_that_would_inject_an_ssh_flag() {
        let args = AddArgs {
            name: Some("evil".into()),
            host: Some("-oProxyCommand=id".into()),
            ..Default::default()
        };
        assert!(from_flags(args).is_err());
    }

    #[test]
    fn dedup_matches_on_host_user_and_port_only() {
        let mut store = Store::default();
        let mut existing = Connection::new("web", "example.com");
        existing.user = Some("deploy".into());
        store.insert_unique(existing);

        let mut same = Connection::new("a different label", "EXAMPLE.COM");
        same.user = Some("deploy".into());
        assert!(already_saved(&store, &same));

        let mut other_user = Connection::new("web", "example.com");
        other_user.user = Some("root".into());
        assert!(!already_saved(&store, &other_user));

        let mut other_port = Connection::new("web", "example.com");
        other_port.user = Some("deploy".into());
        other_port.port = 2222;
        assert!(!already_saved(&store, &other_port));
    }

    #[test]
    fn unknown_id_lists_what_is_available() {
        let mut store = Store::default();
        store.insert_unique(Connection::new("web", "w"));
        store.insert_unique(Connection::new("db", "d"));
        let msg = unknown_id(&store, "wbe").to_string();
        assert!(msg.contains("db, web"), "got: {msg}");

        assert!(unknown_id(&Store::default(), "x")
            .to_string()
            .contains("empty"));
    }

    #[test]
    fn blank_to_none_trims() {
        assert_eq!(blank_to_none("  x  ").as_deref(), Some("x"));
        assert_eq!(blank_to_none("   "), None);
    }
}

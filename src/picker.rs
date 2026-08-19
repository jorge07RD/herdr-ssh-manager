//! The fuzzy picker shown in the Herdr popup pane.
//!
//! Everything printable goes to the filter (fzf-style), so the destructive actions sit
//! on control chords: Ctrl-A adds, Ctrl-D deletes. Enter records the connection time,
//! puts the terminal back the way it was found, and hands the process to `ssh` — which
//! on Unix means this popup *becomes* the session.

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear as ClearScreen, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::io::{self, Stdout};
use std::time::Duration;

use crate::model::{Connection, Store};
use crate::store::StoreFile;
use crate::{cli, herdr, ssh};

/// Restores the terminal on the way out, including while a panic unwinds.
struct TerminalGuard {
    armed: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("could not put the terminal into raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)
            .context("could not switch to the alternate screen")?;
        Ok(Self { armed: true })
    }

    /// Hand the terminal to a line-oriented prompt **without leaving the alternate screen**.
    ///
    /// Leaving it would drop the form onto the user's real terminal, where the output of
    /// every previous form is still sitting — so a second Ctrl-A would render under the
    /// leftovers of the last Ctrl-E and read as a corrupted prompt. Staying inside the
    /// alternate screen and clearing it gives each form a blank slate and leaves whatever
    /// the user had on screen before the picker completely untouched.
    fn suspend(&mut self) -> Result<()> {
        let _ = disable_raw_mode();
        execute!(
            io::stdout(),
            ClearScreen(ClearType::All),
            cursor::MoveTo(0, 0),
            cursor::Show
        )
        .context("could not hand the terminal to the form")
    }

    /// Take the terminal back after a form.
    fn resume(&mut self) -> Result<()> {
        enable_raw_mode().context("could not return the terminal to raw mode")?;
        execute!(io::stdout(), cursor::Hide).context("could not hide the cursor")
    }

    /// Put the terminal back now, and stop the Drop impl from doing it again.
    /// Must be called before exec'ing ssh: after that, Drop never runs.
    fn restore(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
        let _ = disable_raw_mode();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}

/// What the event loop decided to do.
enum Outcome {
    Quit,
    /// Connect, carrying any non-fatal complaint to print once the TUI is torn down.
    /// Boxed to keep the enum small — `Quit` is by far the common case.
    Connect(Box<(Connection, Option<String>)>),
}

/// A pending destructive action, drawn as a modal over the list.
enum Mode {
    Normal,
    ConfirmDelete { id: String, label: String },
}

pub fn run() -> Result<()> {
    let file = StoreFile::discover()?;
    let mut store = file.load_reporting()?;

    let mut guard = TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .context("could not initialise the terminal")?;

    let outcome = event_loop(&mut terminal, &mut guard, &file, &mut store);

    // Give the terminal back before doing anything that writes to it.
    guard.restore();
    let outcome = outcome?;

    match outcome {
        Outcome::Quit => Ok(()),
        Outcome::Connect(payload) => {
            let (conn, warning) = *payload;
            if let Some(warning) = warning {
                eprintln!("herdr-ssh-manager: {warning}");
            }
            connect_somewhere(&conn)
        }
    }
}

/// Put the SSH session where the user can keep it.
///
/// Under Herdr that means handing it to a real pane — the one the popup was opened
/// from, or a fresh tab when that pane is busy — because a popup is modal and dies
/// with its process. Outside Herdr there is nowhere to hand it to, so this process
/// becomes the session instead.
fn connect_somewhere(conn: &Connection) -> Result<()> {
    let Some(ctx) = herdr::Context::from_env() else {
        ssh::connect(conn)?;
        unreachable!("exec replaces the process")
    };

    match hand_off(&ctx, conn) {
        Ok(Placement::ReusedPane) => Ok(()),
        Ok(Placement::NewTab) => Ok(()),
        Err(e) => {
            // Rather than dead-end, fall back to the popup and say why.
            eprintln!("herdr-ssh-manager: could not open the session in a pane ({e:#}).");
            eprintln!("herdr-ssh-manager: connecting here instead.\n");
            ssh::connect(conn)?;
            unreachable!("exec replaces the process")
        }
    }
}

enum Placement {
    ReusedPane,
    NewTab,
}

/// Ask Herdr to run ssh in a pane, reusing the focused one when it is idle.
fn hand_off(ctx: &herdr::Context, conn: &Connection) -> Result<Placement> {
    let program = ssh::find_ssh()?;
    let args = ssh::build_args(conn)?;
    // Herdr types what it is given straight into the shell without quoting, so the
    // command has to arrive already safe.
    let command = herdr::quote_command(&program.to_string_lossy(), &args);

    // An unreadable or missing pane counts as busy: better a spare tab than a
    // command typed over something that matters.
    let reusable = ctx
        .focused_pane_id
        .as_deref()
        .filter(|pane| herdr::pane_is_free(pane).unwrap_or(false));

    if let Some(pane) = reusable {
        herdr::pane_run(pane, &command)?;
        // Name the tab after the connection, the same way a freshly created one is
        // named. Non-fatal: a session that runs is worth more than its label, and
        // there is no way to restore the automatic title afterwards anyway.
        if let Some(tab) = ctx.tab_id.as_deref() {
            let _ = herdr::rename_tab(tab, &conn.name);
        }
        // No focus call needed: this is the pane the popup was opened from, so
        // closing the popup lands the user right on it.
        return Ok(Placement::ReusedPane);
    }

    let pane = herdr::create_tab(ctx.workspace_id.as_deref(), &conn.name)?;
    herdr::pane_run(&pane, &command)?;
    Ok(Placement::NewTab)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    guard: &mut TerminalGuard,
    file: &StoreFile,
    store: &mut Store,
) -> Result<Outcome> {
    let mut query = String::new();
    let mut selected: usize = 0;
    let mut mode = Mode::Normal;
    let mut status: Option<String> = None;
    let mut matcher = Matcher::new(Config::DEFAULT);

    loop {
        let matches = filter(store, &query, &mut matcher);
        if selected >= matches.len() {
            selected = matches.len().saturating_sub(1);
        }

        terminal.draw(|frame| {
            draw(
                frame,
                store,
                &matches,
                &query,
                selected,
                &mode,
                status.as_deref(),
            )
        })?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        // Windows reports both press and release; act on press only.
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if let Mode::ConfirmDelete { id, .. } = &mode {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let id = id.clone();
                    let removed = store.remove(&id);
                    match file.save(store) {
                        Ok(()) => {
                            status = removed.map(|c| format!("Deleted `{}`.", c.id));
                        }
                        Err(e) => {
                            // Put it back rather than diverge from what is on disk.
                            if let Some(c) = removed {
                                store.connections.push(c);
                            }
                            status = Some(format!("Could not save: {e:#}"));
                        }
                    }
                    mode = Mode::Normal;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    mode = Mode::Normal;
                    status = None;
                }
                _ => {}
            }
            continue;
        }

        status = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match (key.code, ctrl) {
            (KeyCode::Esc, _) => {
                // Esc clears a filter first; only an empty filter closes the picker.
                if query.is_empty() {
                    return Ok(Outcome::Quit);
                }
                query.clear();
                selected = 0;
            }
            (KeyCode::Char('c'), true) | (KeyCode::Char('q'), true) => return Ok(Outcome::Quit),
            (KeyCode::Char('q'), false) if query.is_empty() => return Ok(Outcome::Quit),

            (KeyCode::Down, _) | (KeyCode::Char('n'), true) => {
                if !matches.is_empty() {
                    selected = (selected + 1) % matches.len();
                }
            }
            (KeyCode::Up, _) | (KeyCode::Char('p'), true) => {
                if !matches.is_empty() {
                    selected = selected.checked_sub(1).unwrap_or(matches.len() - 1);
                }
            }

            (KeyCode::Enter, _) => {
                let Some(conn) = matches.get(selected).map(|c| (*c).clone()) else {
                    status = Some("Nothing to connect to.".into());
                    continue;
                };
                // Validate before tearing the UI down so the error stays readable.
                if let Err(e) = ssh::find_ssh().and_then(|_| ssh::build_args(&conn).map(|_| ())) {
                    status = Some(format!("{e:#}"));
                    continue;
                }
                if let Some(entry) = store.get_mut(&conn.id) {
                    entry.last_connected_at = Some(chrono::Utc::now());
                }
                // Not fatal: connecting matters more than the timestamp, but the
                // complaint has to outlive the TUI to be seen at all.
                let warning = file
                    .save(store)
                    .err()
                    .map(|e| format!("could not record the connection time: {e:#}"));
                return Ok(Outcome::Connect(Box::new((conn, warning))));
            }

            (KeyCode::Char('a'), true) => match add_interactively(terminal, guard, file, store) {
                Ok(Some(id)) => {
                    query.clear();
                    selected = 0;
                    status = Some(format!("Saved `{id}`."));
                }
                Ok(None) => status = Some("Add cancelled.".into()),
                Err(e) => status = Some(format!("{e:#}")),
            },

            (KeyCode::Char('e'), true) => {
                let Some(id) = matches.get(selected).map(|c| c.id.clone()) else {
                    status = Some("Nothing to edit.".into());
                    continue;
                };
                match edit_interactively(terminal, guard, file, store, &id) {
                    // The filter is deliberately left alone: the user narrowed the list to
                    // reach this entry, and editing it is no reason to lose that.
                    Ok(Some(name)) => status = Some(format!("Updated `{name}`.")),
                    Ok(None) => status = Some("Edit cancelled.".into()),
                    Err(e) => status = Some(format!("{e:#}")),
                }
            }

            (KeyCode::Char('d'), true) => {
                if let Some(conn) = matches.get(selected) {
                    mode = Mode::ConfirmDelete {
                        id: conn.id.clone(),
                        label: format!("{} ({})", conn.name, conn.destination()),
                    };
                }
            }

            (KeyCode::Backspace, _) => {
                query.pop();
                selected = 0;
            }
            (KeyCode::Char(c), false) => {
                query.push(c);
                selected = 0;
            }
            _ => {}
        }
    }
}

/// Drop out of the TUI, run the inquire form on the normal screen, then come back.
///
/// Returns the form's result, or `None` when the user cancelled — backing out of a form is a
/// normal way to leave it, not an error worth shouting about.
fn prompt_outside_tui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    guard: &mut TerminalGuard,
    defaults: cli::AddArgs,
) -> Result<Option<Connection>> {
    guard.suspend()?;
    let result = cli::prompt_for_connection(defaults);
    guard.resume()?;
    terminal.clear()?;
    Ok(result.ok())
}

/// Same suspend/resume dance, for the field-picker editor.
fn edit_outside_tui(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    guard: &mut TerminalGuard,
    existing: &Connection,
) -> Result<Option<Connection>> {
    guard.suspend()?;
    let result = cli::edit_form(existing, cli::Surface::Owned);
    guard.resume()?;
    terminal.clear()?;
    result
}

/// Add a connection without leaving the picker.
fn add_interactively(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    guard: &mut TerminalGuard,
    file: &StoreFile,
    store: &mut Store,
) -> Result<Option<String>> {
    let Some(conn) = prompt_outside_tui(terminal, guard, cli::AddArgs::default())? else {
        return Ok(None);
    };
    let id = store.insert_unique(conn);
    file.save(store)?;
    Ok(Some(id))
}

/// Edit the selected connection, with the form prefilled from what is saved.
fn edit_interactively(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    guard: &mut TerminalGuard,
    file: &StoreFile,
    store: &mut Store,
    id: &str,
) -> Result<Option<String>> {
    let Some(existing) = store.get(id).cloned() else {
        return Ok(None);
    };
    let Some(mut updated) = edit_outside_tui(terminal, guard, &existing)? else {
        return Ok(None);
    };
    // Keep the handle and the history: a rename here must not break `connect <id>`.
    cli::carry_over_identity(&mut updated, &existing);
    let name = updated.name.clone();
    *store.get_mut(id).expect("checked above") = updated;
    file.save(store)?;
    Ok(Some(name))
}

/// Score every connection against the query; an empty query keeps recency order.
fn filter<'a>(store: &'a Store, query: &str, matcher: &mut Matcher) -> Vec<&'a Connection> {
    if query.trim().is_empty() {
        return store.sorted_by_recency();
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, &Connection)> = store
        .connections
        .iter()
        .filter_map(|conn| {
            let mut buf = Vec::new();
            let haystack = conn.search_text();
            pattern
                .score(nucleo_matcher::Utf32Str::new(&haystack, &mut buf), matcher)
                .map(|score| (score, conn))
        })
        .collect();
    // Highest score first; ties fall back to most recently used.
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.last_connected_at.cmp(&a.1.last_connected_at))
    });
    scored.into_iter().map(|(_, c)| c).collect()
}

fn draw(
    frame: &mut Frame,
    store: &Store,
    matches: &[&Connection],
    query: &str,
    selected: usize,
    mode: &Mode,
    status: Option<&str>,
) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());

    // Search line.
    let prompt = Line::from(vec![
        Span::styled(
            "  > ",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw(query),
        Span::styled("▏", Style::new().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(prompt), chunks[0]);

    // Results, or an empty state.
    if store.connections.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No saved connections yet.",
                Style::new().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("  Press Ctrl-A to add one now,"),
            Line::from("  or run `herdr-ssh-manager import` to pull in your ~/.ssh/config."),
        ])
        .wrap(Wrap { trim: false });
        frame.render_widget(msg, chunks[1]);
    } else if matches.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "    no match",
                Style::new().fg(Color::DarkGray),
            ))),
            chunks[1],
        );
    } else {
        let items: Vec<ListItem> = matches.iter().map(|c| ListItem::new(row(c))).collect();
        let list = List::new(items)
            // The symbol also indents every row, selected or not, so the list lines up
            // under the search prompt above it.
            .highlight_symbol("  ▌ ")
            .highlight_style(
                Style::new()
                    .bg(Color::Indexed(238))
                    .add_modifier(Modifier::BOLD),
            );
        let mut state = ListState::default();
        state.select(Some(selected));
        frame.render_stateful_widget(list, chunks[1], &mut state);
    }

    // Footer: a status message when there is one, otherwise the key help.
    let footer = match status {
        Some(msg) => Line::from(Span::styled(
            format!("  {msg}"),
            Style::new().fg(Color::Yellow),
        )),
        None => Line::from(footer_hints(chunks[2].width, store.connections.len())),
    };
    frame.render_widget(Paragraph::new(footer), chunks[2]);

    if let Mode::ConfirmDelete { label, .. } = mode {
        draw_confirm(frame, label);
    }
}

/// The key help, in a form that fits the pane it is drawn in.
///
/// The picker's popup is a percentage of the terminal, so on a narrow window the full hint
/// line would simply be clipped — and it is the rightmost hints that would vanish. Falling
/// back to a shorter wording keeps every key visible instead of silently losing some.
fn footer_hints(width: u16, saved: usize) -> Vec<Span<'static>> {
    let full = vec![
        Span::raw("  "),
        key_hint("↑↓", "move"),
        key_hint("enter", "connect"),
        key_hint("^A", "add"),
        key_hint("^E", "edit"),
        key_hint("^D", "delete"),
        key_hint("esc", "close"),
        Span::styled(format!("{saved} saved"), Style::new().fg(Color::DarkGray)),
    ];
    if line_width(&full) <= width as usize {
        return full;
    }
    vec![
        Span::raw("  "),
        key_hint("⏎", "connect"),
        key_hint("^A", "add"),
        key_hint("^E", "edit"),
        key_hint("^D", "del"),
        Span::styled("esc close", Style::new().fg(Color::DarkGray)),
    ]
}

fn line_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

fn key_hint(key: &str, label: &str) -> Span<'static> {
    Span::styled(
        format!("{key} {label}   "),
        Style::new().fg(Color::DarkGray),
    )
}

fn row(conn: &Connection) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{:<18}", truncate(&conn.name, 18)),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<30}", truncate(&conn.destination(), 30)),
            Style::new().fg(Color::Cyan),
        ),
    ];
    // The tag column is padded even when empty, so the timestamps after it line up
    // instead of drifting with each row's tag list.
    spans.push(Span::styled(
        format!("{:<14}", truncate(&conn.tags.join(","), 14)),
        Style::new().fg(Color::Magenta),
    ));
    if let Some(ts) = conn.last_connected_at {
        spans.push(Span::styled(
            humanize_since(ts),
            Style::new().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

fn draw_confirm(frame: &mut Frame, label: &str) {
    let area = centered_rect(frame.area(), 60, 5);
    frame.render_widget(Clear, area);
    let body = Paragraph::new(vec![
        Line::from(""),
        Line::from(format!("  Delete {label}?")),
        Line::from(Span::styled("  y / n", Style::new().fg(Color::DarkGray))),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(Color::Red))
            .title(" Confirm "),
    );
    frame.render_widget(body, area);
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Coarse "how long ago", good enough for a one-line hint.
pub fn humanize_since(ts: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - ts).num_seconds();
    if secs < 0 {
        return "just now".into();
    }
    let (n, unit) = match secs {
        s if s < 60 => return "just now".into(),
        s if s < 3_600 => (s / 60, "m"),
        s if s < 86_400 => (s / 3_600, "h"),
        s if s < 2_592_000 => (s / 86_400, "d"),
        s if s < 31_536_000 => (s / 2_592_000, "mo"),
        s => (s / 31_536_000, "y"),
    };
    format!("{n}{unit} ago")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn store_with(names: &[(&str, &str)]) -> Store {
        let mut store = Store::default();
        for (name, host) in names {
            store.insert_unique(Connection::new(*name, *host));
        }
        store
    }

    #[test]
    fn an_empty_query_returns_everything_in_recency_order() {
        let mut store = store_with(&[("web", "w.example"), ("db", "d.example")]);
        store.get_mut("db").unwrap().last_connected_at = Some(chrono::Utc::now());
        let mut matcher = Matcher::new(Config::DEFAULT);
        let got: Vec<&str> = filter(&store, "", &mut matcher)
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(got, ["db", "web"]);
        assert_eq!(filter(&store, "   ", &mut matcher).len(), 2);
    }

    #[test]
    fn the_query_matches_name_host_and_tags() {
        let mut store = store_with(&[("Prod DB", "db.example.com"), ("staging web", "web.stg")]);
        store.get_mut("prod-db").unwrap().tags = vec!["critical".into()];
        let mut matcher = Matcher::new(Config::DEFAULT);

        let by_name: Vec<&str> = filter(&store, "prod", &mut matcher)
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(by_name, ["prod-db"]);

        let by_host: Vec<&str> = filter(&store, "web.stg", &mut matcher)
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(by_host, ["staging-web"]);

        let by_tag: Vec<&str> = filter(&store, "critical", &mut matcher)
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(by_tag, ["prod-db"]);
    }

    #[test]
    fn matching_is_fuzzy_and_case_insensitive() {
        let store = store_with(&[("Production Database", "db.example.com")]);
        let mut matcher = Matcher::new(Config::DEFAULT);
        assert_eq!(filter(&store, "proddb", &mut matcher).len(), 1);
        assert_eq!(filter(&store, "PRODUCTION", &mut matcher).len(), 1);
    }

    #[test]
    fn a_query_that_matches_nothing_returns_nothing() {
        let store = store_with(&[("web", "w.example")]);
        let mut matcher = Matcher::new(Config::DEFAULT);
        assert!(filter(&store, "zzzzqqq", &mut matcher).is_empty());
    }

    #[test]
    fn humanize_since_uses_coarse_units() {
        let now = chrono::Utc::now();
        assert_eq!(humanize_since(now), "just now");
        assert_eq!(humanize_since(now - ChronoDuration::minutes(5)), "5m ago");
        assert_eq!(humanize_since(now - ChronoDuration::hours(3)), "3h ago");
        assert_eq!(humanize_since(now - ChronoDuration::days(2)), "2d ago");
        assert_eq!(humanize_since(now - ChronoDuration::days(90)), "3mo ago");
        assert_eq!(humanize_since(now - ChronoDuration::days(800)), "2y ago");
        // A clock that jumped backwards must not render a negative age.
        assert_eq!(humanize_since(now + ChronoDuration::hours(1)), "just now");
    }

    #[test]
    fn the_footer_fits_the_width_it_is_given() {
        // Wide enough for everything.
        let wide = footer_hints(100, 6);
        assert!(line_width(&wide) <= 100);
        let text: String = wide.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("^E edit"), "missing the edit hint: {text}");
        assert!(text.contains("6 saved"));

        // A 70%-of-100-columns popup: the full line does not fit, so the short one is used
        // and still names every key.
        let narrow = footer_hints(70, 6);
        assert!(
            line_width(&narrow) <= 70,
            "footer is {} wide in a 70-column pane",
            line_width(&narrow)
        );
        let text: String = narrow.iter().map(|s| s.content.as_ref()).collect();
        for key in ["^A", "^E", "^D", "esc"] {
            assert!(text.contains(key), "narrow footer lost {key}: {text}");
        }
    }

    #[test]
    fn truncate_keeps_short_strings_and_ellipsises_long_ones() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 10), "abcdefghij");
        assert_eq!(truncate("abcdefghijk", 10), "abcdefghi…");
    }

    #[test]
    fn centered_rect_fits_inside_a_small_area() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 3,
        };
        let r = centered_rect(area, 60, 5);
        assert!(r.width <= area.width && r.height <= area.height);
        assert!(r.x + r.width <= area.x + area.width);
        assert!(r.y + r.height <= area.y + area.height);
    }
}

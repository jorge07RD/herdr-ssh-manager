//! A small parser for `~/.ssh/config`, used by `herdr-ssh-manager import`.
//!
//! This deliberately understands only the handful of keywords that map onto a saved
//! connection. Anything else in the file is ignored, and pattern entries (`Host *`,
//! `Host web-?`) are skipped because they describe defaults rather than a destination.
//! `Include` is not followed: importing is a one-off convenience, not a reimplementation
//! of ssh's config resolution.

use crate::model::Connection;

/// One `Host` block, reduced to what we store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshConfigHost {
    /// The alias as written after `Host`.
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<String>,
    pub proxy_jump: Option<String>,
}

impl SshConfigHost {
    fn new(alias: String) -> Self {
        Self {
            alias,
            hostname: None,
            user: None,
            port: None,
            identity_file: None,
            proxy_jump: None,
        }
    }

    /// The host ssh would actually dial: `HostName` when present, else the alias.
    pub fn effective_host(&self) -> &str {
        self.hostname.as_deref().unwrap_or(&self.alias)
    }

    pub fn to_connection(&self) -> Connection {
        let mut conn = Connection::new(self.alias.clone(), self.effective_host().to_string());
        conn.user = self.user.clone();
        conn.port = self.port.unwrap_or(22);
        conn.identity_file = self.identity_file.clone();
        conn.jump_host = self.proxy_jump.clone();
        conn.tags = vec!["ssh-config".to_string()];
        conn
    }
}

/// Parse the importable `Host` blocks out of an ssh_config document.
pub fn parse(text: &str) -> Vec<SshConfigHost> {
    let mut hosts: Vec<SshConfigHost> = Vec::new();
    // A `Host` line may declare several aliases; they share the block's settings.
    let mut current: Vec<usize> = Vec::new();

    for line in text.lines() {
        let Some((keyword, value)) = split_directive(line) else {
            continue;
        };
        let keyword = keyword.to_ascii_lowercase();

        if keyword == "host" {
            current.clear();
            for alias in value.split_whitespace() {
                // `!pattern` negations and globs describe rules, not destinations.
                if is_pattern(alias) {
                    continue;
                }
                current.push(hosts.len());
                hosts.push(SshConfigHost::new(alias.to_string()));
            }
            continue;
        }
        // A `Match` block ends the applicability of the current Host block.
        if keyword == "match" {
            current.clear();
            continue;
        }
        if current.is_empty() || value.is_empty() {
            continue;
        }

        for &idx in &current {
            let host = &mut hosts[idx];
            match keyword.as_str() {
                "hostname" => host.hostname.get_or_insert_with(|| value.to_string()),
                "user" => host.user.get_or_insert_with(|| value.to_string()),
                "identityfile" => host
                    .identity_file
                    .get_or_insert_with(|| unquote(value).to_string()),
                "proxyjump" => host.proxy_jump.get_or_insert_with(|| value.to_string()),
                "port" => {
                    if let Ok(port) = value.parse::<u16>() {
                        host.port.get_or_insert(port);
                    }
                    continue;
                }
                _ => continue,
            };
        }
    }

    hosts
}

/// Split `Keyword value` or `Keyword = value`, skipping comments and blank lines.
fn split_directive(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let idx = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let (keyword, rest) = line.split_at(idx);
    let value = rest.trim_start().trim_start_matches('=').trim();
    Some((keyword, value))
}

fn is_pattern(alias: &str) -> bool {
    alias.starts_with('!') || alias.contains('*') || alias.contains('?')
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

/// Default path of the user's ssh config.
pub fn default_ssh_config_path() -> Option<std::path::PathBuf> {
    crate::config::home_dir().map(|h| h.join(".ssh").join("config"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/ssh_config_sample");

    fn find<'a>(hosts: &'a [SshConfigHost], alias: &str) -> &'a SshConfigHost {
        hosts
            .iter()
            .find(|h| h.alias == alias)
            .unwrap_or_else(|| panic!("no host {alias:?} in {hosts:#?}"))
    }

    #[test]
    fn parses_a_basic_block() {
        let hosts = parse(FIXTURE);
        let web = find(&hosts, "web");
        assert_eq!(web.hostname.as_deref(), Some("web01.example.com"));
        assert_eq!(web.user.as_deref(), Some("deploy"));
        assert_eq!(web.port, Some(2222));
        assert_eq!(web.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let hosts = parse(FIXTURE);
        assert!(hosts.iter().all(|h| !h.alias.starts_with('#')));
        assert!(hosts.iter().all(|h| !h.alias.is_empty()));
    }

    #[test]
    fn wildcard_and_negated_hosts_are_skipped() {
        let hosts = parse(FIXTURE);
        for alias in ["*", "!prod", "web-?", "*.internal"] {
            assert!(
                !hosts.iter().any(|h| h.alias == alias),
                "pattern {alias:?} should not be importable"
            );
        }
    }

    #[test]
    fn one_host_line_with_several_aliases_yields_one_entry_each() {
        let hosts = parse(FIXTURE);
        let a = find(&hosts, "db-a");
        let b = find(&hosts, "db-b");
        assert_eq!(a.user.as_deref(), Some("postgres"));
        assert_eq!(b.user.as_deref(), Some("postgres"));
        assert_eq!(a.hostname.as_deref(), Some("db.example.com"));
        assert_eq!(b.hostname.as_deref(), Some("db.example.com"));
    }

    #[test]
    fn keywords_are_case_insensitive_and_accept_equals() {
        let hosts = parse(FIXTURE);
        let odd = find(&hosts, "oddly-formatted");
        assert_eq!(odd.hostname.as_deref(), Some("odd.example.com"));
        assert_eq!(odd.user.as_deref(), Some("root"));
        assert_eq!(odd.port, Some(23));
    }

    #[test]
    fn proxy_jump_is_captured() {
        let hosts = parse(FIXTURE);
        assert_eq!(
            find(&hosts, "behind-bastion").proxy_jump.as_deref(),
            Some("bastion.example.com")
        );
    }

    #[test]
    fn the_first_value_of_a_repeated_keyword_wins_as_ssh_does() {
        let hosts = parse(FIXTURE);
        assert_eq!(find(&hosts, "repeated").user.as_deref(), Some("first"));
    }

    #[test]
    fn a_host_without_hostname_dials_its_alias() {
        let hosts = parse(FIXTURE);
        let bare = find(&hosts, "bare");
        assert!(bare.hostname.is_none());
        assert_eq!(bare.effective_host(), "bare");
        assert_eq!(bare.to_connection().host, "bare");
    }

    #[test]
    fn settings_after_a_match_block_are_not_attributed_to_the_last_host() {
        let hosts = parse(FIXTURE);
        // `after-match` is followed by `Match ...` then `User nobody`.
        assert_eq!(find(&hosts, "after-match").user.as_deref(), None);
    }

    #[test]
    fn converting_carries_the_fields_over_and_tags_the_source() {
        let conn = find(&parse(FIXTURE), "web").to_connection();
        assert_eq!(conn.name, "web");
        assert_eq!(conn.host, "web01.example.com");
        assert_eq!(conn.user.as_deref(), Some("deploy"));
        assert_eq!(conn.port, 2222);
        assert_eq!(conn.identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
        assert_eq!(conn.tags, vec!["ssh-config".to_string()]);
    }

    #[test]
    fn a_host_with_no_settings_at_all_still_parses() {
        let hosts = parse("Host solo\n");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].effective_host(), "solo");
    }

    #[test]
    fn an_empty_document_yields_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("# only a comment\n\n   \n").is_empty());
    }
}

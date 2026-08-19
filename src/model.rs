//! The saved-connection data model, as persisted in `connections.toml`.

use serde::{Deserialize, Serialize};

fn default_port() -> u16 {
    22
}

fn is_default_port(port: &u16) -> bool {
    *port == 22
}

/// One saved SSH destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// Stable slug, unique within the store. Used by `edit`/`remove`.
    pub id: String,
    /// Human label shown in the picker.
    pub name: String,
    pub host: String,
    #[serde(default = "default_port", skip_serializing_if = "is_default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// `-i` — may contain a leading `~`, expanded only when building the ssh argv.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    /// `-J` / ProxyJump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump_host: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Extra flags appended verbatim to the ssh argv.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_ssh_args: Vec<String>,
    /// RFC 3339 timestamp, refreshed right before handing over to ssh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Connection {
    pub fn new(name: impl Into<String>, host: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            id: slugify(&name),
            name,
            host: host.into(),
            port: default_port(),
            user: None,
            identity_file: None,
            jump_host: None,
            tags: Vec::new(),
            extra_ssh_args: Vec::new(),
            last_connected_at: None,
            notes: None,
        }
    }

    /// `user@host:port`, with the parts that are defaults left out.
    pub fn destination(&self) -> String {
        let mut s = String::new();
        if let Some(user) = &self.user {
            s.push_str(user);
            s.push('@');
        }
        s.push_str(&self.host);
        if self.port != 22 {
            s.push(':');
            s.push_str(&self.port.to_string());
        }
        s
    }

    /// The haystack the fuzzy matcher scores against.
    pub fn search_text(&self) -> String {
        let mut s = format!("{} {}", self.name, self.destination());
        for tag in &self.tags {
            s.push(' ');
            s.push_str(tag);
        }
        s
    }
}

/// The whole `connections.toml` document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Store {
    #[serde(default, rename = "connection")]
    pub connections: Vec<Connection>,
}

impl Store {
    pub fn get(&self, id: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Connection> {
        self.connections.iter_mut().find(|c| c.id == id)
    }

    /// Assign `conn` an id derived from its name that no other entry holds,
    /// then append it. Returns the id that was used.
    pub fn insert_unique(&mut self, mut conn: Connection) -> String {
        let base = slugify(&conn.name);
        let mut candidate = base.clone();
        let mut n = 2;
        while self.connections.iter().any(|c| c.id == candidate) {
            candidate = format!("{base}-{n}");
            n += 1;
        }
        conn.id = candidate.clone();
        self.connections.push(conn);
        candidate
    }

    /// Remove by id; returns the entry that was dropped.
    pub fn remove(&mut self, id: &str) -> Option<Connection> {
        let idx = self.connections.iter().position(|c| c.id == id)?;
        Some(self.connections.remove(idx))
    }

    /// Most recently connected first; never-connected entries keep their
    /// insertion order at the end.
    pub fn sorted_by_recency(&self) -> Vec<&Connection> {
        let mut v: Vec<&Connection> = self.connections.iter().collect();
        v.sort_by_key(|c| std::cmp::Reverse(c.last_connected_at));
        v
    }
}

/// Lowercase ASCII slug: runs of non-alphanumerics collapse to a single `-`.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        "connection".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_collapses_and_lowercases() {
        assert_eq!(slugify("Prod DB"), "prod-db");
        assert_eq!(slugify("  web//01  "), "web-01");
        // Non-ASCII characters act as separators rather than being transliterated.
        assert_eq!(slugify("Ünïcode"), "n-code");
        assert_eq!(slugify("Café Prod"), "caf-prod");
        assert_eq!(slugify("!!!"), "connection");
    }

    #[test]
    fn insert_unique_disambiguates_colliding_names() {
        let mut store = Store::default();
        assert_eq!(
            store.insert_unique(Connection::new("Prod DB", "a")),
            "prod-db"
        );
        assert_eq!(
            store.insert_unique(Connection::new("Prod DB", "b")),
            "prod-db-2"
        );
        assert_eq!(
            store.insert_unique(Connection::new("prod db", "c")),
            "prod-db-3"
        );
        assert_eq!(store.connections.len(), 3);
    }

    #[test]
    fn destination_omits_defaults() {
        let mut c = Connection::new("web", "example.com");
        assert_eq!(c.destination(), "example.com");
        c.user = Some("root".into());
        assert_eq!(c.destination(), "root@example.com");
        c.port = 2222;
        assert_eq!(c.destination(), "root@example.com:2222");
    }

    #[test]
    fn remove_returns_the_dropped_entry() {
        let mut store = Store::default();
        store.insert_unique(Connection::new("a", "a.example"));
        store.insert_unique(Connection::new("b", "b.example"));
        assert_eq!(store.remove("a").unwrap().host, "a.example");
        assert!(store.remove("a").is_none());
        assert_eq!(store.connections.len(), 1);
    }

    #[test]
    fn sorted_by_recency_puts_newest_first() {
        let mut store = Store::default();
        store.insert_unique(Connection::new("old", "o"));
        store.insert_unique(Connection::new("never", "n"));
        store.insert_unique(Connection::new("new", "w"));
        store.get_mut("old").unwrap().last_connected_at =
            Some("2024-01-01T00:00:00Z".parse().unwrap());
        store.get_mut("new").unwrap().last_connected_at =
            Some("2025-01-01T00:00:00Z".parse().unwrap());
        let order: Vec<&str> = store
            .sorted_by_recency()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(order, ["new", "old", "never"]);
    }
}

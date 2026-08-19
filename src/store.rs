//! Loading and saving `connections.toml`.
//!
//! Saves are atomic: the new document is written to a sibling temp file, flushed to
//! disk, and renamed over the target. A crash (or Herdr killing the popup mid-write)
//! therefore leaves either the old file or the new one, never a truncated mix.

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config;
use crate::model::Store;

pub struct StoreFile {
    path: PathBuf,
}

/// What `load` had to do to produce a usable store.
#[derive(Debug, PartialEq, Eq)]
pub enum LoadOutcome {
    /// File parsed, or did not exist yet.
    Clean,
    /// File was unparseable and was moved aside; the store starts empty.
    RecoveredFromCorrupt { backup: PathBuf, error: String },
}

impl StoreFile {
    /// The store at the plugin's configured location.
    pub fn discover() -> Result<Self> {
        Ok(Self::at(
            config::config_dir()?.join(config::CONNECTIONS_FILE),
        ))
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the store, quarantining the file if it cannot be parsed.
    pub fn load(&self) -> Result<(Store, LoadOutcome)> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Store::default(), LoadOutcome::Clean))
            }
            Err(e) => {
                return Err(e).with_context(|| format!("could not read {}", self.path.display()))
            }
        };

        match toml::from_str::<Store>(&raw) {
            Ok(store) => Ok((store, LoadOutcome::Clean)),
            Err(error) => {
                let backup = self.quarantine()?;
                Ok((
                    Store::default(),
                    LoadOutcome::RecoveredFromCorrupt {
                        backup,
                        error: error.to_string(),
                    },
                ))
            }
        }
    }

    /// Load and print a warning to stderr if the previous file had to be moved aside.
    pub fn load_reporting(&self) -> Result<Store> {
        let (store, outcome) = self.load()?;
        if let LoadOutcome::RecoveredFromCorrupt { backup, error } = outcome {
            eprintln!(
                "herdr-ssh-manager: {} could not be parsed ({error}).\n\
                 herdr-ssh-manager: it was saved as {} and a fresh, empty store was started.",
                self.path.display(),
                backup.display()
            );
        }
        Ok(store)
    }

    /// Move an unparseable file aside so the user can recover it by hand.
    fn quarantine(&self) -> Result<PathBuf> {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let mut backup = self.path.clone();
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| config::CONNECTIONS_FILE.to_string());
        backup.set_file_name(format!("{name}.corrupt-{stamp}"));
        fs::rename(&self.path, &backup).with_context(|| {
            format!(
                "could not move the unreadable {} aside to {}",
                self.path.display(),
                backup.display()
            )
        })?;
        Ok(backup)
    }

    /// Serialize and replace the file atomically.
    pub fn save(&self, store: &Store) -> Result<()> {
        let body = toml::to_string_pretty(store).context("could not serialize connections")?;
        let document = format!(
            "# Managed by herdr-ssh-manager. Hand edits are fine; keep it valid TOML.\n\
             # Docs: https://github.com/jorge07RD/herdr-ssh-manager\n\n{body}"
        );

        let dir = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;

        // Same directory as the target, so the rename below stays on one filesystem.
        let tmp = self.path.with_extension("toml.tmp");
        {
            let mut file = fs::File::create(&tmp)
                .with_context(|| format!("could not create {}", tmp.display()))?;
            file.write_all(document.as_bytes())
                .with_context(|| format!("could not write {}", tmp.display()))?;
            file.sync_all()
                .with_context(|| format!("could not flush {}", tmp.display()))?;
        }
        restrict_permissions(&tmp)?;

        fs::rename(&tmp, &self.path).with_context(|| {
            format!(
                "could not replace {} with {}",
                self.path.display(),
                tmp.display()
            )
        })?;
        Ok(())
    }
}

/// The file records usernames, hostnames and private-key paths: keep it owner-only.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Connection;

    fn sample() -> Store {
        let mut store = Store::default();
        let mut c = Connection::new("Prod DB", "db.example.com");
        c.user = Some("deploy".into());
        c.port = 2222;
        c.identity_file = Some("~/.ssh/id_ed25519".into());
        c.jump_host = Some("bastion.example.com".into());
        c.tags = vec!["prod".into(), "db".into()];
        c.extra_ssh_args = vec!["-o".into(), "ServerAliveInterval=30".into()];
        c.notes = Some("primary replica".into());
        c.last_connected_at = Some("2025-03-04T05:06:07Z".parse().unwrap());
        store.insert_unique(c);
        store.insert_unique(Connection::new("web", "web.example.com"));
        store
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = StoreFile::at(dir.path().join("connections.toml"));
        let original = sample();
        file.save(&original).unwrap();

        let (loaded, outcome) = file.load().unwrap();
        assert_eq!(outcome, LoadOutcome::Clean);
        assert_eq!(loaded, original);
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = StoreFile::at(dir.path().join("connections.toml"));
        let (store, outcome) = file.load().unwrap();
        assert!(store.connections.is_empty());
        assert_eq!(outcome, LoadOutcome::Clean);
    }

    #[test]
    fn port_defaults_to_22_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        fs::write(
            &path,
            "[[connection]]\nid = \"web\"\nname = \"web\"\nhost = \"example.com\"\n",
        )
        .unwrap();
        let (store, _) = StoreFile::at(&path).load().unwrap();
        assert_eq!(store.connections[0].port, 22);
    }

    #[test]
    fn default_port_is_not_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        let file = StoreFile::at(&path);
        let mut store = Store::default();
        store.insert_unique(Connection::new("web", "example.com"));
        file.save(&store).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("port"), "unexpected port key in:\n{text}");
    }

    #[test]
    fn corrupt_file_is_quarantined_and_the_store_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        fs::write(&path, "this is [not valid ::: toml").unwrap();

        let (store, outcome) = StoreFile::at(&path).load().unwrap();
        assert!(store.connections.is_empty());
        let LoadOutcome::RecoveredFromCorrupt { backup, .. } = outcome else {
            panic!("expected the corrupt file to be quarantined, got {outcome:?}");
        };
        assert!(backup.exists(), "backup {} is missing", backup.display());
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            "this is [not valid ::: toml"
        );
        assert!(
            !path.exists(),
            "the corrupt file should have been moved away"
        );
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let file = StoreFile::at(dir.path().join("connections.toml"));
        file.save(&sample()).unwrap();

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn an_interrupted_save_cannot_truncate_the_previous_file() {
        // A save that dies before the rename leaves a stray .tmp; the real file must
        // still hold the previous document in full.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        let file = StoreFile::at(&path);
        file.save(&sample()).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        fs::write(path.with_extension("toml.tmp"), "half-written garbage").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        let (store, outcome) = file.load().unwrap();
        assert_eq!(outcome, LoadOutcome::Clean);
        assert_eq!(store, sample());

        // And the next successful save clears the stray temp file.
        file.save(&store).unwrap();
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.toml");
        StoreFile::at(&path).save(&sample()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "unexpected mode {:o}", mode & 0o777);
    }
}

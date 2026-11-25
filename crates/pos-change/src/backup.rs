use crate::filesystem::{Layout, regular};
use crate::model::{conflict, hash, invalid};
use crate::store::Store;
use crate::{ChangeEngine, ChangeError, ExclusiveWrite};
use pos_vault::Vault;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;
use walkdir::WalkDir;

const DATABASE: &str = ".maggot/changes.sqlite";
const MANIFEST: &str = "manifest.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupManifest {
    pub version: u32,
    pub vault_id: Option<Uuid>,
    pub last_sequence: Option<i64>,
    entries: Vec<Entry>,
    identities: Vec<Identity>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    path: String,
    sha256: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Identity {
    path: String,
    id: String,
}

pub fn create_backup(
    root: &Path,
    destination: &Path,
    _exclusive: &ExclusiveWrite,
) -> Result<BackupManifest, ChangeError> {
    let engine = ChangeEngine::open(root)?;
    let destination = new_destination(destination, &engine.root)?;
    let layout = &engine.layout;
    let _lock = if layout.has_database()? {
        Some(layout.lock()?)
    } else {
        None
    };
    let store = if _lock.is_some() {
        Store::open(layout, false)?
    } else {
        None
    };
    if let Some(store) = &store
        && let Some(pending) = store.pending()?.first()
    {
        return Err(ChangeError::Pending(pending.request.change_id));
    }
    let initial = inventory(&engine.root)?;
    if store.is_none() && initial.iter().any(|entry| entry.path == DATABASE) {
        return Err(conflict(
            "change storage appeared while the capture was starting",
        ));
    }
    let staging = tempfile::tempdir_in(
        destination
            .parent()
            .ok_or_else(|| invalid("invalid backup destination"))?,
    )?;
    let tree = staging.path().join("vault");
    fs::create_dir(&tree)?;
    copy_entries(&engine.root, &tree, &initial, store.is_some())?;
    if let Some(store) = &store {
        let mut copy = Connection::open(tree.join(DATABASE))?;
        let backup = rusqlite::backup::Backup::new(&store.db, &mut copy)?;
        backup.run_to_completion(64, std::time::Duration::from_millis(5), None)?;
    }
    if initial != inventory(&engine.root)? {
        return Err(conflict(
            "vault changed while the backup was being captured",
        ));
    }
    let entries = inventory(&tree)?;
    if !initial
        .iter()
        .filter(|e| e.path != DATABASE)
        .eq(entries.iter().filter(|e| e.path != DATABASE))
    {
        return Err(conflict(
            "copied vault bytes differ from the capture inventory",
        ));
    }
    let identities = if store.is_some() {
        engine
            .vault
            .scan()?
            .list()?
            .into_iter()
            .map(|note| Identity {
                path: note.path,
                id: note.id,
            })
            .collect()
    } else {
        Vec::new()
    };
    if initial != inventory(&engine.root)? {
        return Err(conflict(
            "vault changed while its identities were being captured",
        ));
    }
    let last_sequence = store
        .as_ref()
        .map(|store| sequence(&store.db))
        .transpose()?
        .flatten();
    let manifest = BackupManifest {
        version: 1,
        vault_id: store.as_ref().map(|store| store.vault_id),
        last_sequence,
        entries,
        identities,
    };
    let mut file = File::create(staging.path().join(MANIFEST))?;
    serde_json::to_writer_pretty(&mut file, &manifest)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    verify_backup(staging.path())?;
    fs::create_dir(&destination)?;
    fs::rename(tree, destination.join("vault"))?;
    fs::rename(staging.path().join(MANIFEST), destination.join(MANIFEST))?;
    Ok(manifest)
}

pub fn verify_backup(snapshot: &Path) -> Result<BackupManifest, ChangeError> {
    if !regular(snapshot, true)? || !regular(&snapshot.join(MANIFEST), false)? {
        return Err(invalid("backup has no manifest"));
    }
    let tree = snapshot.join("vault");
    if !regular(&tree, true)? {
        return Err(invalid("backup has no vault tree"));
    }
    let manifest: BackupManifest = serde_json::from_reader(File::open(snapshot.join(MANIFEST))?)?;
    if manifest.version != 1 || manifest.entries != inventory(&tree)? {
        return Err(ChangeError::Corrupt("backup inventory or checksums".into()));
    }
    match Store::open(&Layout::new(&tree), false)? {
        Some(store) => {
            if manifest.vault_id != Some(store.vault_id)
                || manifest.last_sequence != sequence(&store.db)?
                || !store.pending()?.is_empty()
            {
                return Err(ChangeError::Corrupt("backup history boundary".into()));
            }
            let integrity: String = store
                .db
                .pragma_query_value(None, "quick_check", |row| row.get(0))?;
            let has_bad_reference: bool = store.db.query_row(
                "SELECT EXISTS (SELECT 1 FROM pragma_foreign_key_check)",
                [],
                |row| row.get(0),
            )?;
            if integrity != "ok" || has_bad_reference {
                return Err(ChangeError::Corrupt("backup SQLite integrity".into()));
            }
            let mut query = store.db.prepare("SELECT hash, bytes FROM content_blobs")?;
            let mut rows = query.query([])?;
            while let Some(row) = rows.next()? {
                let digest: String = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                if hash(&bytes) != digest {
                    return Err(ChangeError::Corrupt(format!("backup blob {digest}")));
                }
            }
        }
        None if manifest.vault_id.is_none()
            && manifest.last_sequence.is_none()
            && manifest.identities.is_empty() => {}
        None => return Err(ChangeError::Corrupt("backup has no change store".into())),
    }
    Ok(manifest)
}

pub fn restore_backup(snapshot: &Path, destination: &Path) -> Result<BackupManifest, ChangeError> {
    let snapshot = fs::canonicalize(snapshot)?;
    let destination = new_destination(destination, &snapshot)?;
    let manifest = verify_backup(&snapshot)?;
    let staging = tempfile::tempdir_in(
        destination
            .parent()
            .ok_or_else(|| invalid("invalid restore destination"))?,
    )?;
    copy_entries(
        &snapshot.join("vault"),
        staging.path(),
        &manifest.entries,
        false,
    )?;
    if manifest.entries != inventory(staging.path())? {
        return Err(ChangeError::Corrupt(
            "restored vault differs from snapshot".into(),
        ));
    }
    if manifest.vault_id.is_some() {
        let vault = Vault::open(staging.path())?;
        let layout = Layout::new(staging.path());
        layout.initialize()?;
        let mut store = Store::open(&layout, true)?.ok_or(ChangeError::NotInitialized)?;
        let keys = manifest
            .identities
            .iter()
            .map(|identity| {
                let key = vault.file_key(Path::new(&identity.path))?;
                Ok((identity, key))
            })
            .collect::<Result<Vec<_>, ChangeError>>()?;
        let tx = store.db.transaction()?;
        for (identity, key) in keys {
            if !identity.id.starts_with("yaml:") {
                tx.execute("INSERT INTO note_bindings VALUES (?1, ?2) ON CONFLICT(file_key) DO UPDATE SET note_id = excluded.note_id", params![key, identity.id])?;
            }
            tx.execute(
                "UPDATE observed_notes SET file_key = ?1 WHERE note_id = ?2 AND path = ?3",
                params![key, identity.id, identity.path],
            )?;
        }
        tx.commit()?;
    }
    fs::create_dir(&destination)?;
    for entry in fs::read_dir(staging.path())? {
        let entry = entry?;
        fs::rename(entry.path(), destination.join(entry.file_name()))?;
    }
    Ok(manifest)
}

fn sequence(db: &Connection) -> Result<Option<i64>, ChangeError> {
    Ok(db.query_row("SELECT MAX(seq) FROM changes", [], |row| row.get(0))?)
}

fn new_destination(path: &Path, source: &Path) -> Result<PathBuf, ChangeError> {
    let name = path
        .file_name()
        .ok_or_else(|| invalid("destination must name a new directory"))?;
    if name
        .to_str()
        .is_none_or(|name| name.contains([':', '\\', '/']) || name.ends_with(['.', ' ']))
    {
        return Err(invalid("destination must be a portable directory name"));
    }
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)?;
    let full = parent.join(name);
    if full.starts_with(source) || fs::symlink_metadata(&full).is_ok() {
        return Err(invalid(
            "destination must be a new directory outside the source",
        ));
    }
    Ok(full)
}

fn inventory(root: &Path) -> Result<Vec<Entry>, ChangeError> {
    let mut entries = Vec::new();
    let walk = WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .filter_entry(|entry| {
            let relative = entry.path().strip_prefix(root).expect("walk entry");
            if entry.file_name().eq_ignore_ascii_case(".git") {
                return false;
            }
            ![
                "write.lock",
                "changes.sqlite-journal",
                "changes.sqlite-wal",
                "changes.sqlite-shm",
            ]
            .iter()
            .any(|item| relative == Path::new(".maggot").join(item))
        });
    for entry in walk {
        let entry = entry.map_err(io::Error::other)?;
        let relative = entry.path().strip_prefix(root).expect("walk entry");
        let mut parts = Vec::new();
        for component in relative.components() {
            let Component::Normal(part) = component else {
                return Err(invalid("invalid backup path"));
            };
            parts.push(
                part.to_str()
                    .filter(|part| !part.contains([':', '\\', '/']))
                    .ok_or_else(|| ChangeError::UnsafePath(entry.path().into()))?,
            );
        }
        let directory = entry.file_type().is_dir();
        if !regular(entry.path(), directory)? {
            return Err(ChangeError::UnsafePath(entry.path().into()));
        }
        entries.push(Entry {
            path: parts.join("/"),
            sha256: (!directory).then(|| file_hash(entry.path())).transpose()?,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn file_hash(path: &Path) -> Result<String, ChangeError> {
    let mut reader = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(encoded, "{byte:02x}").expect("writing to a String succeeds");
    }
    Ok(encoded)
}

fn copy_entries(
    source: &Path,
    destination: &Path,
    entries: &[Entry],
    skip_database: bool,
) -> Result<(), ChangeError> {
    for entry in entries {
        let target = destination.join(&entry.path);
        if entry.sha256.is_none() {
            fs::create_dir(target)?;
        } else if !skip_database || entry.path != DATABASE {
            let from = source.join(&entry.path);
            if !regular(&from, false)? {
                return Err(ChangeError::UnsafePath(from));
            }
            fs::copy(from, target)?;
        }
    }
    Ok(())
}

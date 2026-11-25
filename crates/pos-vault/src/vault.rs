use crate::{Note, VaultIndex, VaultWatch, note};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Serialize;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;
use walkdir::WalkDir;

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("vault root is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("path is not an included vault-relative Markdown file: {0}")]
    InvalidNotePath(PathBuf),
    #[error("{path}: {message}")]
    InvalidNote { path: String, message: String },
    #[error("could not load .obsidianignore: {0}")]
    Ignore(#[from] ignore::Error),
    #[error("vault index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("vault metadata: {0}")]
    Metadata(#[from] serde_json::Error),
    #[error("vault watcher: {0}")]
    Watch(#[from] notify::Error),
    #[error("vault watcher disconnected")]
    WatchDisconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    UnreadableFile,
    InvalidNote,
    DuplicateId,
    UnresolvedLink,
    AmbiguousLink,
}

#[derive(Clone, Debug, Serialize)]
pub struct VaultIssue {
    pub kind: IssueKind,
    pub path: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct VaultInfo {
    pub root: PathBuf,
    pub obsidian_config: bool,
}

#[derive(Debug)]
pub struct NoteSource {
    pub absolute_path: PathBuf,
    pub metadata: crate::NoteMetadata,
    pub text: String,
}

impl NoteSource {
    pub fn validate_edit(&self, text: &str) -> Result<(), VaultError> {
        let path = &self.metadata.path;
        let edited =
            note::parse(path.clone(), text).map_err(|message| VaultError::InvalidNote {
                path: path.clone(),
                message,
            })?;
        if edited.metadata.id != self.metadata.id {
            return Err(VaultError::InvalidNote {
                path: path.clone(),
                message: "the edit changed the note identity".into(),
            });
        }
        Ok(())
    }
}

/// An Obsidian vault with no filesystem write operations.
pub struct Vault {
    root: PathBuf,
}

impl Vault {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, VaultError> {
        let root = root.as_ref();
        let canonical = fs::canonicalize(root).map_err(|source| io_error(root, source))?;
        if !canonical.is_dir() {
            return Err(VaultError::NotDirectory(canonical));
        }
        Ok(Self { root: canonical })
    }

    pub fn info(&self) -> VaultInfo {
        VaultInfo {
            root: self.root.clone(),
            obsidian_config: fs::symlink_metadata(self.root.join(".obsidian"))
                .is_ok_and(|metadata| metadata.is_dir() && !is_link(&metadata)),
        }
    }

    pub fn read(&self, relative: impl AsRef<Path>) -> Result<Note, VaultError> {
        let relative = relative.as_ref();
        let ignore = self.ignore_rules()?;
        self.read_note(relative, &ignore)
    }

    pub fn read_source(&self, relative: impl AsRef<Path>) -> Result<NoteSource, VaultError> {
        let (absolute_path, text, note) =
            self.load_note(relative.as_ref(), &self.ignore_rules()?)?;
        Ok(NoteSource {
            absolute_path,
            metadata: note.metadata,
            text,
        })
    }

    pub fn scan(&self) -> Result<VaultIndex, VaultError> {
        let ignore = self.ignore_rules()?;
        VaultIndex::build(self, &ignore)
    }

    pub fn watch(&self) -> Result<VaultWatch, VaultError> {
        VaultWatch::start(&self.root)
    }

    pub(crate) fn entries(
        &self,
        ignore: &Gitignore,
    ) -> impl Iterator<Item = Result<walkdir::DirEntry, walkdir::Error>> {
        WalkDir::new(&self.root)
            .follow_links(false)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| {
                let relative = self.relative(entry.path());
                !entry.path_is_symlink()
                    && included(ignore, relative, entry.file_type().is_dir())
                    && !entry.metadata().is_ok_and(|metadata| is_link(&metadata))
            })
    }

    pub(crate) fn relative<'a>(&self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.root).expect("path from vault walk")
    }

    pub(crate) fn read_note(
        &self,
        relative: &Path,
        ignore: &Gitignore,
    ) -> Result<Note, VaultError> {
        self.load_note(relative, ignore).map(|(_, _, note)| note)
    }

    fn load_note(
        &self,
        relative: &Path,
        ignore: &Gitignore,
    ) -> Result<(PathBuf, String, Note), VaultError> {
        if path_key(relative).is_none() || !is_note(relative) || !included(ignore, relative, false)
        {
            return Err(VaultError::InvalidNotePath(relative.into()));
        }
        let mut full = self.root.clone();
        for component in relative.components() {
            full.push(component);
            let metadata = fs::symlink_metadata(&full).map_err(|source| io_error(&full, source))?;
            if is_link(&metadata) {
                return Err(VaultError::InvalidNotePath(relative.into()));
            }
        }
        let canonical = fs::canonicalize(&full).map_err(|source| io_error(&full, source))?;
        if !canonical.starts_with(&self.root) || !canonical.is_file() {
            return Err(VaultError::InvalidNotePath(relative.into()));
        }
        let actual = self.relative(&canonical);
        let path = path_key(actual)
            .filter(|_| is_note(actual) && included(ignore, actual, false))
            .ok_or_else(|| VaultError::InvalidNotePath(relative.into()))?;
        let source = fs::read_to_string(&canonical).map_err(|source| io_error(&full, source))?;
        let note = note::parse(path.clone(), &source)
            .map_err(|message| VaultError::InvalidNote { path, message })?;
        Ok((canonical, source, note))
    }

    fn ignore_rules(&self) -> Result<Gitignore, VaultError> {
        let path = self.root.join(".obsidianignore");
        let mut builder = GitignoreBuilder::new(&self.root);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !is_link(&metadata) => {
                if let Some(error) = builder.add(&path) {
                    return Err(error.into());
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&path, source)),
        }
        Ok(builder.build()?)
    }
}

pub(crate) fn path_key(path: &Path) -> Option<String> {
    let parts = path
        .components()
        .map(|component| {
            let Component::Normal(part) = component else {
                return None;
            };
            part.to_str().filter(|part| !part.contains([':', '\\']))
        })
        .collect::<Option<Vec<_>>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

pub(crate) fn visible(path: &Path) -> bool {
    path.components().all(|component| {
        matches!(component, Component::Normal(name)
            if !name.to_string_lossy().starts_with('.'))
    })
}

fn included(ignore: &Gitignore, path: &Path, directory: bool) -> bool {
    visible(path)
        && !ignore
            .matched_path_or_any_parents(path, directory)
            .is_ignore()
}

pub(crate) fn is_note(path: &Path) -> bool {
    let attachment = path
        .components()
        .next()
        .is_some_and(|component| component.as_os_str().eq_ignore_ascii_case("Attachments"));
    !attachment
        && path
            .file_name()
            .is_some_and(|name| !name.eq_ignore_ascii_case("AGENTS.md"))
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

fn io_error(path: &Path, source: io::Error) -> VaultError {
    VaultError::Io {
        path: path.into(),
        source,
    }
}

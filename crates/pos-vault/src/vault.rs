use crate::{Note, VaultIndex, VaultWatch, identity, note};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Serialize;
use std::fs;
use std::io::{self, Read};
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
    UnresolvedAnchor,
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
    pub file_key: String,
}

impl NoteSource {
    pub fn validate_edit(&self, text: &str) -> Result<(), VaultError> {
        let path = &self.metadata.path;
        let edited =
            note::parse(path.clone(), text).map_err(|message| VaultError::InvalidNote {
                path: path.clone(),
                message,
            })?;
        let declared = |id: &str| id.starts_with("yaml:");
        if declared(&edited.metadata.id) != declared(&self.metadata.id)
            || (declared(&edited.metadata.id) && edited.metadata.id != self.metadata.id)
        {
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
        self.read_note(relative, &ignore, &identity::bindings(&self.root)?)
    }

    pub fn read_source(&self, relative: impl AsRef<Path>) -> Result<NoteSource, VaultError> {
        let (absolute_path, text, note, file_key) = self.load_note(
            relative.as_ref(),
            &self.ignore_rules()?,
            &identity::bindings(&self.root)?,
        )?;
        Ok(NoteSource {
            absolute_path,
            metadata: note.metadata,
            text,
            file_key,
        })
    }

    pub fn find_note(&self, id: &str) -> Result<Option<String>, VaultError> {
        let matches: Vec<_> = self
            .scan()?
            .list()?
            .into_iter()
            .filter(|note| note.id == id)
            .map(|note| note.path)
            .collect();
        match matches.as_slice() {
            [] => Ok(None),
            [path] => Ok(Some(path.clone())),
            _ => Err(VaultError::InvalidNote {
                path: matches.join(", "),
                message: format!("ambiguous note identity {id}"),
            }),
        }
    }

    pub fn file_key(&self, path: &Path) -> Result<String, VaultError> {
        let path = self.resolve_note_path(path, false)?;
        let file = fs::File::open(&path).map_err(|source| io_error(&path, source))?;
        identity::file_key(&file).map_err(|source| io_error(&path, source))
    }

    pub fn validate_new_note(
        &self,
        relative: &Path,
        text: &str,
    ) -> Result<(PathBuf, crate::NoteMetadata), VaultError> {
        let absolute = self.resolve_note_path(relative, true)?;
        let path = path_key(self.relative(&absolute))
            .ok_or_else(|| VaultError::InvalidNotePath(relative.into()))?;
        let note = note::parse(path.clone(), text)
            .map_err(|message| VaultError::InvalidNote { path, message })?;
        Ok((absolute, note.metadata))
    }

    pub fn scan(&self) -> Result<VaultIndex, VaultError> {
        let ignore = self.ignore_rules()?;
        VaultIndex::build(self, &ignore, &identity::bindings(&self.root)?)
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
        bindings: &identity::Bindings,
    ) -> Result<Note, VaultError> {
        self.load_note(relative, ignore, bindings)
            .map(|(_, _, note, _)| note)
    }

    fn load_note(
        &self,
        relative: &Path,
        ignore: &Gitignore,
        bindings: &identity::Bindings,
    ) -> Result<(PathBuf, String, Note, String), VaultError> {
        let canonical = self.note_path(relative, ignore, false)?;
        let path = path_key(self.relative(&canonical))
            .ok_or_else(|| VaultError::InvalidNotePath(relative.into()))?;
        let mut file = fs::File::open(&canonical).map_err(|source| io_error(&canonical, source))?;
        let key = identity::file_key(&file).map_err(|source| io_error(&canonical, source))?;
        let mut source = String::new();
        file.read_to_string(&mut source)
            .map_err(|source| io_error(&canonical, source))?;
        let mut note = note::parse(path.clone(), &source)
            .map_err(|message| VaultError::InvalidNote { path, message })?;
        if !note.metadata.id.starts_with("yaml:") {
            note.metadata.id = bindings
                .get(&key)
                .cloned()
                .unwrap_or_else(|| format!("file:{key}"));
        }
        Ok((canonical, source, note, key))
    }

    pub fn resolve_note_path(
        &self,
        relative: &Path,
        allow_missing: bool,
    ) -> Result<PathBuf, VaultError> {
        self.note_path(relative, &self.ignore_rules()?, allow_missing)
    }

    fn note_path(
        &self,
        relative: &Path,
        ignore: &Gitignore,
        allow_missing: bool,
    ) -> Result<PathBuf, VaultError> {
        if path_key(relative).is_none() || !is_note(relative) || !included(ignore, relative, false)
        {
            return Err(VaultError::InvalidNotePath(relative.into()));
        }
        let mut full = self.root.clone();
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            full.push(component);
            let metadata = match fs::symlink_metadata(&full) {
                Ok(metadata) => metadata,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && allow_missing
                        && components.peek().is_none() =>
                {
                    let parent = full
                        .parent()
                        .ok_or_else(|| VaultError::InvalidNotePath(relative.into()))?;
                    let parent =
                        fs::canonicalize(parent).map_err(|source| io_error(parent, source))?;
                    if !parent.starts_with(&self.root) {
                        return Err(VaultError::InvalidNotePath(relative.into()));
                    }
                    let candidate = parent.join(component.as_os_str());
                    if included(ignore, self.relative(&candidate), false) {
                        return Ok(candidate);
                    }
                    return Err(VaultError::InvalidNotePath(relative.into()));
                }
                Err(source) => return Err(io_error(&full, source)),
            };
            if is_link(&metadata) {
                return Err(VaultError::InvalidNotePath(relative.into()));
            }
        }
        let canonical = fs::canonicalize(&full).map_err(|source| io_error(&full, source))?;
        if !canonical.starts_with(&self.root) || !canonical.is_file() {
            return Err(VaultError::InvalidNotePath(relative.into()));
        }
        let actual = self.relative(&canonical);
        path_key(actual)
            .filter(|_| is_note(actual) && included(ignore, actual, false))
            .ok_or_else(|| VaultError::InvalidNotePath(relative.into()))?;
        Ok(canonical)
    }

    pub(crate) fn ignore_rules(&self) -> Result<Gitignore, VaultError> {
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

pub(crate) fn included(ignore: &Gitignore, path: &Path, directory: bool) -> bool {
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

pub(crate) fn is_link(metadata: &fs::Metadata) -> bool {
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

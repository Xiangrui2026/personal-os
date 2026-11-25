use crate::model::{ChangeError, conflict, hash};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) struct Layout {
    pub database: PathBuf,
    directory: PathBuf,
    staging: PathBuf,
}

impl Layout {
    pub fn new(root: &Path) -> Self {
        let directory = root.join(".maggot");
        Self {
            database: directory.join("changes.sqlite"),
            staging: directory.join("staging"),
            directory,
        }
    }

    pub fn initialize(&self) -> Result<(), ChangeError> {
        ensure_directory(&self.directory)?;
        ensure_directory(&self.staging)
    }

    pub fn has_database(&self) -> Result<bool, ChangeError> {
        if !regular(&self.directory, true)? {
            return Ok(false);
        }
        for sidecar in [
            "changes.sqlite-journal",
            "changes.sqlite-wal",
            "changes.sqlite-shm",
        ] {
            regular(&self.directory.join(sidecar), false)?;
        }
        regular(&self.database, false)
    }

    pub fn lock(&self) -> Result<File, ChangeError> {
        if !regular(&self.directory, true)? || !regular(&self.staging, true)? {
            return Err(ChangeError::NotInitialized);
        }
        let path = self.directory.join("write.lock");
        regular(&path, false)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        Ok(file)
    }

    pub fn replace(
        &self,
        id: Uuid,
        target: &Path,
        before_hash: &str,
        after: &[u8],
    ) -> Result<(), ChangeError> {
        let (pending, backup) = self.artifacts(id);
        if regular(&backup, false)? {
            return Err(conflict("a backup already exists for this execution"));
        }
        stage(&pending, after)?;

        let mut guard = note_guard(target)?;
        let mut guarded = Vec::new();
        guard.read_to_end(&mut guarded)?;
        if hash(&guarded) != before_hash || read_hash(target)? != before_hash {
            return Err(conflict("note changed during preparation"));
        }
        replace_file(target, &pending, &backup)?;
        let actual_backup = read_hash(&backup)?;
        if actual_backup != before_hash || read_hash(target)? != hash(after) {
            return Err(conflict(
                "replacement verification failed; preimage kept in staging",
            ));
        }
        Ok(())
    }

    pub fn create(&self, id: Uuid, target: &Path, after: &[u8]) -> Result<(), ChangeError> {
        let (pending, _) = self.artifacts(id);
        stage(&pending, after)?;
        fs::hard_link(&pending, target)?;
        if read_hash(target)? != hash(after) {
            return Err(conflict(
                "the created note no longer matches the approved bytes",
            ));
        }
        Ok(())
    }

    pub fn delete(&self, id: Uuid, target: &Path, before_hash: &str) -> Result<(), ChangeError> {
        let (_, backup) = self.artifacts(id);
        if regular(&backup, false)? {
            return Err(conflict("a backup already exists for this execution"));
        }
        let mut guard = note_guard(target)?;
        let mut guarded = Vec::new();
        guard.read_to_end(&mut guarded)?;
        if hash(&guarded) != before_hash || read_hash(target)? != before_hash {
            return Err(conflict("the note changed before deletion"));
        }
        fs::rename(target, &backup)?;
        if read_hash(&backup)? != before_hash || target.exists() {
            return Err(conflict(
                "deletion verification failed; preimage kept in staging",
            ));
        }
        Ok(())
    }

    pub fn backup_hash(&self, id: Uuid) -> Result<Option<String>, ChangeError> {
        if !regular(&self.directory, true)? || !regular(&self.staging, true)? {
            return Ok(None);
        }
        let (_, backup) = self.artifacts(id);
        regular(&backup, false)?
            .then(|| read_hash(&backup))
            .transpose()
    }

    pub fn cleanup(&self, id: Uuid) {
        let (pending, backup) = self.artifacts(id);
        for path in [pending, backup] {
            if matches!(regular(&path, false), Ok(true)) {
                let _ = fs::remove_file(path);
            }
        }
    }

    fn artifacts(&self, id: Uuid) -> (PathBuf, PathBuf) {
        (
            self.staging.join(format!("{id}.new")),
            self.staging.join(format!("{id}.before")),
        )
    }
}

fn ensure_directory(path: &Path) -> Result<(), ChangeError> {
    if !regular(path, true)? {
        match fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        if !regular(path, true)? {
            return Err(ChangeError::UnsafePath(path.into()));
        }
    }
    Ok(())
}

pub(crate) fn regular(path: &Path, directory: bool) -> Result<bool, ChangeError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    };
    #[cfg(not(windows))]
    let linked = metadata.file_type().is_symlink();
    let expected = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if linked || !expected {
        return Err(ChangeError::UnsafePath(path.into()));
    }
    Ok(true)
}

fn stage(path: &Path, bytes: &[u8]) -> Result<(), ChangeError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn read_hash(path: &Path) -> Result<String, ChangeError> {
    if !regular(path, false)? {
        return Err(ChangeError::UnsafePath(path.into()));
    }
    Ok(hash(&fs::read(path)?))
}

fn note_guard(path: &Path) -> Result<File, ChangeError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};
        // ReplaceFileW requires delete sharing. Concurrent writable handles are refused.
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
    }
    Ok(options.open(path)?)
}

#[cfg(windows)]
fn replace_file(target: &Path, next: &Path, backup: &Path) -> Result<(), ChangeError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain([0])
            .collect::<Vec<_>>()
    };
    let target = wide(target);
    let next = wide(next);
    let backup = wide(backup);
    // The buffers are live, NUL-terminated paths on the same volume.
    let success = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            next.as_ptr(),
            backup.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(target: &Path, next: &Path, backup: &Path) -> Result<(), ChangeError> {
    fs::set_permissions(next, fs::metadata(target)?.permissions())?;
    fs::hard_link(target, backup)?;
    fs::rename(next, target)?;
    Ok(())
}

use crate::{VaultError, vault::is_link};
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::HashMap,
    fs::{self, File},
    io,
    path::Path,
};

pub(crate) type Bindings = HashMap<String, String>;

pub(crate) fn bindings(root: &Path) -> Result<Bindings, VaultError> {
    let maggot = root.join(".maggot");
    if !fs::symlink_metadata(&maggot).is_ok_and(|m| m.is_dir() && !is_link(&m)) {
        return Ok(Bindings::new());
    }
    let file = maggot.join("changes.sqlite");
    if !fs::symlink_metadata(&file).is_ok_and(|m| m.is_file() && !is_link(&m)) {
        return Ok(Bindings::new());
    }
    let db = Connection::open_with_flags(
        file,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    let application: i64 = db.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if application != 0x4d414747 || version < 2 {
        return Ok(Bindings::new());
    }
    let mut query = db.prepare("SELECT file_key, note_id FROM note_bindings")?;
    let rows = query.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[cfg(windows)]
pub(crate) fn file_key(file: &File) -> io::Result<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // The handle belongs to the live File, and the output points to initialized storage.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let time = information.ftCreationTime;
    Ok(format!(
        "{:x}:{:08x}{:08x}:{:08x}{:08x}",
        information.dwVolumeSerialNumber,
        information.nFileIndexHigh,
        information.nFileIndexLow,
        time.dwHighDateTime,
        time.dwLowDateTime
    ))
}

#[cfg(unix)]
pub(crate) fn file_key(file: &File) -> io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    let created = metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |time| time.as_nanos());
    Ok(format!(
        "{:x}:{:x}:{created:x}",
        metadata.dev(),
        metadata.ino()
    ))
}

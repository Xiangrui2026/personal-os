use crate::VaultError;
use crate::vault::visible;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

#[derive(Debug, Serialize)]
pub struct VaultChanges {
    pub paths: Vec<PathBuf>,
    pub rescan: bool,
}

pub struct VaultWatch {
    _watcher: RecommendedWatcher,
    events: Receiver<notify::Result<Event>>,
    root: PathBuf,
}

impl VaultWatch {
    pub(crate) fn start(root: &Path) -> Result<Self, VaultError> {
        let (send, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(send)?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            events,
            root: root.into(),
        })
    }

    /// Block until an observed vault change, coalescing an initial burst of events.
    pub fn wait_for_change(&self) -> Result<(), VaultError> {
        self.wait_for_changes().map(|_| ())
    }

    pub fn wait_for_changes(&self) -> Result<VaultChanges, VaultError> {
        let mut paths = BTreeSet::new();
        let mut rescan = false;
        loop {
            let event = self
                .events
                .recv()
                .map_err(|_| VaultError::WatchDisconnected)??;
            self.collect(event, &mut paths, &mut rescan);
            if rescan || !paths.is_empty() {
                break;
            }
        }

        let deadline = Instant::now() + Duration::from_millis(250);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(remaining) {
                Ok(event) => {
                    self.collect(event?, &mut paths, &mut rescan);
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return Err(VaultError::WatchDisconnected),
            }
        }
        Ok(VaultChanges {
            paths: paths.into_iter().collect(),
            rescan,
        })
    }

    fn collect(&self, event: Event, paths: &mut BTreeSet<PathBuf>, rescan: &mut bool) {
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        *rescan |= event.need_rescan();
        for path in event.paths {
            if let Ok(relative) = path.strip_prefix(&self.root) {
                if relative == Path::new(".obsidianignore") || relative.as_os_str().is_empty() {
                    *rescan = true;
                } else if visible(relative) {
                    paths.insert(relative.into());
                    *rescan |= matches!(
                        event.kind,
                        EventKind::Create(notify::event::CreateKind::Folder)
                            | EventKind::Remove(notify::event::RemoveKind::Folder)
                    );
                }
            }
        }
    }
}

use crate::VaultError;
use crate::vault::visible;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

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
        loop {
            let event = self
                .events
                .recv()
                .map_err(|_| VaultError::WatchDisconnected)??;
            if self.affects_scan(event) {
                break;
            }
        }

        let deadline = Instant::now() + Duration::from_millis(250);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(remaining) {
                Ok(event) => {
                    event?;
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return Err(VaultError::WatchDisconnected),
            }
        }
        Ok(())
    }

    fn affects_scan(&self, event: Event) -> bool {
        !matches!(event.kind, EventKind::Access(_))
            && (event.need_rescan()
                || event.paths.iter().any(|path| {
                    path.strip_prefix(&self.root).is_ok_and(|relative| {
                        relative == Path::new(".obsidianignore") || visible(relative)
                    })
                }))
    }
}

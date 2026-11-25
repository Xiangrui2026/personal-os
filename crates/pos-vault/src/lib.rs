mod index;
mod note;
mod vault;
mod watch;

pub use index::{ScanReport, VaultIndex, VaultStats};
pub use note::{LocatedWikiLink, Note, NoteMarkup, NoteMetadata, WikiLink, inspect_markdown};
pub use vault::{IssueKind, NoteSource, Vault, VaultError, VaultInfo, VaultIssue};
pub use watch::VaultWatch;

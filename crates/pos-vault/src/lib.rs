mod identity;
mod index;
mod note;
mod property;
mod vault;
mod watch;

pub use index::{Backlink, LinkResolution, ScanReport, VaultIndex, VaultStats};
pub use note::{
    Block, Heading, LocatedWikiLink, Note, NoteMarkup, NoteMetadata, WikiLink, inspect_markdown,
};
pub use property::{PropertyEdit, prepare_property};
pub use vault::{IssueKind, NoteSource, Vault, VaultError, VaultInfo, VaultIssue};
pub use watch::{VaultChanges, VaultWatch};

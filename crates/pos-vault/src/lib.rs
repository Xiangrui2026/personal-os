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

pub fn parse_document(path: &str, text: &str) -> Result<Note, VaultError> {
    note::parse(path.to_owned(), text).map_err(|message| VaultError::InvalidNote {
        path: path.to_owned(),
        message,
    })
}

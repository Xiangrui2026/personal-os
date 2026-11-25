mod backup;
mod engine;
mod filesystem;
mod model;
mod observe;
mod operation;
mod store;

#[cfg(test)]
mod tests;

pub use backup::{BackupManifest, create_backup, restore_backup, verify_backup};
pub use engine::{ChangeEngine, ExclusiveWrite};
pub use model::{
    ChangeError, ChangePlan, ChangeRecord, ChangeRequest, ChangeStoreInfo, Operation, Patch,
    RecoveryOutcome, RecoveryStatus,
};
pub use observe::ObservationReport;
pub use operation::{OperationEdit, OperationHandler, OperationRegistry};

//! Administration entry points. Each workflow has one implementation.
mod legacy;
mod lifecycle;
mod maintenance;
mod placement;
mod recovery;
mod restore;
mod state;

pub use crate::state::lock;
pub use legacy::prepare_legacy;
pub use lifecycle::{
    active, ensure_clean_backup_source, ensure_stopped, maintenance_notice, reboot, restart, start,
    stop,
};
pub use maintenance::{
    MaintenanceRequest, auto_update, auto_update_with, capacity_preflight, check, daily,
    maintenance,
};
pub use placement::{import_legacy, init, init_with};
pub use recovery::{recover, start_check};
pub use restore::restore;
pub use state::{Transaction, TransactionKind, TransactionPhase};

pub(crate) use maintenance::{PENDING_RELEASE, REJECTED_RELEASE, SUCCESS};
pub(crate) use placement::{PLACEMENT, ensure_empty_destination, publish_initial_data};
pub(crate) use recovery::prepare_after_maintenance;
pub(crate) use state::{TRANSACTION, ensure_no_interruption};

//! openADE's own components, built on limn.
//!
//! Everything limn exports is re-exported here, so the application imports
//! its whole interface from one place and these modules read like the
//! library's own.

pub use limn::*;

mod approval_request;
mod context;
mod file_activity;
mod permission_selector;
mod tool_activity;

pub use approval_request::ApprovalRequest;
pub use context::{
    ContextContent, ContextMeter, ContextRow, ContextSegment, ContextUsage, ContextUsageRow,
};
pub use file_activity::{activity_body_inset, FileActivity};
pub use permission_selector::{PermissionOption, PermissionSelector};
pub use tool_activity::ToolActivity;

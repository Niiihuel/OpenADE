//! The app's own glyphs: actions limn has no icon for.

use openide_ui::Icon;

pub(crate) const STEP_OVER: Icon =
    Icon::Custom(include_str!("../assets/openide/debug-step-over.svg"));
pub(crate) const STEP_INTO: Icon =
    Icon::Custom(include_str!("../assets/openide/debug-step-into.svg"));
pub(crate) const STEP_OUT: Icon =
    Icon::Custom(include_str!("../assets/openide/debug-step-out.svg"));
pub(crate) const REPLACE: Icon = Icon::Custom(include_str!("../assets/openide/replace.svg"));
pub(crate) const REPLACE_ALL: Icon =
    Icon::Custom(include_str!("../assets/openide/replace-all.svg"));
pub(crate) const LOCK: Icon = Icon::Custom(include_str!("../assets/bootstrap/lock.svg"));
/// The marketplace, wherever it is offered.
pub(crate) const SHOP: Icon = Icon::Custom(include_str!("../assets/bootstrap/shop.svg"));
/// The debugger's activity, and its form while that activity is shown.
pub(crate) const BUG: Icon = Icon::Custom(include_str!("../assets/bootstrap/bug.svg"));
pub(crate) const BUG_FILL: Icon = Icon::Custom(include_str!("../assets/bootstrap/bug-fill.svg"));
/// Throw a file's changes away.
pub(crate) const DISCARD: Icon =
    Icon::Custom(include_str!("../assets/bootstrap/arrow-counterclockwise.svg"));
/// Exchange with the remote.
pub(crate) const SYNC: Icon = Icon::Custom(include_str!("../assets/bootstrap/arrow-repeat.svg"));

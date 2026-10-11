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

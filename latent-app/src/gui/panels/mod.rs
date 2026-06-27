//! The window's panels: the chrome (menu bar, toolbar, status bar) and the
//! right-hand controls. Each module owns one panel builder; `app::update` calls
//! them in declaration order, with the central canvas added last.

pub(crate) mod controls;
pub(crate) mod menubar;
pub(crate) mod statusbar;
pub(crate) mod toolbar;

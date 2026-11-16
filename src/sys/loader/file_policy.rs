//! Platform dependency shared by runtime search and File selection.

/// The 6.4.0.2 POSIX File library, including Standard SDK copies, imports
/// core entry points from the Advanced runtime by name. Opening it beside a
/// Standard core would load a second NDI implementation. The Windows File
/// library carries its own implementation and imports no core entry points.
#[cfg_attr(ndi_link_static, allow(dead_code))]
pub(crate) const FILE_LIBRARY_NEEDS_ADVANCED_RUNTIME: bool = cfg!(unix);

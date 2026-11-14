//! Give native test objects process-specific names.

/// `label` made unique to this process, for naming a sender or a router.
///
/// The NDI runtime takes a lock in the temporary directory named after each
/// sender. Two test processes that share that directory, such as two sessions
/// on one machine, would collide on a name that is the same in both.
pub fn unique_name(label: &str) -> String {
    format!("{label} {}", std::process::id())
}

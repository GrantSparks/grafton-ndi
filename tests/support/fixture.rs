//! An isolated temporary directory for test-owned files.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// A directory for one test run's fixtures.
///
/// Its name is taken exclusively, so runs that share a temporary directory,
/// even with the same process identifier in different namespaces, cannot use
/// or remove each other's files. It is removed when dropped, whether the test
/// passed or panicked.
pub struct FixtureDir(PathBuf);

impl FixtureDir {
    /// Create a directory no other run has, named after `purpose`.
    pub fn create(purpose: &str) -> Self {
        let mut attempt = 0u32;
        loop {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos());
            let path = std::env::temp_dir().join(format!(
                "grafton-ndi-{purpose}-{}-{nanos:09}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => attempt += 1,
                Err(error) => panic!("cannot create {}: {error}", path.display()),
            }
        }
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        // Nothing can be done about a failure here, and a leftover directory
        // under the temporary directory harms no later run.
        let _ = fs::remove_dir_all(&self.0);
    }
}

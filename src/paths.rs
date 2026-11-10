//! Portable storage paths.
//!
//! DirectCut RS keeps its update work in the folder that
//! contains the executable. It does not read or write AppData, the user profile,
//! or the system temporary directory.

use std::path::{Path, PathBuf};

/// Folder that contains the running executable.
pub fn root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Scratch space for update downloads and staging.
pub fn update_work() -> PathBuf {
    root().join(".freshen").join("work")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_paths_stay_in_the_executable_folder() {
        let executable = std::env::current_exe().unwrap();
        let expected = executable.parent().unwrap();
        let root = root();
        assert_eq!(root, expected);
        assert!(update_work().starts_with(&root));
    }
}

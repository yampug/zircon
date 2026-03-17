use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::WalkBuilder;
use log::info;

/// Directories to always skip, even if not in `.gitignore`.
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "target"];

/// A discovered Crystal source file.
#[derive(Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub modified: SystemTime,
    /// The Crystal project root this file belongs to (directory containing
    /// `shard.yml`), or `None` if no `shard.yml` was found above it.
    pub project_root: Option<PathBuf>,
    pub parsed: bool,
}

/// Represents a scanned workspace containing Crystal source files.
#[derive(Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub files: BTreeMap<PathBuf, FileEntry>,
    /// Directories that contain a `shard.yml` (Crystal project roots).
    pub project_roots: Vec<PathBuf>,
}

impl Workspace {
    /// Scan `root` recursively for `.cr` files, respecting `.gitignore` and
    /// skipping well-known non-source directories.
    pub fn scan(root: &Path) -> Self {
        let root = root.to_path_buf();
        let mut crystal_files: Vec<(PathBuf, SystemTime)> = Vec::new();
        let mut shard_dirs: Vec<PathBuf> = Vec::new();

        let walker = WalkBuilder::new(&root)
            .hidden(true) // skip hidden files/dirs
            .git_ignore(true) // respect .gitignore
            .filter_entry(|entry| {
                if entry.file_type().map_or(false, |ft| ft.is_dir()) {
                    let name = entry.file_name().to_string_lossy();
                    if SKIP_DIRS.contains(&name.as_ref()) {
                        return false;
                    }
                    // Skip `lib/` only when it's a Crystal shard dependency
                    // directory (has a sibling `shard.yml`).
                    if name == "lib" {
                        if let Some(parent) = entry.path().parent() {
                            if parent.join("shard.yml").exists() {
                                return false;
                            }
                        }
                    }
                }
                true
            })
            .build();

        for result in walker {
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if !entry.file_type().map_or(false, |ft| ft.is_file()) {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy();

            if file_name == "shard.yml" {
                if let Some(parent) = path.parent() {
                    shard_dirs.push(parent.to_path_buf());
                }
            }

            if path.extension().map_or(false, |ext| ext == "cr") {
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                crystal_files.push((path.to_path_buf(), modified));
            }
        }

        // Sort shard dirs by depth (deepest first) so that when we look up the
        // project root for a file we match the most specific ancestor.
        shard_dirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));

        let mut files = BTreeMap::new();
        for (path, modified) in crystal_files {
            let project_root = find_project_root(&path, &shard_dirs);
            files.insert(
                path.clone(),
                FileEntry {
                    path,
                    modified,
                    project_root,
                    parsed: false,
                },
            );
        }

        info!(
            "scanned workspace {:?}: {} crystal files, {} project roots",
            root,
            files.len(),
            shard_dirs.len()
        );

        Workspace {
            root,
            files,
            project_roots: shard_dirs,
        }
    }
}

/// Find the most-specific `shard.yml` directory that is an ancestor of `path`.
fn find_project_root(path: &Path, shard_dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in shard_dirs {
        if path.starts_with(dir) {
            return Some(dir.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper to create a temp directory with the given file tree.
    /// Paths ending with `/` create directories; others create files.
    fn create_tree(base: &Path, paths: &[&str]) {
        for p in paths {
            let full = base.join(p);
            if p.ends_with('/') {
                fs::create_dir_all(&full).unwrap();
            } else {
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent).unwrap();
                }
                fs::write(&full, "").unwrap();
            }
        }
    }

    #[test]
    fn test_monorepo_with_shard_yml() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &[
                "backend/shard.yml",
                "backend/src/app.cr",
                "backend/src/models/user.cr",
                "backend/spec/user_spec.cr",
                "frontend/index.html",
                "internal/README.md",
            ],
        );

        let ws = Workspace::scan(tmp.path());

        assert_eq!(ws.files.len(), 3);
        assert_eq!(ws.project_roots.len(), 1);
        assert_eq!(ws.project_roots[0], tmp.path().join("backend"));

        for (_, entry) in &ws.files {
            assert_eq!(
                entry.project_root.as_deref(),
                Some(tmp.path().join("backend").as_path())
            );
            assert!(!entry.parsed);
        }
    }

    #[test]
    fn test_standalone_crystal_project() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &["shard.yml", "src/main.cr", "src/lib/helper.cr", "spec/main_spec.cr"],
        );

        let ws = Workspace::scan(tmp.path());

        assert_eq!(ws.files.len(), 3);
        assert_eq!(ws.project_roots.len(), 1);
        assert_eq!(ws.project_roots[0], tmp.path().to_path_buf());
    }

    #[test]
    fn test_no_shard_yml_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &["src/app.cr", "src/models/user.cr"],
        );

        let ws = Workspace::scan(tmp.path());

        assert_eq!(ws.files.len(), 2);
        assert!(ws.project_roots.is_empty());

        // With no shard.yml, project_root should be None
        for (_, entry) in &ws.files {
            assert!(entry.project_root.is_none());
        }
    }

    #[test]
    fn test_gitignore_exclusion() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &[
                ".git/",
                "shard.yml",
                "src/app.cr",
                "src/generated.cr",
                ".gitignore",
            ],
        );
        // Write a .gitignore that excludes generated.cr
        fs::write(tmp.path().join(".gitignore"), "src/generated.cr\n").unwrap();

        let ws = Workspace::scan(tmp.path());

        assert_eq!(ws.files.len(), 1);
        assert!(ws.files.values().any(|e| e.path.ends_with("src/app.cr")));
        assert!(!ws.files.values().any(|e| e.path.ends_with("src/generated.cr")));
    }

    #[test]
    fn test_skip_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &[
                "shard.yml",
                "src/app.cr",
                "lib/some_shard/src/shard.cr",
                "node_modules/dep/src/dep.cr",
                "target/debug/build.cr",
                ".git/objects/foo.cr",
            ],
        );

        let ws = Workspace::scan(tmp.path());

        // Only src/app.cr — lib/ skipped (shard.yml sibling), others always skipped
        assert_eq!(ws.files.len(), 1);
        assert!(ws.files.values().any(|e| e.path.ends_with("src/app.cr")));
    }

    #[test]
    fn test_lib_not_skipped_without_shard_yml() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &[
                "src/app.cr",
                "src/lib/helper.cr",
            ],
        );

        let ws = Workspace::scan(tmp.path());

        // No shard.yml, so src/lib/ is not a dependency dir — both files found
        assert_eq!(ws.files.len(), 2);
    }

    #[test]
    fn test_multiple_shard_ymls() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &[
                "services/api/shard.yml",
                "services/api/src/main.cr",
                "services/worker/shard.yml",
                "services/worker/src/job.cr",
                "shared/utils.cr",
            ],
        );

        let ws = Workspace::scan(tmp.path());

        assert_eq!(ws.files.len(), 3);
        assert_eq!(ws.project_roots.len(), 2);

        let api_file = ws.files.values().find(|e| e.path.ends_with("main.cr")).unwrap();
        assert_eq!(
            api_file.project_root.as_deref(),
            Some(tmp.path().join("services/api").as_path())
        );

        let worker_file = ws.files.values().find(|e| e.path.ends_with("job.cr")).unwrap();
        assert_eq!(
            worker_file.project_root.as_deref(),
            Some(tmp.path().join("services/worker").as_path())
        );

        let shared_file = ws.files.values().find(|e| e.path.ends_with("utils.cr")).unwrap();
        assert!(shared_file.project_root.is_none());
    }
}

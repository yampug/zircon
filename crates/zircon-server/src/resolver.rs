use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use log::debug;

/// The result of resolving a single `require` statement.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedRequire {
    /// Resolved to one or more file paths (glob may produce multiple).
    Files(Vec<PathBuf>),
    /// Standard library require — not resolved to a local file.
    Stdlib(String),
    /// Could not resolve to any file.
    Unresolved(String),
}

/// Resolves Crystal `require` statements to file paths.
pub struct RequireResolver {
    /// Directories containing `shard.yml` (Crystal project roots).
    project_roots: Vec<PathBuf>,
}

impl RequireResolver {
    pub fn new(project_roots: Vec<PathBuf>) -> Self {
        RequireResolver { project_roots }
    }

    /// Resolve a single require path relative to the file that contains it.
    pub fn resolve(&self, require_path: &str, from_file: &Path) -> ResolvedRequire {
        let from_dir = from_file.parent().unwrap_or(Path::new("."));

        if require_path.starts_with("./") || require_path.starts_with("../") {
            self.resolve_relative(require_path, from_dir)
        } else {
            self.resolve_shard_or_stdlib(require_path, from_file)
        }
    }

    /// Build a dependency graph: maps each file to the list of local file paths
    /// it requires. Stdlib and unresolved requires are excluded.
    /// Circular requires are handled — each file is processed at most once.
    pub fn build_graph<F>(&self, files: &[PathBuf], read_source: F) -> HashMap<PathBuf, Vec<PathBuf>>
    where
        F: Fn(&Path) -> Option<String>,
    {
        let mut graph: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut visited: HashSet<PathBuf> = HashSet::new();
        let mut queue: Vec<PathBuf> = files.to_vec();

        while let Some(file) = queue.pop() {
            if !visited.insert(file.clone()) {
                continue; // already processed — handles circular requires
            }

            let source = match read_source(&file) {
                Some(s) => s,
                None => continue,
            };

            let require_paths = extract_requires(&source);
            let mut deps = Vec::new();

            for req in require_paths {
                match self.resolve(&req, &file) {
                    ResolvedRequire::Files(paths) => {
                        for p in &paths {
                            if !visited.contains(p) {
                                queue.push(p.clone());
                            }
                        }
                        deps.extend(paths);
                    }
                    _ => {} // stdlib / unresolved — skip
                }
            }

            graph.insert(file, deps);
        }

        graph
    }

    fn resolve_relative(&self, require_path: &str, from_dir: &Path) -> ResolvedRequire {
        // Handle glob patterns
        if require_path.ends_with("/*") {
            let dir = from_dir.join(&require_path[..require_path.len() - 2]);
            return self.resolve_glob(&dir, false);
        }
        if require_path.ends_with("/**") {
            let dir = from_dir.join(&require_path[..require_path.len() - 3]);
            return self.resolve_glob(&dir, true);
        }

        // Standard relative require — try with .cr extension
        let target = from_dir.join(require_path);
        let with_ext = target.with_extension("cr");

        if with_ext.is_file() {
            let canonical = with_ext.canonicalize().unwrap_or(with_ext);
            return ResolvedRequire::Files(vec![canonical]);
        }

        // Crystal also tries <path>/basename.cr (directory form)
        if let Some(basename) = target.file_name() {
            let dir_form = target.join(basename).with_extension("cr");
            if dir_form.is_file() {
                let canonical = dir_form.canonicalize().unwrap_or(dir_form);
                return ResolvedRequire::Files(vec![canonical]);
            }
        }

        ResolvedRequire::Unresolved(require_path.to_string())
    }

    fn resolve_glob(&self, dir: &Path, recursive: bool) -> ResolvedRequire {
        let mut files = Vec::new();
        if dir.is_dir() {
            collect_cr_files(dir, recursive, &mut files);
            files.sort();
        }
        if files.is_empty() {
            ResolvedRequire::Unresolved(format!("{}/*", dir.display()))
        } else {
            ResolvedRequire::Files(files)
        }
    }

    fn resolve_shard_or_stdlib(&self, require_path: &str, from_file: &Path) -> ResolvedRequire {
        // Find the project root for this file.
        let project_root = self.project_roots.iter().find(|root| {
            from_file.starts_with(root)
        });

        if let Some(root) = project_root {
            // Try shard resolution: lib/<name>/src/<name>.cr
            let shard_name = require_path.split('/').next().unwrap_or(require_path);
            let shard_entry = root.join("lib").join(shard_name).join("src").join(shard_name);
            let shard_file = shard_entry.with_extension("cr");

            if shard_file.is_file() {
                // If require has a subpath (e.g., "some_shard/submodule"),
                // resolve the subpath within the shard's src directory.
                if require_path.contains('/') {
                    let subpath = &require_path[shard_name.len() + 1..];
                    let shard_src = root.join("lib").join(shard_name).join("src");
                    let target = shard_src.join(subpath).with_extension("cr");
                    if target.is_file() {
                        return ResolvedRequire::Files(vec![target]);
                    }
                }
                return ResolvedRequire::Files(vec![shard_file]);
            }
        }

        // Not a shard — classify as stdlib
        debug!("classifying {:?} as stdlib require", require_path);
        ResolvedRequire::Stdlib(require_path.to_string())
    }
}

/// Collect `.cr` files from a directory, optionally recursing into subdirs.
fn collect_cr_files(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().map_or(false, |e| e == "cr") {
            out.push(path.canonicalize().unwrap_or(path));
        } else if recursive && path.is_dir() {
            collect_cr_files(&path, true, out);
        }
    }
}

/// Extract require paths from Crystal source code.
/// Matches `require "path"` statements.
pub fn extract_requires(source: &str) -> Vec<String> {
    let mut requires = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("require ") {
            let rest = rest.trim();
            if rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2 {
                let path = &rest[1..rest.len() - 1];
                if !path.is_empty() {
                    requires.push(path.to_string());
                }
            }
        }
    }
    requires
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Canonicalize a path for test assertions (macOS symlinks /var → /private/var).
    fn canon(path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    #[test]
    fn test_extract_requires() {
        let source = r#"
require "json"
require "./models/user"
require "../shared/utils"

class Foo
end
"#;
        let reqs = extract_requires(source);
        assert_eq!(reqs, vec!["json", "./models/user", "../shared/utils"]);
    }

    #[test]
    fn test_relative_require() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &[
                "src/app.cr",
                "src/models/user.cr",
                "src/models/post.cr",
            ],
        );

        let resolver = RequireResolver::new(vec![]);
        let result = resolver.resolve("./models/user", &tmp.path().join("src/app.cr"));

        assert_eq!(
            result,
            ResolvedRequire::Files(vec![canon(&tmp.path().join("src/models/user.cr"))])
        );
    }

    #[test]
    fn test_relative_parent_require() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &["src/models/user.cr", "src/utils.cr"],
        );

        let resolver = RequireResolver::new(vec![]);
        let result = resolver.resolve("../utils", &tmp.path().join("src/models/user.cr"));

        assert_eq!(
            result,
            ResolvedRequire::Files(vec![canon(&tmp.path().join("src/utils.cr"))])
        );
    }

    #[test]
    fn test_glob_require() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &[
                "src/app.cr",
                "src/models/post.cr",
                "src/models/user.cr",
            ],
        );

        let resolver = RequireResolver::new(vec![]);
        let result = resolver.resolve("./models/*", &tmp.path().join("src/app.cr"));

        match result {
            ResolvedRequire::Files(files) => {
                assert_eq!(files.len(), 2);
                assert!(files.contains(&canon(&tmp.path().join("src/models/post.cr"))));
                assert!(files.contains(&canon(&tmp.path().join("src/models/user.cr"))));
            }
            other => panic!("expected Files, got {:?}", other),
        }
    }

    #[test]
    fn test_recursive_glob_require() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &[
                "src/app.cr",
                "src/models/user.cr",
                "src/models/concerns/validatable.cr",
            ],
        );

        let resolver = RequireResolver::new(vec![]);
        let result = resolver.resolve("./models/**", &tmp.path().join("src/app.cr"));

        match result {
            ResolvedRequire::Files(files) => {
                assert_eq!(files.len(), 2);
                assert!(files.contains(&canon(&tmp.path().join("src/models/user.cr"))));
                assert!(files.contains(&canon(&tmp.path().join("src/models/concerns/validatable.cr"))));
            }
            other => panic!("expected Files, got {:?}", other),
        }
    }

    #[test]
    fn test_stdlib_require() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(tmp.path(), &["shard.yml", "src/app.cr"]);

        let resolver = RequireResolver::new(vec![tmp.path().to_path_buf()]);
        let result = resolver.resolve("json", &tmp.path().join("src/app.cr"));

        assert_eq!(result, ResolvedRequire::Stdlib("json".to_string()));
    }

    #[test]
    fn test_shard_require() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &[
                "shard.yml",
                "src/app.cr",
                "lib/my_shard/src/my_shard.cr",
            ],
        );

        let resolver = RequireResolver::new(vec![canon(tmp.path())]);
        let result = resolver.resolve("my_shard", &canon(&tmp.path().join("src/app.cr")));

        match result {
            ResolvedRequire::Files(files) => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].file_name().unwrap(), "my_shard.cr");
            }
            other => panic!("expected Files, got {:?}", other),
        }
    }

    #[test]
    fn test_unresolved_relative() {
        let tmp = tempfile::tempdir().unwrap();
        create_tree(tmp.path(), &["src/app.cr"]);

        let resolver = RequireResolver::new(vec![]);
        let result = resolver.resolve("./nonexistent", &tmp.path().join("src/app.cr"));

        assert_eq!(
            result,
            ResolvedRequire::Unresolved("./nonexistent".to_string())
        );
    }

    #[test]
    fn test_circular_requires() {
        let tmp = tempfile::tempdir().unwrap();

        // a.cr requires b.cr, b.cr requires a.cr
        let a = tmp.path().join("a.cr");
        let b = tmp.path().join("b.cr");
        fs::write(&a, "require \"./b\"\n").unwrap();
        fs::write(&b, "require \"./a\"\n").unwrap();

        let resolver = RequireResolver::new(vec![]);
        let ca = canon(&a);
        let cb = canon(&b);
        let graph = resolver.build_graph(&[ca.clone()], |path| fs::read_to_string(path).ok());

        // Both files should be in the graph without infinite loop
        assert!(graph.contains_key(&ca));
        assert!(graph.contains_key(&cb));
        assert_eq!(graph[&ca], vec![cb.clone()]);
        assert_eq!(graph[&cb], vec![ca.clone()]);
    }

    #[test]
    fn test_build_graph_follows_dependencies() {
        let tmp = tempfile::tempdir().unwrap();

        fs::create_dir_all(tmp.path().join("src/models")).unwrap();
        fs::write(tmp.path().join("src/app.cr"), "require \"./models/user\"\nrequire \"./models/post\"\n").unwrap();
        fs::write(tmp.path().join("src/models/user.cr"), "require \"json\"\n").unwrap();
        fs::write(tmp.path().join("src/models/post.cr"), "").unwrap();

        // Canonicalize after files exist
        let app = canon(&tmp.path().join("src/app.cr"));
        let user = canon(&tmp.path().join("src/models/user.cr"));
        let post = canon(&tmp.path().join("src/models/post.cr"));

        let resolver = RequireResolver::new(vec![]);
        let graph = resolver.build_graph(&[app.clone()], |path| fs::read_to_string(path).ok());

        // app depends on user and post
        assert_eq!(graph[&app].len(), 2);
        assert!(graph[&app].contains(&user));
        assert!(graph[&app].contains(&post));

        // user and post were followed transitively
        assert!(graph.contains_key(&user));
        assert!(graph.contains_key(&post));

        // user's "json" require is stdlib, not in the file deps
        assert!(graph[&user].is_empty());
    }

    #[test]
    fn test_directory_form_require() {
        // Crystal: require "./foo" can resolve to foo/foo.cr
        let tmp = tempfile::tempdir().unwrap();
        create_tree(
            tmp.path(),
            &["src/app.cr", "src/foo/foo.cr"],
        );

        let resolver = RequireResolver::new(vec![]);
        let result = resolver.resolve("./foo", &tmp.path().join("src/app.cr"));

        assert_eq!(
            result,
            ResolvedRequire::Files(vec![canon(&tmp.path().join("src/foo/foo.cr"))])
        );
    }
}

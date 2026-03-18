use std::path::Path;
use std::process::Command;
use std::time::Duration;

use log::{debug, warn};
use lsp_types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, Position, Range};
use serde::Deserialize;

/// Source label for Crystal compiler diagnostics.
pub const SOURCE_COMPILER: &str = "zircon-compiler";

const COMPILER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct CompilerDiagnostic {
    file: String,
    line: u64,
    column: u64,
    #[serde(default)]
    size: Option<u64>,
    message: String,
    #[serde(default = "default_severity")]
    severity: String,
}

fn default_severity() -> String {
    "error".to_string()
}

/// Run the Crystal compiler on `file` and return LSP diagnostics.
///
/// Returns `None` if the Crystal binary is not found. Returns an empty vec
/// if the file compiles cleanly.
pub fn check_file(file: &Path) -> Option<Vec<Diagnostic>> {
    let crystal = find_crystal_binary()?;

    let output = match Command::new(&crystal)
        .args(["build", "--no-codegen", "-f", "json"])
        .arg(file)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            warn!("failed to spawn crystal: {}", e);
            return None;
        }
    };

    // Wait with timeout.
    let result = wait_with_timeout(output, COMPILER_TIMEOUT);

    let stderr = match result {
        Some(output) => String::from_utf8_lossy(&output.stderr).to_string(),
        None => {
            warn!("crystal compiler timed out after {:?}", COMPILER_TIMEOUT);
            return Some(Vec::new());
        }
    };

    Some(parse_compiler_output(&stderr, file))
}

/// Parse the JSON diagnostic output from Crystal's stderr.
pub fn parse_compiler_output(stderr: &str, target_file: &Path) -> Vec<Diagnostic> {
    let entries: Vec<CompilerDiagnostic> = match serde_json::from_str(stderr) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let target_str = target_file.to_string_lossy();

    entries
        .into_iter()
        .filter(|e| {
            // Only include diagnostics for the target file.
            e.file == target_str.as_ref() || target_file.ends_with(&e.file)
        })
        .map(|e| {
            let line = if e.line > 0 { e.line - 1 } else { 0 };
            let col = if e.column > 0 { e.column - 1 } else { 0 };
            let end_col = col + e.size.unwrap_or(1);

            let severity = match e.severity.as_str() {
                "warning" => DiagnosticSeverity::WARNING,
                _ => DiagnosticSeverity::ERROR,
            };

            let tags = if is_nil_risk(&e.message) {
                Some(vec![DiagnosticTag::UNNECESSARY])
            } else {
                None
            };

            Diagnostic {
                range: Range {
                    start: Position {
                        line: line as u32,
                        character: col as u32,
                    },
                    end: Position {
                        line: line as u32,
                        character: end_col as u32,
                    },
                },
                severity: Some(severity),
                source: Some(SOURCE_COMPILER.to_string()),
                message: e.message,
                tags,
                ..Default::default()
            }
        })
        .collect()
}

/// Returns true if a diagnostic message indicates a Nil-risk issue.
pub fn is_nil_risk(message: &str) -> bool {
    message.contains("Nil")
        && (message.contains("for Nil")
            || message.contains("not Nil")
            || message.contains("| Nil"))
}

/// Run `crystal tool expand` at a specific cursor position and return the
/// expanded code. Returns `None` if the crystal binary is not found or the
/// expansion fails.
pub fn macro_expand(file: &Path, line: u32, col: u32) -> Option<String> {
    let crystal = find_crystal_binary()?;
    let cursor = format!("{}:{}:{}", file.display(), line, col);

    let output = match Command::new(&crystal)
        .args(["tool", "expand", "-c", &cursor])
        .arg(file)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            warn!("failed to spawn crystal tool expand: {}", e);
            return None;
        }
    };

    let result = wait_with_timeout(output, COMPILER_TIMEOUT)?;
    let stdout = String::from_utf8_lossy(&result.stdout).to_string();

    if stdout.trim().is_empty() {
        debug!("crystal tool expand produced no output");
        return None;
    }

    Some(stdout.trim().to_string())
}

/// Locate the `crystal` binary on PATH.
fn find_crystal_binary() -> Option<String> {
    let result = Command::new("which").arg("crystal").output().ok()?;
    if result.status.success() {
        let path = String::from_utf8_lossy(&result.stdout).trim().to_string();
        if path.is_empty() {
            debug!("crystal binary not found on PATH");
            None
        } else {
            Some(path)
        }
    } else {
        debug!("crystal binary not found on PATH");
        None
    }
}

/// Wait for a child process with a timeout. Returns `None` if timed out (and kills the process).
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Option<std::process::Output> {
    use std::thread;
    use std::time::Instant;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                // Process exited — collect remaining output via wait_with_output.
                return child.wait_with_output().ok();
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_compiler_output_error() {
        let json = r#"[{"file":"test.cr","line":5,"column":3,"size":4,"message":"undefined method 'foo'","severity":"error"}]"#;
        let diags = parse_compiler_output(json, Path::new("test.cr"));

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "undefined method 'foo'");
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].source.as_deref(), Some(SOURCE_COMPILER));
        // Line 5 → 0-indexed line 4
        assert_eq!(diags[0].range.start.line, 4);
        // Column 3 → 0-indexed column 2
        assert_eq!(diags[0].range.start.character, 2);
        // Size 4 → end col = 2 + 4 = 6
        assert_eq!(diags[0].range.end.character, 6);
    }

    #[test]
    fn test_parse_compiler_output_warning() {
        let json = r#"[{"file":"test.cr","line":1,"column":1,"message":"unused var","severity":"warning"}]"#;
        let diags = parse_compiler_output(json, Path::new("test.cr"));

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
    }

    #[test]
    fn test_parse_compiler_output_nil_risk() {
        let json = r#"[{"file":"test.cr","line":3,"column":5,"message":"undefined method 'size' for Nil","severity":"error"}]"#;
        let diags = parse_compiler_output(json, Path::new("test.cr"));

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].tags, Some(vec![DiagnosticTag::UNNECESSARY]));
    }

    #[test]
    fn test_parse_compiler_output_no_nil_tag_for_normal_error() {
        let json = r#"[{"file":"test.cr","line":1,"column":1,"message":"syntax error","severity":"error"}]"#;
        let diags = parse_compiler_output(json, Path::new("test.cr"));

        assert_eq!(diags.len(), 1);
        assert!(diags[0].tags.is_none());
    }

    #[test]
    fn test_parse_compiler_output_filters_other_files() {
        let json = r#"[
            {"file":"test.cr","line":1,"column":1,"message":"err1","severity":"error"},
            {"file":"other.cr","line":1,"column":1,"message":"err2","severity":"error"}
        ]"#;
        let diags = parse_compiler_output(json, Path::new("test.cr"));

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "err1");
    }

    #[test]
    fn test_parse_compiler_output_invalid_json() {
        let diags = parse_compiler_output("not json at all", Path::new("test.cr"));
        assert!(diags.is_empty());
    }

    #[test]
    fn test_parse_compiler_output_empty_array() {
        let diags = parse_compiler_output("[]", Path::new("test.cr"));
        assert!(diags.is_empty());
    }

    #[test]
    fn test_is_nil_risk() {
        assert!(is_nil_risk("undefined method 'size' for Nil"));
        assert!(is_nil_risk("expected not Nil but got Nil"));
        assert!(is_nil_risk("type is String | Nil"));
        assert!(!is_nil_risk("undefined method 'foo'"));
        assert!(!is_nil_risk("syntax error"));
    }

    #[test]
    fn test_parse_compiler_output_missing_size() {
        let json = r#"[{"file":"test.cr","line":2,"column":4,"message":"some error","severity":"error"}]"#;
        let diags = parse_compiler_output(json, Path::new("test.cr"));

        assert_eq!(diags.len(), 1);
        // Without size, end_col = col + 1 (default size of 1)
        assert_eq!(diags[0].range.start.character, 3);
        assert_eq!(diags[0].range.end.character, 4);
    }

    #[test]
    fn test_parse_compiler_output_default_severity() {
        let json = r#"[{"file":"test.cr","line":1,"column":1,"message":"something"}]"#;
        let diags = parse_compiler_output(json, Path::new("test.cr"));

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
    }
}

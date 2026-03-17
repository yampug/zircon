use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
use tree_sitter::{Node, Parser};

use crate::crystal_cli;

/// Source label for tree-sitter syntax diagnostics.
pub const SOURCE_SYNTAX: &str = "zircon-syntax";

/// Stores syntax and compiler diagnostics per file, merging them for publishing.
pub struct DiagnosticStore {
    syntax: HashMap<PathBuf, Vec<Diagnostic>>,
    compiler: HashMap<PathBuf, Vec<Diagnostic>>,
}

impl DiagnosticStore {
    pub fn new() -> Self {
        DiagnosticStore {
            syntax: HashMap::new(),
            compiler: HashMap::new(),
        }
    }

    /// Update the syntax (tree-sitter) diagnostics for a file.
    pub fn set_syntax(&mut self, path: &Path, diags: Vec<Diagnostic>) {
        self.syntax.insert(path.to_path_buf(), diags);
    }

    /// Update the compiler diagnostics for a file.
    pub fn set_compiler(&mut self, path: &Path, diags: Vec<Diagnostic>) {
        self.compiler.insert(path.to_path_buf(), diags);
    }

    /// Clear all diagnostics for a file.
    pub fn clear(&mut self, path: &Path) {
        self.syntax.remove(path);
        self.compiler.remove(path);
    }

    /// Return the merged, deduplicated diagnostics for a file.
    ///
    /// When a compiler diagnostic and a syntax diagnostic overlap on the same
    /// line, the compiler diagnostic is kept (it is more specific) and the
    /// syntax diagnostic is dropped.
    pub fn merged(&self, path: &Path) -> Vec<Diagnostic> {
        let syntax = self.syntax.get(path);
        let compiler = self.compiler.get(path);

        match (syntax, compiler) {
            (None, None) => Vec::new(),
            (Some(s), None) => s.clone(),
            (None, Some(c)) => c.clone(),
            (Some(s), Some(c)) => merge_diagnostics(s, c),
        }
    }
}

/// Merge syntax and compiler diagnostics, deduplicating by line.
/// Compiler diagnostics take precedence over syntax diagnostics on the same line.
fn merge_diagnostics(syntax: &[Diagnostic], compiler: &[Diagnostic]) -> Vec<Diagnostic> {
    // Collect the set of lines that have compiler diagnostics.
    let compiler_lines: std::collections::HashSet<u32> = compiler
        .iter()
        .map(|d| d.range.start.line)
        .collect();

    let mut result: Vec<Diagnostic> = compiler.to_vec();

    // Add syntax diagnostics that don't overlap with compiler diagnostics.
    for d in syntax {
        if !compiler_lines.contains(&d.range.start.line) {
            result.push(d.clone());
        }
    }

    result
}

/// Extract syntax error diagnostics from Crystal source code.
pub fn extract_syntax_errors(parser: &mut Parser, source: &str) -> Vec<Diagnostic> {
    let tree = match parser.parse(source, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut diagnostics = Vec::new();
    collect_errors(tree.root_node(), source, &mut diagnostics);
    diagnostics
}

fn collect_errors(node: Node, source: &str, diagnostics: &mut Vec<Diagnostic>) {
    if node.is_error() {
        diagnostics.push(make_error_diagnostic(&node, source));
    } else if node.is_missing() {
        diagnostics.push(make_missing_diagnostic(&node));
    }

    // Don't recurse into ERROR nodes — their children would produce
    // confusing duplicate diagnostics.
    if !node.is_error() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_errors(child, source, diagnostics);
        }
    }
}

fn make_error_diagnostic(node: &Node, source: &str) -> Diagnostic {
    let start = node.start_position();
    let end = node.end_position();

    let text = node.utf8_text(source.as_bytes()).unwrap_or("");
    let message = if text.is_empty() || text.len() > 50 {
        "Syntax error".to_string()
    } else {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            "Unexpected token".to_string()
        } else {
            format!("Unexpected `{}`", trimmed)
        }
    };

    Diagnostic {
        range: Range {
            start: Position {
                line: start.row as u32,
                character: start.column as u32,
            },
            end: Position {
                line: end.row as u32,
                character: end.column as u32,
            },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some(SOURCE_SYNTAX.to_string()),
        message,
        ..Default::default()
    }
}

fn make_missing_diagnostic(node: &Node) -> Diagnostic {
    let pos = node.start_position();
    let kind = node.kind();

    Diagnostic {
        range: Range {
            start: Position {
                line: pos.row as u32,
                character: pos.column as u32,
            },
            end: Position {
                line: pos.row as u32,
                character: pos.column as u32,
            },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some(SOURCE_SYNTAX.to_string()),
        message: format!("Missing `{}`", kind),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_parser() -> Parser {
        let lang = tree_sitter_crystal::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&lang).unwrap();
        parser
    }

    #[test]
    fn test_valid_code_no_errors() {
        let mut parser = make_parser();
        let diags = extract_syntax_errors(
            &mut parser,
            "class Foo\n  def bar\n  end\nend\n",
        );
        assert!(diags.is_empty());
    }

    #[test]
    fn test_missing_end() {
        let mut parser = make_parser();
        let diags = extract_syntax_errors(&mut parser, "class Foo\n  def bar\n  end\n");
        assert!(!diags.is_empty(), "should report error for missing end");
        assert!(diags
            .iter()
            .all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
        assert!(diags
            .iter()
            .all(|d| d.source.as_deref() == Some(SOURCE_SYNTAX)));
    }

    #[test]
    fn test_unexpected_token() {
        let mut parser = make_parser();
        let diags = extract_syntax_errors(&mut parser, "def foo\n  @@@ invalid\nend\n");
        assert!(!diags.is_empty());
    }

    #[test]
    fn test_diagnostics_have_position() {
        let mut parser = make_parser();
        let diags = extract_syntax_errors(&mut parser, "class Foo\n");
        assert!(!diags.is_empty(), "incomplete class should have errors");
        for d in &diags {
            // Range should be valid.
            assert!(
                d.range.start.line < d.range.end.line
                    || (d.range.start.line == d.range.end.line
                        && d.range.start.character <= d.range.end.character)
            );
        }
    }

    #[test]
    fn test_fixing_error_clears_diagnostics() {
        let mut parser = make_parser();

        let diags = extract_syntax_errors(&mut parser, "class Foo\n");
        assert!(!diags.is_empty(), "broken code should have errors");

        let diags = extract_syntax_errors(&mut parser, "class Foo\nend\n");
        assert!(diags.is_empty(), "fixed code should have no errors");
    }

    #[test]
    fn test_multiple_errors() {
        let mut parser = make_parser();
        let diags = extract_syntax_errors(&mut parser, "class\ndef\n");
        assert!(!diags.is_empty());
    }

    #[test]
    fn test_malformed_string_literal() {
        let mut parser = make_parser();
        let diags = extract_syntax_errors(&mut parser, "x = \"hello\n");
        assert!(!diags.is_empty(), "unclosed string should produce errors");
    }

    #[test]
    fn test_source_label() {
        let mut parser = make_parser();
        let diags = extract_syntax_errors(&mut parser, "class Foo\n");
        for d in &diags {
            assert_eq!(d.source.as_deref(), Some("zircon-syntax"));
        }
    }

    fn make_diag(line: u32, source: &str, message: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line,
                    character: 0,
                },
                end: Position {
                    line,
                    character: 5,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some(source.to_string()),
            message: message.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_store_syntax_only() {
        let mut store = DiagnosticStore::new();
        let path = Path::new("test.cr");
        let diags = vec![make_diag(0, SOURCE_SYNTAX, "syntax err")];
        store.set_syntax(path, diags);

        let merged = store.merged(path);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source.as_deref(), Some(SOURCE_SYNTAX));
    }

    #[test]
    fn test_store_compiler_only() {
        let mut store = DiagnosticStore::new();
        let path = Path::new("test.cr");
        let diags = vec![make_diag(0, crystal_cli::SOURCE_COMPILER, "type err")];
        store.set_compiler(path, diags);

        let merged = store.merged(path);
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].source.as_deref(),
            Some(crystal_cli::SOURCE_COMPILER)
        );
    }

    #[test]
    fn test_store_dedup_same_line_prefers_compiler() {
        let mut store = DiagnosticStore::new();
        let path = Path::new("test.cr");
        store.set_syntax(
            path,
            vec![make_diag(5, SOURCE_SYNTAX, "unexpected token")],
        );
        store.set_compiler(
            path,
            vec![make_diag(5, crystal_cli::SOURCE_COMPILER, "undefined method 'foo'")],
        );

        let merged = store.merged(path);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].message, "undefined method 'foo'");
        assert_eq!(
            merged[0].source.as_deref(),
            Some(crystal_cli::SOURCE_COMPILER)
        );
    }

    #[test]
    fn test_store_different_lines_kept() {
        let mut store = DiagnosticStore::new();
        let path = Path::new("test.cr");
        store.set_syntax(
            path,
            vec![make_diag(1, SOURCE_SYNTAX, "syntax err")],
        );
        store.set_compiler(
            path,
            vec![make_diag(5, crystal_cli::SOURCE_COMPILER, "type err")],
        );

        let merged = store.merged(path);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_store_clear_removes_both() {
        let mut store = DiagnosticStore::new();
        let path = Path::new("test.cr");
        store.set_syntax(path, vec![make_diag(0, SOURCE_SYNTAX, "a")]);
        store.set_compiler(
            path,
            vec![make_diag(1, crystal_cli::SOURCE_COMPILER, "b")],
        );

        store.clear(path);
        let merged = store.merged(path);
        assert!(merged.is_empty());
    }

    #[test]
    fn test_store_update_syntax_preserves_compiler() {
        let mut store = DiagnosticStore::new();
        let path = Path::new("test.cr");
        store.set_compiler(
            path,
            vec![make_diag(3, crystal_cli::SOURCE_COMPILER, "type err")],
        );
        // Syntax errors change but compiler stays.
        store.set_syntax(path, vec![]);

        let merged = store.merged(path);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].message, "type err");
    }

    #[test]
    fn test_store_update_compiler_preserves_syntax() {
        let mut store = DiagnosticStore::new();
        let path = Path::new("test.cr");
        store.set_syntax(
            path,
            vec![make_diag(0, SOURCE_SYNTAX, "syntax err")],
        );
        // Compiler clears but syntax stays.
        store.set_compiler(path, vec![]);

        let merged = store.merged(path);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].message, "syntax err");
    }

    #[test]
    fn test_store_empty_file() {
        let store = DiagnosticStore::new();
        let merged = store.merged(Path::new("unknown.cr"));
        assert!(merged.is_empty());
    }
}

use lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};
use tree_sitter::{Node, Parser};

/// Source label for tree-sitter syntax diagnostics.
pub const SOURCE_SYNTAX: &str = "zircon-syntax";

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
}

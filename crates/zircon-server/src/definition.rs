use std::path::Path;

use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location, Position, Range};
use tree_sitter::{Parser, Point};

use crate::index::{DocumentIndex, SymbolKind};
use crate::uri;

/// Handle a `textDocument/definition` request.
pub fn handle(
    index: &DocumentIndex,
    parser: &mut Parser,
    params: GotoDefinitionParams,
    source: Option<&str>,
) -> Option<GotoDefinitionResponse> {
    let lsp_uri = &params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let path = uri::to_path(lsp_uri)?;

    let source = source?;
    let tree = parser.parse(source, None)?;

    let point = Point {
        row: position.line as usize,
        column: position.character as usize,
    };
    let node = tree.root_node().descendant_for_point_range(point, point)?;

    let (name, kinds) = classify_node(node, source)?;

    // Search the index, prioritizing current file.
    let mut results: Vec<(&Path, _)> = Vec::new();
    if kinds.is_empty() {
        results = index.find_by_name(&name);
    } else {
        for kind in &kinds {
            results.extend(index.find_definition(&name, *kind));
        }
    }

    if results.is_empty() {
        return Some(GotoDefinitionResponse::Array(vec![]));
    }

    // Sort: current file first, then alphabetically by path.
    results.sort_by(|a, b| {
        let a_local = a.0 == path.as_path();
        let b_local = b.0 == path.as_path();
        b_local.cmp(&a_local).then_with(|| a.0.cmp(b.0))
    });

    let locations: Vec<Location> = results
        .iter()
        .filter_map(|(p, sym)| {
            let u = uri::from_path(p)?;
            Some(Location {
                uri: u,
                range: Range {
                    start: Position {
                        line: sym.start_line as u32,
                        character: sym.start_col as u32,
                    },
                    end: Position {
                        line: sym.end_line as u32,
                        character: sym.end_col as u32,
                    },
                },
            })
        })
        .collect();

    Some(GotoDefinitionResponse::Array(locations))
}

/// Classify a tree-sitter node to determine the symbol name and which
/// `SymbolKind`s to search for.
pub fn classify_node(
    node: tree_sitter::Node,
    source: &str,
) -> Option<(String, Vec<SymbolKind>)> {
    let text = node.utf8_text(source.as_bytes()).ok()?;
    let name = text.to_string();

    if name.is_empty() {
        return None;
    }

    match node.kind() {
        "instance_var" => {
            Some((name, vec![SymbolKind::Field]))
        }
        "class_var" => {
            Some((name, vec![SymbolKind::Field]))
        }
        "constant" => {
            Some((
                name,
                vec![
                    SymbolKind::Class,
                    SymbolKind::Module,
                    SymbolKind::Struct,
                    SymbolKind::Enum,
                    SymbolKind::Constant,
                    SymbolKind::Type,
                ],
            ))
        }
        "identifier" => {
            let parent = node.parent();
            match parent.map(|p| p.kind()) {
                Some("call") | Some("method_def") | Some("abstract_method_def") => {
                    Some((name, vec![SymbolKind::Method]))
                }
                _ => {
                    Some((
                        name,
                        vec![SymbolKind::Method, SymbolKind::Macro, SymbolKind::Function],
                    ))
                }
            }
        }
        "named_type" | "generic_type" => {
            let type_name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .unwrap_or(text)
                .to_string();
            Some((
                type_name,
                vec![
                    SymbolKind::Class,
                    SymbolKind::Module,
                    SymbolKind::Struct,
                    SymbolKind::Enum,
                    SymbolKind::Type,
                ],
            ))
        }
        _ => {
            if text.chars().all(|c| c.is_alphanumeric() || c == '_') {
                Some((name, vec![]))
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::DocumentIndex;
    use std::path::PathBuf;

    fn make_parser() -> Parser {
        let lang = tree_sitter_crystal::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&lang).unwrap();
        parser
    }

    fn make_params(path: &str, line: u32, character: u32) -> GotoDefinitionParams {
        let uri = uri::from_path(Path::new(path)).unwrap();
        GotoDefinitionParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    #[test]
    fn test_definition_method_call() {
        let mut index = DocumentIndex::new();
        let source = "class Foo\n  def greet\n    \"hello\"\n  end\n\n  def run\n    greet\n  end\nend\n";
        let path = PathBuf::from("/tmp/test_def.cr");
        index.update_file(&path, source);

        let mut parser = make_parser();
        // Cursor on "greet" at line 6, col 4
        let params = make_params("/tmp/test_def.cr", 6, 4);

        let result = handle(&index, &mut parser, params, Some(source));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                assert!(!locs.is_empty(), "should find at least one definition");
                assert_eq!(locs[0].range.start.line, 1);
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn test_definition_class_name() {
        let mut index = DocumentIndex::new();
        let file_a = PathBuf::from("/tmp/a.cr");
        let file_b = PathBuf::from("/tmp/b.cr");
        let source_a = "class User\n  def name\n  end\nend\n";
        let source_b = "user = User.new\n";

        index.update_file(&file_a, source_a);
        index.update_file(&file_b, source_b);

        let mut parser = make_parser();
        // Cursor on "User" at line 0, col 7
        let params = make_params("/tmp/b.cr", 0, 7);

        let result = handle(&index, &mut parser, params, Some(source_b));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                assert!(!locs.is_empty());
                let uri_a = uri::from_path(&file_a).unwrap();
                assert_eq!(locs[0].uri, uri_a);
                assert_eq!(locs[0].range.start.line, 0);
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn test_definition_constant() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/const.cr");
        let source = "MAX = 100\nputs MAX\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        // Cursor on "MAX" at line 1, col 5
        let params = make_params("/tmp/const.cr", 1, 5);

        let result = handle(&index, &mut parser, params, Some(source));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                assert!(!locs.is_empty());
                assert_eq!(locs[0].range.start.line, 0);
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn test_definition_instance_var() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/ivar.cr");
        let source = "class Foo\n  @name = \"\"\n\n  def show\n    @name\n  end\nend\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        // Cursor on "@name" at line 4, col 4
        let params = make_params("/tmp/ivar.cr", 4, 4);

        let result = handle(&index, &mut parser, params, Some(source));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                assert!(!locs.is_empty());
                assert_eq!(locs[0].range.start.line, 1);
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn test_definition_unknown_symbol() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/unknown.cr");
        let source = "puts unknown_method\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        // Cursor on "unknown_method" at line 0, col 5
        let params = make_params("/tmp/unknown.cr", 0, 5);

        let result = handle(&index, &mut parser, params, Some(source));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                assert!(locs.is_empty(), "unknown symbol should return empty");
            }
            other => panic!("expected empty Array, got {:?}", other),
        }
    }

    #[test]
    fn test_definition_cross_file() {
        let mut index = DocumentIndex::new();
        let file_a = PathBuf::from("/tmp/models/user.cr");
        let file_b = PathBuf::from("/tmp/app.cr");
        let source_a = "class User\n  getter name : String\nend\n";
        let source_b = "user = User.new\nuser.name\n";

        index.update_file(&file_a, source_a);
        index.update_file(&file_b, source_b);

        let mut parser = make_parser();
        // Cursor on "name" at line 1, col 5
        let params = make_params("/tmp/app.cr", 1, 5);

        let result = handle(&index, &mut parser, params, Some(source_b));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                assert!(!locs.is_empty(), "should find name definition");
                let uri_a = uri::from_path(&file_a).unwrap();
                let user_locs: Vec<_> = locs.iter().filter(|l| l.uri == uri_a).collect();
                assert!(!user_locs.is_empty(), "should find def in user.cr");
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }
}

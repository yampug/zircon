use std::path::Path;

use lsp_types::{
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Location, Position, Range,
    SymbolInformation, WorkspaceSymbolParams,
};

use crate::index::{DocumentIndex, Symbol, SymbolKind};
use crate::uri;

const MAX_WORKSPACE_RESULTS: usize = 100;

/// Handle `textDocument/documentSymbol`.
pub fn handle_document_symbols(
    index: &DocumentIndex,
    params: DocumentSymbolParams,
) -> Option<DocumentSymbolResponse> {
    let path = uri::to_path(&params.text_document.uri)?;
    let symbols = index.symbols_for_file(&path)?;

    let tree = build_symbol_tree(symbols);
    Some(DocumentSymbolResponse::Nested(tree))
}

/// Handle `workspace/symbol`.
#[allow(deprecated)] // SymbolInformation::deprecated is deprecated
pub fn handle_workspace_symbols(
    index: &DocumentIndex,
    params: WorkspaceSymbolParams,
) -> Vec<SymbolInformation> {
    let query = &params.query;

    let mut results: Vec<SymbolInformation> = index
        .all_symbols()
        .into_iter()
        .filter(|(_, sym)| {
            // Skip fields from workspace symbol — they're noisy.
            sym.kind != SymbolKind::Field
        })
        .filter(|(_, sym)| {
            if query.is_empty() {
                true
            } else {
                fuzzy_match(&sym.name, query)
            }
        })
        .filter_map(|(path, sym)| {
            let u = uri::from_path(path)?;
            let container = sym.parent.clone();
            Some(SymbolInformation {
                name: sym.name.clone(),
                kind: to_lsp_symbol_kind(sym.kind),
                tags: None,
                deprecated: None,
                location: Location {
                    uri: u,
                    range: name_range(sym),
                },
                container_name: container,
            })
        })
        .collect();

    results.truncate(MAX_WORKSPACE_RESULTS);
    results
}

/// Build a nested `DocumentSymbol` tree from a flat symbol list using `parent`.
#[allow(deprecated)] // DocumentSymbol::deprecated is deprecated
fn build_symbol_tree(symbols: &[Symbol]) -> Vec<DocumentSymbol> {
    // Collect top-level symbols (no parent) and group children by parent name.
    let mut roots: Vec<DocumentSymbol> = Vec::new();
    let mut children_map: std::collections::HashMap<&str, Vec<&Symbol>> =
        std::collections::HashMap::new();

    for sym in symbols {
        if let Some(ref parent) = sym.parent {
            children_map.entry(parent.as_str()).or_default().push(sym);
        }
    }

    for sym in symbols {
        if sym.parent.is_some() {
            continue; // will be added as a child
        }
        roots.push(symbol_to_doc_symbol(sym, &children_map));
    }

    roots
}

#[allow(deprecated)]
fn symbol_to_doc_symbol(
    sym: &Symbol,
    children_map: &std::collections::HashMap<&str, Vec<&Symbol>>,
) -> DocumentSymbol {
    let children = children_map
        .get(sym.name.as_str())
        .map(|kids| {
            kids.iter()
                .map(|child| symbol_to_doc_symbol(child, children_map))
                .collect()
        })
        .unwrap_or_default();

    DocumentSymbol {
        name: sym.name.clone(),
        detail: None,
        kind: to_lsp_symbol_kind(sym.kind),
        tags: None,
        deprecated: None,
        range: def_range(sym),
        selection_range: name_range(sym),
        children: Some(children),
    }
}

/// Simple fuzzy matching: every character of `query` appears in `name` in order.
fn fuzzy_match(name: &str, query: &str) -> bool {
    let mut name_chars = name.chars();
    for qc in query.chars() {
        let qc_lower = qc.to_lowercase().next().unwrap_or(qc);
        loop {
            match name_chars.next() {
                Some(nc) => {
                    if nc.to_lowercase().next().unwrap_or(nc) == qc_lower {
                        break;
                    }
                }
                None => return false,
            }
        }
    }
    true
}

fn name_range(sym: &Symbol) -> Range {
    Range {
        start: Position {
            line: sym.start_line as u32,
            character: sym.start_col as u32,
        },
        end: Position {
            line: sym.end_line as u32,
            character: sym.end_col as u32,
        },
    }
}

fn def_range(sym: &Symbol) -> Range {
    Range {
        start: Position {
            line: sym.def_start_line as u32,
            character: sym.def_start_col as u32,
        },
        end: Position {
            line: sym.def_end_line as u32,
            character: sym.def_end_col as u32,
        },
    }
}

fn to_lsp_symbol_kind(kind: SymbolKind) -> lsp_types::SymbolKind {
    match kind {
        SymbolKind::Class => lsp_types::SymbolKind::CLASS,
        SymbolKind::Module => lsp_types::SymbolKind::MODULE,
        SymbolKind::Struct => lsp_types::SymbolKind::STRUCT,
        SymbolKind::Enum => lsp_types::SymbolKind::ENUM,
        SymbolKind::Lib => lsp_types::SymbolKind::MODULE,
        SymbolKind::Method => lsp_types::SymbolKind::METHOD,
        SymbolKind::Macro => lsp_types::SymbolKind::FUNCTION,
        SymbolKind::Function => lsp_types::SymbolKind::FUNCTION,
        SymbolKind::Constant => lsp_types::SymbolKind::CONSTANT,
        SymbolKind::Type => lsp_types::SymbolKind::TYPE_PARAMETER,
        SymbolKind::Field => lsp_types::SymbolKind::FIELD,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_doc_params(path: &str) -> DocumentSymbolParams {
        let u = uri::from_path(Path::new(path)).unwrap();
        DocumentSymbolParams {
            text_document: lsp_types::TextDocumentIdentifier { uri: u },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }
    }

    #[test]
    fn test_document_symbols_nested_hierarchy() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/ds.cr");
        let source = "module App\n  class Server\n    def start\n    end\n    def stop\n    end\n  end\nend\n";
        index.update_file(&path, source);

        let params = make_doc_params("/tmp/ds.cr");
        let result = handle_document_symbols(&index, params).unwrap();

        match result {
            DocumentSymbolResponse::Nested(roots) => {
                assert_eq!(roots.len(), 1);
                let app = &roots[0];
                assert_eq!(app.name, "App");
                assert_eq!(app.kind, lsp_types::SymbolKind::MODULE);

                let children = app.children.as_ref().unwrap();
                let server = children.iter().find(|c| c.name == "Server").unwrap();
                assert_eq!(server.kind, lsp_types::SymbolKind::CLASS);

                let server_children = server.children.as_ref().unwrap();
                let method_names: Vec<&str> =
                    server_children.iter().map(|c| c.name.as_str()).collect();
                assert!(method_names.contains(&"start"));
                assert!(method_names.contains(&"stop"));
            }
            _ => panic!("expected Nested"),
        }
    }

    #[test]
    fn test_document_symbols_kinds() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/kinds.cr");
        let source = "class Foo\nend\nenum Color\nend\nMAX = 10\nalias Name = String\n";
        index.update_file(&path, source);

        let params = make_doc_params("/tmp/kinds.cr");
        let result = handle_document_symbols(&index, params).unwrap();

        match result {
            DocumentSymbolResponse::Nested(roots) => {
                let foo = roots.iter().find(|s| s.name == "Foo").unwrap();
                assert_eq!(foo.kind, lsp_types::SymbolKind::CLASS);

                let color = roots.iter().find(|s| s.name == "Color").unwrap();
                assert_eq!(color.kind, lsp_types::SymbolKind::ENUM);

                let max = roots.iter().find(|s| s.name == "MAX").unwrap();
                assert_eq!(max.kind, lsp_types::SymbolKind::CONSTANT);

                let name = roots.iter().find(|s| s.name == "Name").unwrap();
                assert_eq!(name.kind, lsp_types::SymbolKind::TYPE_PARAMETER);
            }
            _ => panic!("expected Nested"),
        }
    }

    #[test]
    fn test_document_symbols_range() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/range.cr");
        // "class Foo\n  def bar\n  end\nend\n"
        let source = "class Foo\n  def bar\n  end\nend\n";
        index.update_file(&path, source);

        let params = make_doc_params("/tmp/range.cr");
        let result = handle_document_symbols(&index, params).unwrap();

        match result {
            DocumentSymbolResponse::Nested(roots) => {
                let foo = &roots[0];
                // Full range should span from line 0 to line 3
                assert_eq!(foo.range.start.line, 0);
                assert_eq!(foo.range.end.line, 3);
                // Selection range should be just "Foo"
                assert_eq!(foo.selection_range.start.line, 0);
                assert_eq!(foo.selection_range.start.character, 6); // "class Foo" → col 6
            }
            _ => panic!("expected Nested"),
        }
    }

    #[test]
    fn test_workspace_symbols_prefix() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/ws.cr"),
            "class FooClient\nend\nclass BarService\nend\n",
        );

        let params = WorkspaceSymbolParams {
            query: "Foo".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let results = handle_workspace_symbols(&index, params);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "FooClient");
    }

    #[test]
    fn test_workspace_symbols_fuzzy() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/fz.cr"),
            "class FooBar\nend\nclass FooBaz\nend\nclass Quux\nend\n",
        );

        let params = WorkspaceSymbolParams {
            query: "FoB".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let results = handle_workspace_symbols(&index, params);

        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"FooBar"));
        assert!(names.contains(&"FooBaz"));
        assert!(!names.contains(&"Quux"));
    }

    #[test]
    fn test_workspace_symbols_empty_query() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/all.cr"),
            "class A\nend\ndef b\nend\n",
        );

        let params = WorkspaceSymbolParams {
            query: "".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let results = handle_workspace_symbols(&index, params);

        assert!(results.len() >= 2);
    }

    #[test]
    fn test_workspace_symbols_container_name() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/cn.cr"),
            "class Foo\n  def bar\n  end\nend\n",
        );

        let params = WorkspaceSymbolParams {
            query: "bar".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let results = handle_workspace_symbols(&index, params);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].container_name, Some("Foo".to_string()));
    }

    #[test]
    fn test_workspace_symbols_limit() {
        let mut index = DocumentIndex::new();
        // Generate 150 classes.
        let mut source = String::new();
        for i in 0..150 {
            source.push_str(&format!("class C{}\nend\n", i));
        }
        index.update_file(Path::new("/tmp/many.cr"), &source);

        let params = WorkspaceSymbolParams {
            query: "C".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let results = handle_workspace_symbols(&index, params);

        assert!(results.len() <= MAX_WORKSPACE_RESULTS);
    }

    #[test]
    fn test_fuzzy_match() {
        assert!(fuzzy_match("FooBar", "FoB"));
        assert!(fuzzy_match("FooClient", "FClient"));
        assert!(fuzzy_match("FooBar", "fob")); // case-insensitive
        assert!(fuzzy_match("initialize", "init"));
        assert!(!fuzzy_match("Foo", "Bar"));
        assert!(!fuzzy_match("Foo", "Fooo"));
    }
}

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionList, CompletionParams, CompletionResponse,
};

use crate::index::{DocumentIndex, Symbol, SymbolKind};
use crate::uri;

/// Handle a `textDocument/completion` request.
pub fn handle(
    index: &DocumentIndex,
    params: CompletionParams,
    current_source: Option<&str>,
) -> Option<CompletionResponse> {
    let lsp_uri = &params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let current_path = uri::to_path(lsp_uri)?;

    let source = current_source?;
    let lines: Vec<&str> = source.lines().collect();
    let line = lines.get(position.line as usize)?;
    let col = position.character as usize;
    let prefix = if col <= line.len() { &line[..col] } else { line };

    let items = if let Some(receiver) = detect_dot_completion(prefix) {
        complete_dot(index, &receiver)
    } else if let Some(scope) = detect_scope_completion(prefix) {
        complete_scope(index, &scope)
    } else if let Some(partial) = detect_require_completion(prefix) {
        complete_require(&current_path, &partial)
    } else {
        let partial = extract_word_before_cursor(prefix);
        complete_general(index, &partial)
    };

    Some(CompletionResponse::List(CompletionList {
        is_incomplete: false,
        items,
    }))
}

/// Detect `receiver.` pattern and return the receiver name.
fn detect_dot_completion(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim_end();
    if !trimmed.ends_with('.') {
        return None;
    }
    // Get the word before the dot.
    let before_dot = &trimmed[..trimmed.len() - 1];
    let receiver = before_dot
        .rsplit(|c: char| !c.is_alphanumeric() && c != '_' && c != '@')
        .next()?;
    if receiver.is_empty() {
        return None;
    }
    Some(receiver.to_string())
}

/// Detect `Scope::` pattern and return the scope name.
fn detect_scope_completion(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim_end();
    if !trimmed.ends_with("::") {
        return None;
    }
    let before = &trimmed[..trimmed.len() - 2];
    let scope = before
        .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
        .next()?;
    if scope.is_empty() {
        return None;
    }
    Some(scope.to_string())
}

/// Detect `require "partial` pattern and return the partial path.
fn detect_require_completion(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim();
    let after = trimmed.strip_prefix("require ")?;
    let after = after.strip_prefix('"')?;
    // Don't match if the string is already closed.
    if after.contains('"') {
        return None;
    }
    Some(after.to_string())
}

/// Extract the partial word immediately before the cursor.
fn extract_word_before_cursor(prefix: &str) -> String {
    prefix
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// Complete methods after `.` — find methods belonging to the receiver's type.
fn complete_dot(index: &DocumentIndex, receiver: &str) -> Vec<CompletionItem> {
    // If receiver starts with uppercase, it's a type name — find its methods.
    // Otherwise, we can't infer the type yet; return all methods as fallback.
    let mut seen = HashSet::new();

    if receiver.starts_with(|c: char| c.is_uppercase()) {
        let children = index.find_by_parent(receiver);
        return children
            .into_iter()
            .filter(|(_, sym)| matches!(sym.kind, SymbolKind::Method | SymbolKind::Function))
            .filter(|(_, sym)| seen.insert(sym.name.clone()))
            .map(|(_, sym)| symbol_to_completion(sym))
            .collect();
    }

    // Fallback: suggest all methods in the index (deduplicated by name).
    index
        .all_symbols()
        .into_iter()
        .filter(|(_, sym)| sym.kind == SymbolKind::Method)
        .filter(|(_, sym)| seen.insert(sym.name.clone()))
        .map(|(_, sym)| symbol_to_completion(sym))
        .collect()
}

/// Complete after `::` — suggest constants and nested types in the scope.
fn complete_scope(index: &DocumentIndex, scope: &str) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();
    index
        .find_by_parent(scope)
        .into_iter()
        .filter(|(_, sym)| {
            matches!(
                sym.kind,
                SymbolKind::Class
                    | SymbolKind::Module
                    | SymbolKind::Struct
                    | SymbolKind::Enum
                    | SymbolKind::Constant
                    | SymbolKind::Type
            )
        })
        .filter(|(_, sym)| seen.insert(sym.name.clone()))
        .map(|(_, sym)| symbol_to_completion(sym))
        .collect()
}

/// Complete inside `require "..."` — suggest relative file paths.
fn complete_require(current_file: &Path, partial: &str) -> Vec<CompletionItem> {
    let from_dir = match current_file.parent() {
        Some(d) => d,
        None => return vec![],
    };

    // For relative paths starting with ./ or ../
    if partial.starts_with("./") || partial.starts_with("../") {
        let (dir_part, file_prefix) = match partial.rfind('/') {
            Some(i) => (&partial[..i], &partial[i + 1..]),
            None => (partial, ""),
        };
        let target_dir = from_dir.join(dir_part);
        return list_cr_files(&target_dir, file_prefix)
            .into_iter()
            .map(|name| {
                let insert = format!("{}/{}", dir_part, name);
                CompletionItem {
                    label: name,
                    kind: Some(CompletionItemKind::FILE),
                    insert_text: Some(insert),
                    ..Default::default()
                }
            })
            .collect();
    }

    // For empty or partial bare names: suggest starting with ./
    let target_dir = from_dir;
    list_cr_files(target_dir, partial)
        .into_iter()
        .map(|name| {
            let insert = format!("./{}", name);
            CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::FILE),
                insert_text: Some(insert),
                ..Default::default()
            }
        })
        .collect()
}

/// List `.cr` files in a directory matching a prefix, returning names without extension.
fn list_cr_files(dir: &Path, prefix: &str) -> Vec<String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let mut results = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(prefix) && !name.starts_with('.') {
                results.push(name);
            }
        } else if path.extension().map_or(false, |e| e == "cr") {
            if let Some(stem) = path.file_stem() {
                let name = stem.to_string_lossy().to_string();
                if name.starts_with(prefix) {
                    results.push(name);
                }
            }
        }
    }
    results.sort();
    results
}

/// General completions: suggest types and methods matching partial prefix.
fn complete_general(index: &DocumentIndex, partial: &str) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();
    let mut items: Vec<CompletionItem> = index
        .all_symbols()
        .into_iter()
        .filter(|(_, sym)| {
            matches!(
                sym.kind,
                SymbolKind::Class
                    | SymbolKind::Module
                    | SymbolKind::Struct
                    | SymbolKind::Enum
                    | SymbolKind::Constant
                    | SymbolKind::Type
                    | SymbolKind::Method
                    | SymbolKind::Macro
                    | SymbolKind::Function
            )
        })
        .filter(|(_, sym)| {
            if partial.is_empty() {
                true
            } else {
                sym.name.starts_with(partial)
            }
        })
        .filter(|(_, sym)| seen.insert((sym.name.clone(), sym.kind)))
        .map(|(_, sym)| symbol_to_completion(sym))
        .collect();

    // Limit results for performance.
    items.truncate(100);
    items
}

fn symbol_to_completion(sym: &Symbol) -> CompletionItem {
    let detail = sym.parent.as_ref().map(|p| format!("({})", p));

    CompletionItem {
        label: sym.name.clone(),
        kind: Some(symbol_kind_to_completion_kind(sym.kind)),
        detail,
        ..Default::default()
    }
}

fn symbol_kind_to_completion_kind(kind: SymbolKind) -> CompletionItemKind {
    match kind {
        SymbolKind::Class => CompletionItemKind::CLASS,
        SymbolKind::Module => CompletionItemKind::MODULE,
        SymbolKind::Struct => CompletionItemKind::STRUCT,
        SymbolKind::Enum => CompletionItemKind::ENUM,
        SymbolKind::Lib => CompletionItemKind::MODULE,
        SymbolKind::Method => CompletionItemKind::METHOD,
        SymbolKind::Macro => CompletionItemKind::KEYWORD,
        SymbolKind::Function => CompletionItemKind::FUNCTION,
        SymbolKind::Constant => CompletionItemKind::CONSTANT,
        SymbolKind::Type => CompletionItemKind::TYPE_PARAMETER,
        SymbolKind::Field => CompletionItemKind::FIELD,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::DocumentIndex;
    use lsp_types::Position;

    fn make_params(path: &str, line: u32, character: u32) -> CompletionParams {
        let uri = uri::from_path(Path::new(path)).unwrap();
        CompletionParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        }
    }

    fn extract_labels(resp: Option<CompletionResponse>) -> Vec<String> {
        match resp {
            Some(CompletionResponse::List(list)) => {
                list.items.into_iter().map(|i| i.label).collect()
            }
            other => panic!("expected List, got {:?}", other),
        }
    }

    fn extract_items(resp: Option<CompletionResponse>) -> Vec<CompletionItem> {
        match resp {
            Some(CompletionResponse::List(list)) => list.items,
            other => panic!("expected List, got {:?}", other),
        }
    }

    #[test]
    fn test_dot_completion_typed_receiver() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/user.cr"),
            "class User\n  def name\n  end\n  def email\n  end\nend\n",
        );

        let source = "u = User.new\nu.\n";
        let params = make_params("/tmp/comp.cr", 1, 2);
        let labels = extract_labels(handle(&index, params, Some(source)));

        // "User." should show User's methods
        // But "u." — we can't infer the type, so fallback to all methods
        assert!(labels.contains(&"name".to_string()));
        assert!(labels.contains(&"email".to_string()));
    }

    #[test]
    fn test_dot_completion_class_receiver() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/user.cr"),
            "class User\n  def name\n  end\n  def email\n  end\nend\n",
        );

        let source = "User.\n";
        let params = make_params("/tmp/comp.cr", 0, 5);
        let labels = extract_labels(handle(&index, params, Some(source)));

        assert!(labels.contains(&"name".to_string()));
        assert!(labels.contains(&"email".to_string()));
    }

    #[test]
    fn test_scope_completion() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/http.cr"),
            "module HTTP\n  class Client\n  end\n  VERSION = \"1.0\"\nend\n",
        );

        let source = "HTTP::\n";
        let params = make_params("/tmp/comp.cr", 0, 6);
        let labels = extract_labels(handle(&index, params, Some(source)));

        assert!(labels.contains(&"Client".to_string()));
        assert!(labels.contains(&"VERSION".to_string()));
    }

    #[test]
    fn test_general_completion_with_prefix() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/types.cr"),
            "class UserProfile\nend\nclass UserSession\nend\nclass Order\nend\n",
        );

        let source = "User\n";
        let params = make_params("/tmp/comp.cr", 0, 4);
        let labels = extract_labels(handle(&index, params, Some(source)));

        assert!(labels.contains(&"UserProfile".to_string()));
        assert!(labels.contains(&"UserSession".to_string()));
        assert!(!labels.contains(&"Order".to_string()));
    }

    #[test]
    fn test_completion_item_kinds() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/kinds.cr"),
            "class Foo\n  def bar\n  end\nend\nMAX = 10\n",
        );

        let source = "\n";
        let params = make_params("/tmp/comp.cr", 0, 0);
        let items = extract_items(handle(&index, params, Some(source)));

        let foo = items.iter().find(|i| i.label == "Foo").unwrap();
        assert_eq!(foo.kind, Some(CompletionItemKind::CLASS));

        let bar = items.iter().find(|i| i.label == "bar").unwrap();
        assert_eq!(bar.kind, Some(CompletionItemKind::METHOD));

        let max = items.iter().find(|i| i.label == "MAX").unwrap();
        assert_eq!(max.kind, Some(CompletionItemKind::CONSTANT));
    }

    #[test]
    fn test_completion_detail_shows_parent() {
        let mut index = DocumentIndex::new();
        index.update_file(
            Path::new("/tmp/parent.cr"),
            "class Foo\n  def bar\n  end\nend\n",
        );

        let source = "Foo.\n";
        let params = make_params("/tmp/comp.cr", 0, 4);
        let items = extract_items(handle(&index, params, Some(source)));

        let bar = items.iter().find(|i| i.label == "bar").unwrap();
        assert_eq!(bar.detail, Some("(Foo)".to_string()));
    }

    #[test]
    fn test_require_completion() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src/models")).unwrap();
        std::fs::write(tmp.path().join("src/app.cr"), "").unwrap();
        std::fs::write(tmp.path().join("src/models/user.cr"), "").unwrap();
        std::fs::write(tmp.path().join("src/models/post.cr"), "").unwrap();

        let index = DocumentIndex::new();
        let app_path = tmp.path().join("src/app.cr");
        let source = "require \"./models/\n";
        let params = make_params(app_path.to_str().unwrap(), 0, 18);
        let labels = extract_labels(handle(&index, params, Some(source)));

        assert!(labels.contains(&"user".to_string()));
        assert!(labels.contains(&"post".to_string()));
    }

    #[test]
    fn test_detect_dot_completion() {
        assert_eq!(detect_dot_completion("user."), Some("user".to_string()));
        assert_eq!(detect_dot_completion("User."), Some("User".to_string()));
        assert_eq!(detect_dot_completion("  foo."), Some("foo".to_string()));
        assert_eq!(detect_dot_completion("abc"), None);
    }

    #[test]
    fn test_detect_scope_completion() {
        assert_eq!(
            detect_scope_completion("HTTP::"),
            Some("HTTP".to_string())
        );
        assert_eq!(detect_scope_completion("Foo::"), Some("Foo".to_string()));
        assert_eq!(detect_scope_completion("foo."), None);
    }

    #[test]
    fn test_detect_require_completion() {
        assert_eq!(
            detect_require_completion("require \"./models/"),
            Some("./models/".to_string())
        );
        assert_eq!(
            detect_require_completion("require \"json"),
            Some("json".to_string())
        );
        // Closed string — no completion.
        assert_eq!(detect_require_completion("require \"json\""), None);
    }
}

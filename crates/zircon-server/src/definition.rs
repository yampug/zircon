use std::path::Path;

use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location, Position, Range};
use tree_sitter::{Parser, Point};

use crate::index::{DocumentIndex, Symbol, SymbolKind};
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

    // Try type-aware resolution for method calls on typed receivers.
    if let Some(typed_results) = try_type_aware_resolution(index, source, &tree, node) {
        let mut results = typed_results;
        sort_results(&mut results, &path);
        let locations = results_to_locations(&results);
        if !locations.is_empty() {
            return Some(GotoDefinitionResponse::Array(locations));
        }
    }

    // Fall back to name-based search.
    let (name, kinds) = classify_node(node, source)?;

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

    sort_results(&mut results, &path);
    let locations = results_to_locations(&results);
    Some(GotoDefinitionResponse::Array(locations))
}

/// Sort definition results: current file first, then alphabetically.
fn sort_results(results: &mut [(&Path, &Symbol)], current_path: &Path) {
    results.sort_by(|a, b| {
        let a_local = a.0 == current_path;
        let b_local = b.0 == current_path;
        b_local.cmp(&a_local).then_with(|| a.0.cmp(b.0))
    });
}

/// Convert symbol results to LSP Location objects.
fn results_to_locations(results: &[(&Path, &Symbol)]) -> Vec<Location> {
    results
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
        .collect()
}

// ---------------------------------------------------------------------------
// Type-aware resolution
// ---------------------------------------------------------------------------

/// Attempt to resolve a method call by inferring the receiver's type and
/// searching the class hierarchy. Returns `None` if the node is not a method
/// call on a receiver, or if the type cannot be inferred.
fn try_type_aware_resolution<'a>(
    index: &'a DocumentIndex,
    source: &str,
    _tree: &tree_sitter::Tree,
    node: tree_sitter::Node,
) -> Option<Vec<(&'a Path, &'a Symbol)>> {
    let parent = node.parent()?;
    if parent.kind() != "call" {
        return None;
    }

    // Make sure the cursor node is the method name, not the receiver.
    let method_node = parent.child_by_field_name("method")?;
    if method_node.id() != node.id() {
        return None;
    }

    let receiver_node = parent.child_by_field_name("receiver")?;
    let method_name = node.utf8_text(source.as_bytes()).ok()?;

    let type_name = infer_receiver_type(receiver_node, source)?;
    let results = index.find_method_in_hierarchy(&type_name, method_name);
    if results.is_empty() {
        return None; // fall back to name-based search
    }
    Some(results)
}

/// Infer the type of a receiver node.
fn infer_receiver_type(
    receiver_node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    let text = receiver_node.utf8_text(source.as_bytes()).ok()?;

    // Receiver is a constant → direct class/module reference (e.g. User.new).
    if receiver_node.kind() == "constant" || text.chars().next()?.is_uppercase() {
        return Some(text.to_string());
    }

    // Receiver is a local variable → scan the enclosing scope for its type.
    if receiver_node.kind() == "identifier" {
        return infer_variable_type(text, source, receiver_node);
    }

    // Receiver is itself a call ending in .new (e.g. User.new.to_s) — rare.
    if receiver_node.kind() == "call" {
        return extract_constructor_type(receiver_node, source);
    }

    None
}

/// Scan the enclosing scope for a variable's type — from type annotations in
/// method parameters, type declarations, or `Foo.new` assignments.
fn infer_variable_type(
    var_name: &str,
    source: &str,
    context: tree_sitter::Node,
) -> Option<String> {
    let scope = find_enclosing_scope(context)?;

    // Check method parameters first.
    if scope.kind() == "method_def" || scope.kind() == "abstract_method_def" {
        if let Some(params) = scope.child_by_field_name("params") {
            if let Some(t) = find_param_type(params, var_name, source) {
                return Some(t);
            }
        }
    }

    // Walk scope children for assignments / type declarations.
    infer_from_children(scope, var_name, source)
}

fn find_enclosing_scope(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut cur = Some(node);
    while let Some(n) = cur {
        match n.kind() {
            "method_def" | "abstract_method_def" | "class_def" | "module_def"
            | "struct_def" | "block" | "program" | "expressions" => return Some(n),
            _ => cur = n.parent(),
        }
    }
    None
}

fn find_param_type(
    params: tree_sitter::Node,
    var_name: &str,
    source: &str,
) -> Option<String> {
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        if child.kind() != "param" {
            continue;
        }
        let name_node = child.child_by_field_name("name")?;
        let name = name_node.utf8_text(source.as_bytes()).ok()?;
        if name != var_name {
            continue;
        }
        let type_node = child.child_by_field_name("type")?;
        return extract_type_name(type_node, source);
    }
    None
}

fn infer_from_children(
    node: tree_sitter::Node,
    var_name: &str,
    source: &str,
) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "assign" => {
                if let Some(t) = infer_from_assign(child, var_name, source) {
                    return Some(t);
                }
            }
            "type_declaration" => {
                if let Some(t) = infer_from_type_decl(child, var_name, source) {
                    return Some(t);
                }
            }
            _ => {
                // Recurse into blocks / if-else / etc.
                if let Some(t) = infer_from_children(child, var_name, source) {
                    return Some(t);
                }
            }
        }
    }
    None
}

fn infer_from_assign(
    node: tree_sitter::Node,
    var_name: &str,
    source: &str,
) -> Option<String> {
    let lhs = node.child_by_field_name("lhs")?;
    let lhs_text = lhs.utf8_text(source.as_bytes()).ok()?;
    if lhs_text != var_name {
        return None;
    }
    let rhs = node.child_by_field_name("rhs")?;
    extract_constructor_type(rhs, source)
}

fn infer_from_type_decl(
    node: tree_sitter::Node,
    var_name: &str,
    source: &str,
) -> Option<String> {
    let var_node = node.child_by_field_name("var")?;
    let var_text = var_node.utf8_text(source.as_bytes()).ok()?;
    if var_text != var_name {
        return None;
    }
    let type_node = node.child_by_field_name("type")?;
    extract_type_name(type_node, source)
}

/// Extract the class name from a `ClassName.new` call node.
fn extract_constructor_type(
    node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    if node.kind() != "call" {
        return None;
    }
    let method = node.child_by_field_name("method")?;
    let method_text = method.utf8_text(source.as_bytes()).ok()?;
    if method_text != "new" {
        return None;
    }
    let receiver = node.child_by_field_name("receiver")?;
    let name = receiver.utf8_text(source.as_bytes()).ok()?;
    if name.chars().next()?.is_uppercase() {
        Some(name.to_string())
    } else {
        None
    }
}

/// Pull the base type name from a type node, stripping `?` and generics.
fn extract_type_name(
    type_node: tree_sitter::Node,
    source: &str,
) -> Option<String> {
    let text = type_node.utf8_text(source.as_bytes()).ok()?;
    let base = text.trim().trim_end_matches('?');
    let name = base.split('(').next()?.trim();
    if name.chars().next()?.is_uppercase() {
        Some(name.to_string())
    } else {
        None
    }
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

    #[test]
    fn test_type_aware_constructor_resolution() {
        // `user = User.new` followed by `user.name` should resolve to User#name
        // and NOT to an unrelated `name` method on another class.
        let mut index = DocumentIndex::new();
        let file_a = PathBuf::from("/tmp/user.cr");
        let file_b = PathBuf::from("/tmp/pet.cr");
        let file_c = PathBuf::from("/tmp/main.cr");

        let source_a = "class User\n  def name\n    \"alice\"\n  end\nend\n";
        let source_b = "class Pet\n  def name\n    \"fido\"\n  end\nend\n";
        let source_c = "user = User.new\nuser.name\n";

        index.update_file(&file_a, source_a);
        index.update_file(&file_b, source_b);
        index.update_file(&file_c, source_c);

        let mut parser = make_parser();
        // Cursor on "name" in `user.name` at line 1, col 5
        let params = make_params("/tmp/main.cr", 1, 5);

        let result = handle(&index, &mut parser, params, Some(source_c));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                assert_eq!(locs.len(), 1, "should find exactly one definition");
                let uri_a = uri::from_path(&file_a).unwrap();
                assert_eq!(locs[0].uri, uri_a, "should resolve to User#name");
                assert_eq!(locs[0].range.start.line, 1);
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn test_type_aware_superclass_resolution() {
        // `child.greet` where Child < Parent should resolve to Parent#greet
        let mut index = DocumentIndex::new();
        let file_a = PathBuf::from("/tmp/parent.cr");
        let file_b = PathBuf::from("/tmp/child.cr");
        let file_c = PathBuf::from("/tmp/run.cr");

        let source_a = "class Parent\n  def greet\n    \"hello\"\n  end\nend\n";
        let source_b = "class Child < Parent\nend\n";
        let source_c = "c = Child.new\nc.greet\n";

        index.update_file(&file_a, source_a);
        index.update_file(&file_b, source_b);
        index.update_file(&file_c, source_c);

        let mut parser = make_parser();
        // Cursor on "greet" in `c.greet` at line 1, col 2
        let params = make_params("/tmp/run.cr", 1, 2);

        let result = handle(&index, &mut parser, params, Some(source_c));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                assert!(!locs.is_empty(), "should find inherited method");
                let uri_a = uri::from_path(&file_a).unwrap();
                assert_eq!(locs[0].uri, uri_a, "should resolve to Parent#greet");
                assert_eq!(locs[0].range.start.line, 1);
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn test_type_aware_include_resolution() {
        // `obj.say_hi` where Greeter includes Greetable should resolve
        // to Greetable#say_hi
        let mut index = DocumentIndex::new();
        let file_a = PathBuf::from("/tmp/greetable.cr");
        let file_b = PathBuf::from("/tmp/greeter.cr");
        let file_c = PathBuf::from("/tmp/use.cr");

        let source_a = "module Greetable\n  def say_hi\n    \"hi\"\n  end\nend\n";
        let source_b = "class Greeter\n  include Greetable\nend\n";
        let source_c = "g = Greeter.new\ng.say_hi\n";

        index.update_file(&file_a, source_a);
        index.update_file(&file_b, source_b);
        index.update_file(&file_c, source_c);

        let mut parser = make_parser();
        // Cursor on "say_hi" in `g.say_hi` at line 1, col 2
        let params = make_params("/tmp/use.cr", 1, 2);

        let result = handle(&index, &mut parser, params, Some(source_c));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                assert!(!locs.is_empty(), "should find included method");
                let uri_a = uri::from_path(&file_a).unwrap();
                assert_eq!(locs[0].uri, uri_a, "should resolve to Greetable#say_hi");
                assert_eq!(locs[0].range.start.line, 1);
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn test_type_aware_fallback() {
        // When receiver type can't be inferred, falls back to name-based search.
        let mut index = DocumentIndex::new();
        let file_a = PathBuf::from("/tmp/defs.cr");
        let file_b = PathBuf::from("/tmp/call.cr");

        let source_a = "class Foo\n  def do_thing\n  end\nend\n";
        // `x` has no visible assignment, so type can't be inferred
        let source_b = "x.do_thing\n";

        index.update_file(&file_a, source_a);
        index.update_file(&file_b, source_b);

        let mut parser = make_parser();
        // Cursor on "do_thing" at line 0, col 2
        let params = make_params("/tmp/call.cr", 0, 2);

        let result = handle(&index, &mut parser, params, Some(source_b));

        match result {
            Some(GotoDefinitionResponse::Array(locs)) => {
                // Should fall back to name-based and still find Foo#do_thing
                assert!(!locs.is_empty(), "fallback should find do_thing");
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }
}


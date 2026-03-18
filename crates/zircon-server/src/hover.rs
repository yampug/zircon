use std::fs;
use std::path::Path;

use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};
use tree_sitter::{Parser, Point};

use crate::definition::classify_node;
use crate::index::{DocumentIndex, Symbol, SymbolKind};
use crate::uri;

/// Handle a `textDocument/hover` request.
pub fn handle(
    index: &DocumentIndex,
    parser: &mut Parser,
    params: HoverParams,
    current_source: Option<&str>,
) -> Option<Hover> {
    let lsp_uri = &params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let current_path = uri::to_path(lsp_uri)?;

    let current_source = current_source?;
    let tree = parser.parse(current_source, None)?;

    let point = Point {
        row: position.line as usize,
        column: position.character as usize,
    };
    let node = tree.root_node().descendant_for_point_range(point, point)?;

    // Check if cursor is on a require string.
    if node.kind() == "literal_content" {
        if let Some(parent) = node.parent() {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "require" {
                    let require_path = node.utf8_text(current_source.as_bytes()).ok()?;
                    return hover_require(require_path, &current_path);
                }
            }
        }
    }

    // Check if cursor is on an instance variable — show type info from ivar index.
    if node.kind() == "instance_var" {
        let ivar_name = node.utf8_text(current_source.as_bytes()).ok()?;
        if let Some(hover) = hover_instance_var(index, ivar_name, node, current_source) {
            return Some(hover);
        }
    }

    let (name, kinds) = classify_node(node, current_source)?;

    // Search the index for definitions.
    let mut results: Vec<(&Path, &Symbol)> = Vec::new();
    if kinds.is_empty() {
        results = index.find_by_name(&name);
    } else {
        for kind in &kinds {
            results.extend(index.find_definition(&name, *kind));
        }
    }

    // Take the first result (prefer current file).
    results.sort_by(|a, b| {
        let a_local = a.0 == current_path.as_path();
        let b_local = b.0 == current_path.as_path();
        b_local.cmp(&a_local).then_with(|| a.0.cmp(b.0))
    });

    let (def_path, sym) = results.first()?;

    // Read the definition file's source to extract context.
    let def_source = if *def_path == current_path.as_path() {
        current_source.to_string()
    } else {
        fs::read_to_string(def_path).ok()?
    };

    let markdown = build_hover_content(sym, &def_source);

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: None,
    })
}

/// Build Markdown hover content for a symbol by reading its definition source.
fn build_hover_content(sym: &Symbol, source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let def_line = sym.start_line;

    match sym.kind {
        SymbolKind::Method | SymbolKind::Function | SymbolKind::Macro => {
            let signature = extract_signature(&lines, def_line);
            let doc = extract_doc_comment(&lines, def_line);
            format_method_hover(&signature, &doc, sym)
        }
        SymbolKind::Class | SymbolKind::Module | SymbolKind::Struct | SymbolKind::Enum
        | SymbolKind::Lib => {
            let def = get_line(&lines, def_line).unwrap_or_default();
            let doc = extract_doc_comment(&lines, def_line);
            format_hover(&def, &doc)
        }
        SymbolKind::Constant => {
            let def = get_line(&lines, def_line).unwrap_or_default();
            format_hover(&def, &None)
        }
        SymbolKind::Type => {
            let def = get_line(&lines, def_line).unwrap_or_default();
            let doc = extract_doc_comment(&lines, def_line);
            format_hover(&def, &doc)
        }
        SymbolKind::Field => {
            let def = get_line(&lines, def_line).unwrap_or_default();
            format_hover(&def, &None)
        }
    }
}

/// Extract a method/macro signature. Includes the def line, and if it spans
/// multiple lines (e.g., multi-line params), continues until we see the closing
/// `)` or the body starts.
fn extract_signature(lines: &[&str], start: usize) -> String {
    let first = get_line(lines, start).unwrap_or_default();
    let trimmed = first.trim();

    // If the line contains the full signature, return it.
    if trimmed.contains(')') || !trimmed.contains('(') {
        return first;
    }

    // Multi-line signature: collect until closing paren.
    let mut sig = first.clone();
    for i in (start + 1)..lines.len() {
        let line = lines[i].trim();
        sig.push('\n');
        sig.push_str(lines[i]);
        if line.contains(')') {
            break;
        }
    }
    sig
}

/// Extract doc comments (lines starting with `#`) immediately above `def_line`.
fn extract_doc_comment(lines: &[&str], def_line: usize) -> Option<String> {
    let mut comments = Vec::new();
    let mut i = def_line;

    while i > 0 {
        i -= 1;
        let trimmed = lines[i].trim();
        if trimmed.starts_with('#') {
            // Strip the leading `# ` or `#`.
            let text = trimmed.strip_prefix("# ").unwrap_or(
                trimmed.strip_prefix('#').unwrap_or(trimmed),
            );
            comments.push(text.to_string());
        } else {
            break;
        }
    }

    if comments.is_empty() {
        return None;
    }

    comments.reverse();
    Some(comments.join("\n"))
}

fn get_line(lines: &[&str], index: usize) -> Option<String> {
    lines.get(index).map(|l| l.trim().to_string())
}

/// Format hover for a method, appending inferred return type when there is no
/// explicit annotation.  Inferred types are shown in italics to distinguish them.
fn format_method_hover(signature: &str, doc: &Option<String>, sym: &Symbol) -> String {
    // Check if the signature already contains an explicit return type (`: Type`
    // after the closing paren or after the method name if no params).
    let has_explicit = signature_has_return_type(signature);

    let display_sig = if !has_explicit {
        if let Some(ref inferred) = sym.return_type {
            format!("{} : {}", signature.trim_end(), inferred)
        } else {
            signature.to_string()
        }
    } else {
        signature.to_string()
    };

    let mut parts = Vec::new();
    if !display_sig.is_empty() {
        parts.push(format!("```crystal\n{}\n```", display_sig));
    }
    if !has_explicit {
        if let Some(ref inferred) = sym.return_type {
            parts.push(format!("*inferred return type: {}*", inferred));
        }
    }
    if let Some(doc) = doc {
        parts.push(doc.clone());
    }
    parts.join("\n\n")
}

/// Heuristic check: does the signature line contain an explicit return type?
/// Looks for `: Type` after `)` or after the method name when there are no parens.
fn signature_has_return_type(sig: &str) -> bool {
    // If there's a closing paren, check for `: ` after it.
    if let Some(paren_pos) = sig.rfind(')') {
        let after = &sig[paren_pos + 1..];
        return after.contains(':');
    }
    // No parens — check for `: ` after the method name (e.g., `def foo : String`).
    // Skip past `def name` and check for `:`.
    if let Some(def_pos) = sig.find("def ") {
        let after_def = &sig[def_pos + 4..];
        // The method name is the first word, check if there's a `:` after it.
        if let Some(space_pos) = after_def.find(|c: char| c.is_whitespace()) {
            let rest = &after_def[space_pos..];
            return rest.trim_start().starts_with(':');
        }
    }
    false
}

fn format_hover(code: &impl AsRef<str>, doc: &Option<String>) -> String {
    let mut parts = Vec::new();
    let code = code.as_ref();
    if !code.is_empty() {
        parts.push(format!("```crystal\n{}\n```", code));
    }
    if let Some(doc) = doc {
        parts.push(doc.clone());
    }
    parts.join("\n\n")
}

/// Build hover content for an instance variable using the ivar index.
fn hover_instance_var(
    index: &DocumentIndex,
    ivar_name: &str,
    node: tree_sitter::Node,
    source: &str,
) -> Option<Hover> {
    // Find the enclosing class to look up the ivar.
    let class_name = find_enclosing_class(node, source)?;
    let ivar = index.find_instance_var(&class_name, ivar_name)?;

    let md = if let Some(ref type_name) = ivar.type_name {
        format!("```crystal\n{} : {}\n```\n\n({})", ivar_name, type_name, class_name)
    } else {
        format!("```crystal\n{}\n```\n\n({})", ivar_name, class_name)
    };

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: None,
    })
}

/// Walk up the AST to find the enclosing class/struct name.
fn find_enclosing_class(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = Some(node);
    while let Some(n) = current {
        if n.kind() == "class_def" || n.kind() == "struct_def" {
            if let Some(name_node) = n.child_by_field_name("name") {
                return name_node.utf8_text(source.as_bytes()).ok().map(String::from);
            }
        }
        current = n.parent();
    }
    None
}

/// Build hover content for a `require` path.
fn hover_require(require_path: &str, current_file: &Path) -> Option<Hover> {
    let from_dir = current_file.parent()?;

    let resolved = if require_path.starts_with("./") || require_path.starts_with("../") {
        let target = from_dir.join(require_path).with_extension("cr");
        if target.exists() {
            target
                .canonicalize()
                .unwrap_or(target)
                .display()
                .to_string()
        } else {
            format!("{} (not found)", require_path)
        }
    } else {
        format!("{} (stdlib/shard)", require_path)
    };

    let md = format!("```\n{}\n```", resolved);

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: md,
        }),
        range: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::DocumentIndex;
    use lsp_types::Position;
    use std::path::PathBuf;

    fn make_parser() -> Parser {
        let lang = tree_sitter_crystal::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&lang).unwrap();
        parser
    }

    fn make_params(path: &str, line: u32, character: u32) -> HoverParams {
        let uri = uri::from_path(Path::new(path)).unwrap();
        HoverParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
        }
    }

    fn extract_md(hover: Option<Hover>) -> String {
        match hover {
            Some(Hover {
                contents: HoverContents::Markup(m),
                ..
            }) => m.value,
            other => panic!("expected Markup hover, got {:?}", other),
        }
    }

    #[test]
    fn test_hover_method_signature() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_method.cr");
        let source = "class Foo\n  def greet(name : String) : String\n    \"hello #{name}\"\n  end\nend\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        // Hover on "greet" call — but we need a call site. Let's hover on the def itself.
        let source2 = "class Foo\n  def greet(name : String) : String\n    \"hello #{name}\"\n  end\n\n  def run\n    greet(\"world\")\n  end\nend\n";
        index.update_file(&path, source2);

        let params = make_params("/tmp/hover_method.cr", 6, 4);
        let md = extract_md(handle(&index, &mut parser, params, Some(source2)));

        assert!(md.contains("def greet(name : String) : String"));
    }

    #[test]
    fn test_hover_class_with_doc_comment() {
        let tmp = tempfile::tempdir().unwrap();
        let def_path = tmp.path().join("user.cr");
        let def_source = "# A user in the system.\n# Has a name and email.\nclass User\n  def name\n  end\nend\n";
        std::fs::write(&def_path, def_source).unwrap();

        let ref_path = tmp.path().join("app.cr");
        let ref_source = "u = User.new\n";

        let mut index = DocumentIndex::new();
        index.update_file(&def_path, def_source);
        index.update_file(&ref_path, ref_source);

        let mut parser = make_parser();
        let params = make_params(ref_path.to_str().unwrap(), 0, 4);
        let md = extract_md(handle(&index, &mut parser, params, Some(ref_source)));

        assert!(md.contains("class User"), "should show class definition");
        assert!(
            md.contains("A user in the system"),
            "should show doc comment, got: {}",
            md
        );
    }

    #[test]
    fn test_hover_constant() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_const.cr");
        let source = "MAX = 100\nputs MAX\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        let params = make_params("/tmp/hover_const.cr", 1, 5);
        let md = extract_md(handle(&index, &mut parser, params, Some(source)));

        assert!(md.contains("MAX = 100"));
    }

    #[test]
    fn test_hover_instance_var() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_ivar.cr");
        let source = "class Foo\n  @name : String = \"\"\n\n  def show\n    @name\n  end\nend\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        let params = make_params("/tmp/hover_ivar.cr", 4, 4);
        let md = extract_md(handle(&index, &mut parser, params, Some(source)));

        assert!(md.contains("@name"));
    }

    #[test]
    fn test_hover_unknown_returns_none() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_unknown.cr");
        let source = "unknown_thing\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        let params = make_params("/tmp/hover_unknown.cr", 0, 5);
        let result = handle(&index, &mut parser, params, Some(source));

        assert!(result.is_none(), "unknown symbol should return None");
    }

    #[test]
    fn test_hover_require_path() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("src/app.cr");
        let user = tmp.path().join("src/models/user.cr");
        std::fs::create_dir_all(tmp.path().join("src/models")).unwrap();
        std::fs::write(&user, "class User\nend\n").unwrap();
        std::fs::write(&app, "require \"./models/user\"\n").unwrap();

        let mut index = DocumentIndex::new();
        index.update_file(&app, "require \"./models/user\"\n");

        let mut parser = make_parser();
        let params = make_params(app.to_str().unwrap(), 0, 12);
        let md = extract_md(handle(
            &index,
            &mut parser,
            params,
            Some("require \"./models/user\"\n"),
        ));

        assert!(md.contains("user.cr"), "should show resolved path, got: {}", md);
    }

    #[test]
    fn test_hover_stdlib_require() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_stdlib.cr");
        let source = "require \"json\"\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        let params = make_params("/tmp/hover_stdlib.cr", 0, 10);
        let md = extract_md(handle(&index, &mut parser, params, Some(source)));

        assert!(md.contains("stdlib/shard"), "should indicate stdlib, got: {}", md);
    }

    #[test]
    fn test_hover_ivar_with_type() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_ivar_type.cr");
        let source = "class User\n  @name : String\n\n  def show\n    @name\n  end\nend\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        // Hover on @name at line 4, col 4
        let params = make_params("/tmp/hover_ivar_type.cr", 4, 4);
        let md = extract_md(handle(&index, &mut parser, params, Some(source)));

        assert!(md.contains("@name : String"), "should show type, got: {}", md);
        assert!(md.contains("User"), "should show class name, got: {}", md);
    }

    #[test]
    fn test_hover_ivar_from_property() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_ivar_prop.cr");
        let source = "class User\n  property name : String\n\n  def show\n    @name\n  end\nend\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        let params = make_params("/tmp/hover_ivar_prop.cr", 4, 4);
        let md = extract_md(handle(&index, &mut parser, params, Some(source)));

        assert!(md.contains("@name : String"), "should show type from property, got: {}", md);
    }

    #[test]
    fn test_hover_ivar_no_type() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_ivar_notype.cr");
        let source = "class Foo\n  @data = something\n\n  def show\n    @data\n  end\nend\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        let params = make_params("/tmp/hover_ivar_notype.cr", 4, 4);
        let md = extract_md(handle(&index, &mut parser, params, Some(source)));

        assert!(md.contains("@data"), "should show ivar name, got: {}", md);
        assert!(md.contains("Foo"), "should show class name, got: {}", md);
    }

    #[test]
    fn test_hover_method_inferred_string() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_infer.cr");
        let source = "class Foo\n  def greet\n    \"hello\"\n  end\n\n  def run\n    greet\n  end\nend\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        // Hover on "greet" call at line 6
        let params = make_params("/tmp/hover_infer.cr", 6, 4);
        let md = extract_md(handle(&index, &mut parser, params, Some(source)));

        assert!(md.contains("def greet"), "should show signature, got: {}", md);
        assert!(md.contains(": String"), "should show inferred String, got: {}", md);
        assert!(md.contains("*inferred"), "should mark as inferred, got: {}", md);
    }

    #[test]
    fn test_hover_method_explicit_type_no_inferred_label() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_explicit.cr");
        let source = "class Foo\n  def greet : String\n    \"hello\"\n  end\n\n  def run\n    greet\n  end\nend\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        let params = make_params("/tmp/hover_explicit.cr", 6, 4);
        let md = extract_md(handle(&index, &mut parser, params, Some(source)));

        assert!(md.contains("def greet : String"), "should show explicit type, got: {}", md);
        assert!(!md.contains("*inferred"), "should NOT show inferred label, got: {}", md);
    }

    #[test]
    fn test_hover_method_inferred_constructor() {
        let mut index = DocumentIndex::new();
        let path = PathBuf::from("/tmp/hover_ctor.cr");
        let source = "class Factory\n  def build\n    Widget.new\n  end\n\n  def run\n    build\n  end\nend\n";
        index.update_file(&path, source);

        let mut parser = make_parser();
        let params = make_params("/tmp/hover_ctor.cr", 6, 4);
        let md = extract_md(handle(&index, &mut parser, params, Some(source)));

        assert!(md.contains(": Widget"), "should infer Widget, got: {}", md);
    }
}

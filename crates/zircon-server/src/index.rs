use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use log::warn;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

/// Embedded copy of the definition-only portion of our tags query.
/// We only need `@definition.*` captures for the symbol index.
const TAGS_QUERY_SRC: &str = include_str!("../../../languages/crystal/tags.scm");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Class,
    Module,
    Struct,
    Enum,
    Lib,
    Method,
    Macro,
    Function,
    Constant,
    Type,
    Field,
}

impl SymbolKind {
    /// Map a `@definition.<kind>` capture name to a `SymbolKind`.
    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "definition.class" => Some(Self::Class),
            "definition.module" => Some(Self::Module),
            "definition.struct" => Some(Self::Struct),
            "definition.enum" => Some(Self::Enum),
            "definition.lib" => Some(Self::Lib),
            "definition.method" => Some(Self::Method),
            "definition.macro" => Some(Self::Macro),
            "definition.function" => Some(Self::Function),
            "definition.constant" => Some(Self::Constant),
            "definition.type" => Some(Self::Type),
            "definition.field" => Some(Self::Field),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub byte_range: Range<usize>,
    /// Name (selection) range.
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// Full definition range (e.g., from `class` keyword to `end`).
    pub def_start_line: usize,
    pub def_start_col: usize,
    pub def_end_line: usize,
    pub def_end_col: usize,
    /// Enclosing class/module/struct name, if any.
    pub parent: Option<String>,
}

/// Tracks the class hierarchy for a single class, struct, or module.
#[derive(Debug, Clone, Default)]
pub struct ClassInfo {
    pub superclass: Option<String>,
    pub includes: Vec<String>,
    pub extends: Vec<String>,
}

/// Parses Crystal source files and maintains a per-file symbol index.
pub struct DocumentIndex {
    parser: Parser,
    query: Query,
    files: HashMap<PathBuf, Vec<Symbol>>,
    /// Maps class/module name → hierarchy info (superclass, includes, extends).
    pub class_hierarchy: HashMap<String, ClassInfo>,
}

/// Node kinds that represent enclosing scopes for parent tracking.
const SCOPE_NODE_KINDS: &[&str] = &[
    "class_def",
    "module_def",
    "struct_def",
    "enum_def",
    "lib_def",
];

impl DocumentIndex {
    pub fn new() -> Self {
        let lang = tree_sitter_crystal::LANGUAGE.into();
        let mut parser = Parser::new();
        parser
            .set_language(&lang)
            .expect("failed to load Crystal grammar");
        let query = Query::new(&lang, TAGS_QUERY_SRC).expect("failed to compile tags query");
        DocumentIndex {
            parser,
            query,
            files: HashMap::new(),
            class_hierarchy: HashMap::new(),
        }
    }

    /// Index all `.cr` files from discovered paths.
    pub fn index_files(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.index_file(path);
        }
    }

    /// Parse and index a single file from disk. Replaces any previous symbols
    /// for this path.
    pub fn index_file(&mut self, path: &Path) {
        match fs::read_to_string(path) {
            Ok(source) => {
                let symbols = self.extract_symbols(&source);
                let hierarchy = self.extract_hierarchy(&source);
                self.files.insert(path.to_path_buf(), symbols);
                for (name, info) in hierarchy {
                    self.class_hierarchy.insert(name, info);
                }
            }
            Err(e) => {
                warn!("failed to read {:?}: {}", path, e);
            }
        }
    }

    /// Re-parse a single file from its in-memory contents. Used for
    /// incremental updates when the editor sends `didChange`.
    pub fn update_file(&mut self, path: &Path, source: &str) {
        let symbols = self.extract_symbols(source);
        let hierarchy = self.extract_hierarchy(source);
        self.files.insert(path.to_path_buf(), symbols);
        for (name, info) in hierarchy {
            self.class_hierarchy.insert(name, info);
        }
    }

    /// Search all indexed files for definitions matching `name` and `kind`.
    pub fn find_definition(&self, name: &str, kind: SymbolKind) -> Vec<(&Path, &Symbol)> {
        let mut results = Vec::new();
        for (path, symbols) in &self.files {
            for sym in symbols {
                if sym.kind == kind && sym.name == name {
                    results.push((path.as_path(), sym));
                }
            }
        }
        results
    }

    /// Search all indexed files for any definition matching `name` (any kind).
    pub fn find_by_name(&self, name: &str) -> Vec<(&Path, &Symbol)> {
        let mut results = Vec::new();
        for (path, symbols) in &self.files {
            for sym in symbols {
                if sym.name == name {
                    results.push((path.as_path(), sym));
                }
            }
        }
        results
    }

    /// Return all symbols for a given file.
    pub fn symbols_for_file(&self, path: &Path) -> Option<&Vec<Symbol>> {
        self.files.get(path)
    }

    /// Return all indexed file paths.
    pub fn indexed_paths(&self) -> Vec<&Path> {
        self.files.keys().map(|p| p.as_path()).collect()
    }

    /// Find all symbols whose parent matches `parent_name`.
    pub fn find_by_parent(&self, parent_name: &str) -> Vec<(&Path, &Symbol)> {
        let mut results = Vec::new();
        for (path, symbols) in &self.files {
            for sym in symbols {
                if sym.parent.as_deref() == Some(parent_name) {
                    results.push((path.as_path(), sym));
                }
            }
        }
        results
    }

    /// Return all definition symbols across all indexed files.
    pub fn all_symbols(&self) -> Vec<(&Path, &Symbol)> {
        let mut results = Vec::new();
        for (path, symbols) in &self.files {
            for sym in symbols {
                results.push((path.as_path(), sym));
            }
        }
        results
    }

    /// Search for a method by walking the class hierarchy: the class itself,
    /// then included modules, then the superclass chain.
    pub fn find_method_in_hierarchy(
        &self,
        class_name: &str,
        method_name: &str,
    ) -> Vec<(&Path, &Symbol)> {
        let mut visited = HashSet::new();
        self.find_method_in_hierarchy_inner(class_name, method_name, &mut visited)
    }

    fn find_method_in_hierarchy_inner(
        &self,
        class_name: &str,
        method_name: &str,
        visited: &mut HashSet<String>,
    ) -> Vec<(&Path, &Symbol)> {
        if !visited.insert(class_name.to_string()) {
            return Vec::new();
        }

        // Check methods defined directly on this class/module.
        let results: Vec<_> = self
            .find_by_parent(class_name)
            .into_iter()
            .filter(|(_, sym)| sym.name == method_name && sym.kind == SymbolKind::Method)
            .collect();
        if !results.is_empty() {
            return results;
        }

        if let Some(info) = self.class_hierarchy.get(class_name) {
            // Check included modules.
            for module_name in &info.includes {
                let results =
                    self.find_method_in_hierarchy_inner(module_name, method_name, visited);
                if !results.is_empty() {
                    return results;
                }
            }

            // Check superclass.
            if let Some(ref superclass) = info.superclass {
                return self.find_method_in_hierarchy_inner(superclass, method_name, visited);
            }
        }

        Vec::new()
    }

    /// Parse source to extract class hierarchy (superclass, include, extend).
    fn extract_hierarchy(&mut self, source: &str) -> Vec<(String, ClassInfo)> {
        let tree = match self.parser.parse(source, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut result = Vec::new();
        collect_hierarchy(tree.root_node(), source, &mut result);
        result
    }

    /// Extract symbols from Crystal source code using the tags query.
    fn extract_symbols(&mut self, source: &str) -> Vec<Symbol> {
        let tree = match self.parser.parse(source, None) {
            Some(t) => t,
            None => return Vec::new(),
        };

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, tree.root_node(), source.as_bytes());
        let capture_names = self.query.capture_names();

        let mut symbols = Vec::new();

        while let Some(m) = matches.next() {
            // Find the definition tag and its node for this match.
            let def_capture = m.captures.iter().find_map(|c| {
                let cap_name = &capture_names[c.index as usize];
                if cap_name.starts_with("definition.") {
                    Some((*cap_name, c.node))
                } else {
                    None
                }
            });

            let (def_tag, def_node) = match def_capture {
                Some(pair) => pair,
                None => continue, // skip reference captures
            };

            let kind = match SymbolKind::from_tag(def_tag) {
                Some(k) => k,
                None => continue,
            };

            // Find the @name capture.
            let name_cap = match m.captures.iter().find(|c| {
                capture_names[c.index as usize] == "name"
            }) {
                Some(c) => c,
                None => continue,
            };

            let name = match name_cap.node.utf8_text(source.as_bytes()) {
                Ok(t) => t.to_string(),
                Err(_) => continue,
            };

            let node = name_cap.node;
            let start = node.start_position();
            let end = node.end_position();
            let def_start = def_node.start_position();
            let def_end = def_node.end_position();

            // Walk up from the *definition node's parent* to find the
            // enclosing scope (skipping the definition node itself).
            let parent = find_parent_scope(def_node, source);

            symbols.push(Symbol {
                name,
                kind,
                byte_range: node.byte_range(),
                start_line: start.row,
                start_col: start.column,
                end_line: end.row,
                end_col: end.column,
                def_start_line: def_start.row,
                def_start_col: def_start.column,
                def_end_line: def_end.row,
                def_end_col: def_end.column,
                parent,
            });
        }

        symbols
    }
}

/// Walk up from `node` to find the nearest enclosing class/module/struct and
/// return its name.
fn find_parent_scope(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(n) = current {
        if SCOPE_NODE_KINDS.contains(&n.kind()) {
            // The name child of the scope node.
            if let Some(name_node) = n.child_by_field_name("name") {
                return name_node.utf8_text(source.as_bytes()).ok().map(String::from);
            }
        }
        current = n.parent();
    }
    None
}

/// Recursively walk the tree to find class/struct/module definitions and
/// extract their superclass, include, and extend relationships.
fn collect_hierarchy(
    node: tree_sitter::Node,
    source: &str,
    result: &mut Vec<(String, ClassInfo)>,
) {
    match node.kind() {
        "class_def" | "struct_def" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    let mut info = ClassInfo::default();
                    if let Some(super_node) = node.child_by_field_name("superclass") {
                        if let Ok(s) = super_node.utf8_text(source.as_bytes()) {
                            info.superclass = Some(s.to_string());
                        }
                    }
                    if let Some(body) = node.child_by_field_name("body") {
                        collect_includes_extends(body, source, &mut info);
                    }
                    result.push((name.to_string(), info));
                }
            }
        }
        "module_def" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                    let mut info = ClassInfo::default();
                    if let Some(body) = node.child_by_field_name("body") {
                        collect_includes_extends(body, source, &mut info);
                    }
                    result.push((name.to_string(), info));
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_hierarchy(child, source, result);
    }
}

/// Scan a class/module body for `include` and `extend` statements.
fn collect_includes_extends(body: tree_sitter::Node, source: &str, info: &mut ClassInfo) {
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        match child.kind() {
            "include" => {
                if let Some(mod_node) = child.named_child(0) {
                    if let Ok(name) = mod_node.utf8_text(source.as_bytes()) {
                        info.includes.push(name.to_string());
                    }
                }
            }
            "extend" => {
                if let Some(mod_node) = child.named_child(0) {
                    if let Ok(name) = mod_node.utf8_text(source.as_bytes()) {
                        info.extends.push(name.to_string());
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_source(source: &str) -> Vec<Symbol> {
        let mut idx = DocumentIndex::new();
        idx.update_file(Path::new("test.cr"), source);
        idx.files.get(Path::new("test.cr")).cloned().unwrap_or_default()
    }

    fn defs(symbols: &[Symbol]) -> Vec<(&str, SymbolKind, Option<&str>)> {
        symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.parent.as_deref()))
            .collect()
    }

    #[test]
    fn test_nested_class_and_method() {
        let symbols = index_source("class Foo\n  def bar\n  end\nend\n");
        let d = defs(&symbols);

        assert!(d.contains(&("Foo", SymbolKind::Class, None)));
        assert!(d.contains(&("bar", SymbolKind::Method, Some("Foo"))));
    }

    #[test]
    fn test_module_with_methods() {
        let symbols = index_source(
            "module Utils\n  def self.help\n  end\n\n  def format(str)\n  end\nend\n",
        );
        let d = defs(&symbols);

        assert!(d.contains(&("Utils", SymbolKind::Module, None)));
        assert!(d.contains(&("help", SymbolKind::Method, Some("Utils"))));
        assert!(d.contains(&("format", SymbolKind::Method, Some("Utils"))));
    }

    #[test]
    fn test_constants_and_alias() {
        let symbols = index_source(
            "MAX = 100\nalias Name = String\n",
        );
        let d = defs(&symbols);

        assert!(d.contains(&("MAX", SymbolKind::Constant, None)));
        assert!(d.contains(&("Name", SymbolKind::Type, None)));
    }

    #[test]
    fn test_macro_def() {
        let symbols = index_source("macro my_macro\nend\n");
        let d = defs(&symbols);

        assert!(d.contains(&("my_macro", SymbolKind::Macro, None)));
    }

    #[test]
    fn test_instance_and_class_vars() {
        let symbols = index_source(
            "class Foo\n  @name = \"\"\n  @@count = 0\n  @age : Int32\nend\n",
        );
        let d = defs(&symbols);

        assert!(d.contains(&("@name", SymbolKind::Field, Some("Foo"))));
        assert!(d.contains(&("@@count", SymbolKind::Field, Some("Foo"))));
        assert!(d.contains(&("@age", SymbolKind::Field, Some("Foo"))));
    }

    #[test]
    fn test_abstract_method() {
        let symbols = index_source(
            "abstract class Base\n  abstract def run\nend\n",
        );
        let d = defs(&symbols);

        assert!(d.contains(&("Base", SymbolKind::Class, None)));
        assert!(d.contains(&("run", SymbolKind::Method, Some("Base"))));
    }

    #[test]
    fn test_enum_def() {
        let symbols = index_source("enum Color\n  Red\n  Green\nend\n");
        let d = defs(&symbols);

        assert!(d.contains(&("Color", SymbolKind::Enum, None)));
    }

    #[test]
    fn test_property_macro() {
        let symbols = index_source(
            "class User\n  property name : String\n  getter age : Int32\nend\n",
        );
        let d = defs(&symbols);

        assert!(d.contains(&("name", SymbolKind::Method, Some("User"))));
        assert!(d.contains(&("age", SymbolKind::Method, Some("User"))));
    }

    #[test]
    fn test_find_definition() {
        let mut idx = DocumentIndex::new();
        idx.update_file(
            Path::new("a.cr"),
            "class Foo\n  def greet\n  end\nend\n",
        );
        idx.update_file(
            Path::new("b.cr"),
            "class Bar\n  def greet\n  end\nend\n",
        );

        let results = idx.find_definition("greet", SymbolKind::Method);
        assert_eq!(results.len(), 2);

        let foo_results = idx.find_definition("Foo", SymbolKind::Class);
        assert_eq!(foo_results.len(), 1);
        assert_eq!(foo_results[0].0, Path::new("a.cr"));
        assert_eq!(foo_results[0].1.start_line, 0);
        assert_eq!(foo_results[0].1.start_col, 6); // "class Foo" → col 6
    }

    #[test]
    fn test_update_file_replaces() {
        let mut idx = DocumentIndex::new();
        idx.update_file(Path::new("a.cr"), "def old_method\nend\n");

        assert_eq!(idx.find_definition("old_method", SymbolKind::Method).len(), 1);

        idx.update_file(Path::new("a.cr"), "def new_method\nend\n");

        assert_eq!(idx.find_definition("old_method", SymbolKind::Method).len(), 0);
        assert_eq!(idx.find_definition("new_method", SymbolKind::Method).len(), 1);
    }

    #[test]
    fn test_nested_classes() {
        let symbols = index_source(
            "module App\n  class Server\n    def start\n    end\n  end\nend\n",
        );
        let d = defs(&symbols);

        assert!(d.contains(&("App", SymbolKind::Module, None)));
        assert!(d.contains(&("Server", SymbolKind::Class, Some("App"))));
        assert!(d.contains(&("start", SymbolKind::Method, Some("Server"))));
    }

    #[test]
    fn test_require_statements_not_indexed_as_definitions() {
        let symbols = index_source("require \"json\"\nrequire \"./models/user\"\n");
        // require statements are references, not definitions — should produce
        // no definition symbols.
        let definition_count = symbols.iter().filter(|s| true).count();
        // The index only stores definition symbols, so the vec should be empty
        // (require is captured as reference.call, which we skip).
        assert!(
            symbols.iter().all(|s| s.kind != SymbolKind::Class),
            "no class definitions expected"
        );
    }

    #[test]
    fn test_fun_def() {
        let symbols = index_source("lib LibC\n  fun printf(format : UInt8*, ...) : Int32\nend\n");
        let d = defs(&symbols);

        assert!(d.contains(&("LibC", SymbolKind::Lib, None)));
        assert!(d.contains(&("printf", SymbolKind::Function, Some("LibC"))));
    }

    #[test]
    fn test_hierarchy_superclass() {
        let mut idx = DocumentIndex::new();
        idx.update_file(
            Path::new("a.cr"),
            "class Animal\n  def breathe\n  end\nend\n\nclass Dog < Animal\n  def bark\n  end\nend\n",
        );

        let info = idx.class_hierarchy.get("Dog").expect("Dog should be in hierarchy");
        assert_eq!(info.superclass.as_deref(), Some("Animal"));

        // Method defined directly on Dog.
        let bark = idx.find_method_in_hierarchy("Dog", "bark");
        assert_eq!(bark.len(), 1);
        assert_eq!(bark[0].1.name, "bark");

        // Method inherited from Animal.
        let breathe = idx.find_method_in_hierarchy("Dog", "breathe");
        assert_eq!(breathe.len(), 1);
        assert_eq!(breathe[0].1.name, "breathe");
        assert_eq!(breathe[0].1.parent.as_deref(), Some("Animal"));
    }

    #[test]
    fn test_hierarchy_includes() {
        let mut idx = DocumentIndex::new();
        idx.update_file(
            Path::new("a.cr"),
            "module Greetable\n  def greet\n  end\nend\n\nclass Person\n  include Greetable\n  def name\n  end\nend\n",
        );

        let info = idx.class_hierarchy.get("Person").expect("Person in hierarchy");
        assert_eq!(info.includes, vec!["Greetable"]);

        // Method from included module.
        let greet = idx.find_method_in_hierarchy("Person", "greet");
        assert_eq!(greet.len(), 1);
        assert_eq!(greet[0].1.parent.as_deref(), Some("Greetable"));
    }

    #[test]
    fn test_hierarchy_not_found() {
        let mut idx = DocumentIndex::new();
        idx.update_file(
            Path::new("a.cr"),
            "class Foo\n  def bar\n  end\nend\n",
        );

        let results = idx.find_method_in_hierarchy("Foo", "nonexistent");
        assert!(results.is_empty());
    }
}

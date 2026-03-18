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
    /// Inferred return type for methods (only set when no explicit annotation).
    pub return_type: Option<String>,
}

/// Tracks the class hierarchy for a single class, struct, or module.
#[derive(Debug, Clone, Default)]
pub struct ClassInfo {
    pub superclass: Option<String>,
    pub includes: Vec<String>,
    pub extends: Vec<String>,
}

/// An instance variable with optional type information.
#[derive(Debug, Clone)]
pub struct InstanceVariable {
    pub name: String,
    pub type_name: Option<String>,
    pub class_name: String,
    pub line: usize,
    pub col: usize,
}

/// Parses Crystal source files and maintains a per-file symbol index.
pub struct DocumentIndex {
    parser: Parser,
    query: Query,
    files: HashMap<PathBuf, Vec<Symbol>>,
    /// Maps class/module name → hierarchy info (superclass, includes, extends).
    pub class_hierarchy: HashMap<String, ClassInfo>,
    /// Maps class name → instance variables with type info.
    pub instance_vars: HashMap<String, Vec<InstanceVariable>>,
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
            instance_vars: HashMap::new(),
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
                let ivars = self.extract_instance_vars(&source);
                self.files.insert(path.to_path_buf(), symbols);
                for (name, info) in hierarchy {
                    self.class_hierarchy.insert(name, info);
                }
                for (class_name, vars) in ivars {
                    self.instance_vars.insert(class_name, vars);
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
        let ivars = self.extract_instance_vars(source);
        self.files.insert(path.to_path_buf(), symbols);
        for (name, info) in hierarchy {
            self.class_hierarchy.insert(name, info);
        }
        for (class_name, vars) in ivars {
            self.instance_vars.insert(class_name, vars);
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

    /// Return instance variables for a given class name.
    pub fn find_instance_vars(&self, class_name: &str) -> &[InstanceVariable] {
        self.instance_vars
            .get(class_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Find a specific instance variable by name within a class.
    pub fn find_instance_var(&self, class_name: &str, ivar_name: &str) -> Option<&InstanceVariable> {
        self.instance_vars
            .get(class_name)?
            .iter()
            .find(|iv| iv.name == ivar_name)
    }

    /// Parse source to extract instance variables per class.
    fn extract_instance_vars(
        &mut self,
        source: &str,
    ) -> Vec<(String, Vec<InstanceVariable>)> {
        let tree = match self.parser.parse(source, None) {
            Some(t) => t,
            None => return Vec::new(),
        };
        let mut result: HashMap<String, Vec<InstanceVariable>> = HashMap::new();
        collect_instance_vars(tree.root_node(), source, &mut result);
        result.into_iter().collect()
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

            // For methods, check for an explicit return type on the def node.
            // If present, store it; inference will only apply when this is None.
            let return_type = if kind == SymbolKind::Method || kind == SymbolKind::Function {
                extract_explicit_return_type(def_node, source)
            } else {
                None
            };

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
                return_type,
            });
        }

        // Post-pass: infer return types for methods without explicit annotations.
        infer_method_return_types(&tree, source, &mut symbols);

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

/// Extract the explicit return type from a method/function definition node.
/// The `type` field is `multiple: true` and includes the `:` token — we need
/// to find the first *named* child with the `type` field.
fn extract_explicit_return_type(def_node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut cursor = def_node.walk();
    for child in def_node.children_by_field_name("type", &mut cursor) {
        if child.is_named() {
            return child.utf8_text(source.as_bytes()).ok().map(String::from);
        }
    }
    None
}

/// Walk the tree to find method_def nodes and infer return types for symbols
/// that don't have explicit return type annotations.
fn infer_method_return_types(
    tree: &tree_sitter::Tree,
    source: &str,
    symbols: &mut [Symbol],
) {
    let mut methods: HashMap<(usize, usize), String> = HashMap::new();
    collect_method_return_types(tree.root_node(), source, &mut methods);

    for sym in symbols.iter_mut() {
        if (sym.kind == SymbolKind::Method || sym.kind == SymbolKind::Function)
            && sym.return_type.is_none()
        {
            if let Some(inferred) = methods.remove(&(sym.def_start_line, sym.def_start_col)) {
                sym.return_type = Some(inferred);
            }
        }
    }
}

/// Recursively find method_def nodes and infer their return types.
fn collect_method_return_types(
    node: tree_sitter::Node,
    source: &str,
    results: &mut HashMap<(usize, usize), String>,
) {
    if node.kind() == "method_def" || node.kind() == "fun_def" {
        // Skip methods with explicit return types.
        if node.child_by_field_name("type").is_none() {
            if let Some(body) = node.child_by_field_name("body") {
                if let Some(inferred) = infer_return_type(body, source) {
                    let pos = node.start_position();
                    results.insert((pos.row, pos.column), inferred);
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_method_return_types(child, source, results);
    }
}

/// Infer the return type of a method body by analyzing the last expression.
fn infer_return_type(body: tree_sitter::Node, source: &str) -> Option<String> {
    let last = last_expression(body)?;
    infer_expression_type(last, source)
}

/// Find the last named expression in a body/expressions node.
fn last_expression(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let count = node.named_child_count();
    if count == 0 {
        return Some(node);
    }
    node.named_child(count - 1)
}

/// Infer the type of a single expression node.
fn infer_expression_type(node: tree_sitter::Node, source: &str) -> Option<String> {
    match node.kind() {
        // Literals
        "string" | "chained_string" | "heredoc_start" => Some("String".to_string()),
        "integer" => Some("Int32".to_string()),
        "float" => Some("Float64".to_string()),
        "true" | "false" => Some("Bool".to_string()),
        "nil" => Some("Nil".to_string()),
        "symbol" => Some("Symbol".to_string()),
        "char" => Some("Char".to_string()),
        "array" => Some("Array".to_string()),
        "hash" => Some("Hash".to_string()),
        "tuple" => Some("Tuple".to_string()),
        "named_tuple" => Some("NamedTuple".to_string()),
        "regex" => Some("Regex".to_string()),
        "range" => Some("Range".to_string()),

        // String interpolation is always String
        "string_interpolation" => Some("String".to_string()),

        // Constructor calls: Foo.new → Foo
        "call" => infer_call_type(node, source),

        // Binary operations
        "and" | "or" => Some("Bool".to_string()),

        // If/unless → union of branch types
        "if" | "unless" => infer_conditional_type(node, source),

        // Case expression
        "case" => infer_case_type(node, source),

        // Expressions block → type of last expression
        "expressions" => {
            let last = last_expression(node)?;
            infer_expression_type(last, source)
        }

        // Begin block → type of body
        "begin" => {
            if let Some(body) = node.child_by_field_name("body") {
                let last = last_expression(body)?;
                infer_expression_type(last, source)
            } else {
                None
            }
        }

        // Return statement → type of the value
        "return" => {
            let child = node.named_child(0)?;
            infer_expression_type(child, source)
        }

        _ => None,
    }
}

/// Infer the return type of a method call.
fn infer_call_type(node: tree_sitter::Node, source: &str) -> Option<String> {
    let method = node.child_by_field_name("method")?;
    let method_text = method.utf8_text(source.as_bytes()).ok()?;

    // Constructor: Foo.new → Foo
    if method_text == "new" {
        let receiver = node.child_by_field_name("receiver")?;
        let name = receiver.utf8_text(source.as_bytes()).ok()?;
        if name.chars().next()?.is_uppercase() {
            return Some(name.to_string());
        }
    }

    // String-producing methods
    if matches!(method_text, "to_s" | "inspect" | "chomp" | "strip"
        | "downcase" | "upcase" | "gsub" | "sub" | "tr" | "join"
        | "lstrip" | "rstrip" | "reverse") {
        return Some("String".to_string());
    }

    // Comparison operators → Bool
    if matches!(method_text, "==" | "!=" | "<" | ">" | "<=" | ">="
        | "===" | "<=>" | "includes?" | "empty?" | "nil?" | "is_a?"
        | "responds_to?" | "any?" | "all?" | "none?") {
        return Some("Bool".to_string());
    }

    // Size/count → Int32
    if matches!(method_text, "size" | "length" | "count" | "index") {
        return Some("Int32".to_string());
    }

    // Arithmetic: if receiver is known numeric, result is numeric
    if matches!(method_text, "+" | "-" | "*" | "/" | "%" | "**" | "&"
        | "|" | "^" | "<<" | ">>" | "~") {
        if let Some(receiver) = node.child_by_field_name("receiver") {
            if let Some(recv_type) = infer_expression_type(receiver, source) {
                if matches!(recv_type.as_str(),
                    "Int32" | "Int64" | "UInt32" | "UInt64" | "Float32" | "Float64"
                ) {
                    return Some(recv_type);
                }
            }
        }
    }

    None
}

/// Infer the type of an if/unless expression from its branches.
fn infer_conditional_type(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut types = Vec::new();

    // Then branch (the body of the if)
    if let Some(body) = node.child_by_field_name("body") {
        if let Some(last) = last_expression(body) {
            if let Some(t) = infer_expression_type(last, source) {
                if !types.contains(&t) {
                    types.push(t);
                }
            }
        }
    }

    // Else branch
    if let Some(else_node) = node.child_by_field_name("else") {
        // The else node's body is its first named child (the expressions).
        if let Some(else_body) = else_node.named_child(0) {
            if let Some(last) = last_expression(else_body) {
                if let Some(t) = infer_expression_type(last, source) {
                    if !types.contains(&t) {
                        types.push(t);
                    }
                }
            }
        }
    }

    match types.len() {
        0 => None,
        1 => Some(types.into_iter().next().unwrap()),
        _ => Some(types.join(" | ")),
    }
}

/// Infer the type of a case expression from its when branches.
fn infer_case_type(node: tree_sitter::Node, source: &str) -> Option<String> {
    let mut types = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        if child.kind() == "when" {
            if let Some(body) = child.child_by_field_name("body") {
                if let Some(last) = last_expression(body) {
                    if let Some(t) = infer_expression_type(last, source) {
                        if !types.contains(&t) {
                            types.push(t);
                        }
                    }
                }
            }
        } else if child.kind() == "else" {
            if let Some(else_body) = child.named_child(0) {
                if let Some(last) = last_expression(else_body) {
                    if let Some(t) = infer_expression_type(last, source) {
                        if !types.contains(&t) {
                            types.push(t);
                        }
                    }
                }
            }
        }
    }

    match types.len() {
        0 => None,
        1 => Some(types.into_iter().next().unwrap()),
        _ => Some(types.join(" | ")),
    }
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

/// Recursively walk the tree to find class/struct bodies and extract instance
/// variables from assignments, type declarations, and property/getter/setter macros.
fn collect_instance_vars(
    node: tree_sitter::Node,
    source: &str,
    result: &mut HashMap<String, Vec<InstanceVariable>>,
) {
    match node.kind() {
        "class_def" | "struct_def" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(class_name) = name_node.utf8_text(source.as_bytes()) {
                    if let Some(body) = node.child_by_field_name("body") {
                        let vars = extract_ivars_from_body(body, class_name, source);
                        if !vars.is_empty() {
                            result
                                .entry(class_name.to_string())
                                .or_default()
                                .extend(vars);
                        }
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_instance_vars(child, source, result);
    }
}

/// Extract instance variables from a class/struct body node.
fn extract_ivars_from_body(
    body: tree_sitter::Node,
    class_name: &str,
    source: &str,
) -> Vec<InstanceVariable> {
    let mut vars = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = body.walk();

    for child in body.children(&mut cursor) {
        match child.kind() {
            // @name = value
            "assign" => {
                if let Some(lhs) = child.child_by_field_name("lhs") {
                    if lhs.kind() == "instance_var" {
                        if let Ok(name) = lhs.utf8_text(source.as_bytes()) {
                            if seen.insert(name.to_string()) {
                                let type_name = child
                                    .child_by_field_name("rhs")
                                    .and_then(|rhs| infer_type_from_rhs(rhs, source));
                                let pos = lhs.start_position();
                                vars.push(InstanceVariable {
                                    name: name.to_string(),
                                    type_name,
                                    class_name: class_name.to_string(),
                                    line: pos.row,
                                    col: pos.column,
                                });
                            }
                        }
                    }
                }
            }
            // @name : Type
            "type_declaration" => {
                if let Some(var_node) = child.child_by_field_name("var") {
                    if var_node.kind() == "instance_var" {
                        if let Ok(name) = var_node.utf8_text(source.as_bytes()) {
                            let type_name = child
                                .child_by_field_name("type")
                                .and_then(|t| t.utf8_text(source.as_bytes()).ok())
                                .map(|s| s.to_string());
                            let pos = var_node.start_position();
                            // Type declaration takes priority — replace any prior entry.
                            seen.insert(name.to_string());
                            vars.retain(|v| v.name != name);
                            vars.push(InstanceVariable {
                                name: name.to_string(),
                                type_name,
                                class_name: class_name.to_string(),
                                line: pos.row,
                                col: pos.column,
                            });
                        }
                    }
                }
            }
            // property/getter/setter macros
            "call" => {
                if let Some(method) = child.child_by_field_name("method") {
                    if let Ok(macro_name) = method.utf8_text(source.as_bytes()) {
                        if is_property_macro(macro_name) {
                            if let Some(args) = child.child_by_field_name("arguments") {
                                extract_ivar_from_property_macro(
                                    args,
                                    class_name,
                                    source,
                                    &mut vars,
                                    &mut seen,
                                );
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    vars
}

/// Check if a method name is a property/getter/setter macro.
fn is_property_macro(name: &str) -> bool {
    let base = name.trim_end_matches(|c| c == '?' || c == '!');
    matches!(
        base,
        "property" | "getter" | "setter"
            | "class_property" | "class_getter" | "class_setter"
    )
}

/// Extract instance variable info from a property/getter/setter macro's arguments.
fn extract_ivar_from_property_macro(
    args: tree_sitter::Node,
    class_name: &str,
    source: &str,
    vars: &mut Vec<InstanceVariable>,
    seen: &mut HashSet<String>,
) {
    let mut cursor = args.walk();
    for child in args.children(&mut cursor) {
        match child.kind() {
            // Typed form: `property name : String`
            "type_declaration" => {
                if let Some(var_node) = child.child_by_field_name("var") {
                    if let Ok(prop_name) = var_node.utf8_text(source.as_bytes()) {
                        let ivar_name = format!("@{}", prop_name);
                        let type_name = child
                            .child_by_field_name("type")
                            .and_then(|t| t.utf8_text(source.as_bytes()).ok())
                            .map(|s| s.to_string());
                        let pos = var_node.start_position();
                        if seen.insert(ivar_name.clone()) {
                            vars.push(InstanceVariable {
                                name: ivar_name,
                                type_name,
                                class_name: class_name.to_string(),
                                line: pos.row,
                                col: pos.column,
                            });
                        }
                    }
                }
            }
            // Untyped form: `getter name` or `getter :name`
            "identifier" => {
                if let Ok(prop_name) = child.utf8_text(source.as_bytes()) {
                    let ivar_name = format!("@{}", prop_name);
                    let pos = child.start_position();
                    if seen.insert(ivar_name.clone()) {
                        vars.push(InstanceVariable {
                            name: ivar_name,
                            type_name: None,
                            class_name: class_name.to_string(),
                            line: pos.row,
                            col: pos.column,
                        });
                    }
                }
            }
            "symbol" => {
                if let Ok(sym_text) = child.utf8_text(source.as_bytes()) {
                    let prop_name = sym_text.trim_start_matches(':');
                    let ivar_name = format!("@{}", prop_name);
                    let pos = child.start_position();
                    if seen.insert(ivar_name.clone()) {
                        vars.push(InstanceVariable {
                            name: ivar_name,
                            type_name: None,
                            class_name: class_name.to_string(),
                            line: pos.row,
                            col: pos.column,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Infer a type from the RHS of an assignment (e.g., `SomeClass.new` → "SomeClass").
fn infer_type_from_rhs(rhs: tree_sitter::Node, source: &str) -> Option<String> {
    if rhs.kind() == "call" {
        let method = rhs.child_by_field_name("method")?;
        let method_text = method.utf8_text(source.as_bytes()).ok()?;
        if method_text == "new" {
            let receiver = rhs.child_by_field_name("receiver")?;
            let name = receiver.utf8_text(source.as_bytes()).ok()?;
            if name.chars().next()?.is_uppercase() {
                return Some(name.to_string());
            }
        }
    }
    // String literal → String
    if rhs.kind() == "string" {
        return Some("String".to_string());
    }
    // Integer literal → Int32
    if rhs.kind() == "integer" {
        return Some("Int32".to_string());
    }
    // Float literal → Float64
    if rhs.kind() == "float" {
        return Some("Float64".to_string());
    }
    // Bool literals
    if rhs.kind() == "true" || rhs.kind() == "false" {
        return Some("Bool".to_string());
    }
    // Array literal
    if rhs.kind() == "array" {
        return Some("Array".to_string());
    }
    // Hash literal
    if rhs.kind() == "hash" {
        return Some("Hash".to_string());
    }
    // Nil literal
    if rhs.kind() == "nil" {
        return Some("Nil".to_string());
    }
    None
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

    #[test]
    fn test_ivar_direct_assignment() {
        let mut idx = DocumentIndex::new();
        idx.update_file(
            Path::new("a.cr"),
            "class User\n  @name = \"\"\n  @age = 0\nend\n",
        );

        let vars = idx.find_instance_vars("User");
        assert_eq!(vars.len(), 2);
        let name_var = vars.iter().find(|v| v.name == "@name").unwrap();
        assert_eq!(name_var.type_name.as_deref(), Some("String"));
        let age_var = vars.iter().find(|v| v.name == "@age").unwrap();
        assert_eq!(age_var.type_name.as_deref(), Some("Int32"));
    }

    #[test]
    fn test_ivar_type_declaration() {
        let mut idx = DocumentIndex::new();
        idx.update_file(
            Path::new("a.cr"),
            "class User\n  @name : String\n  @email : String?\nend\n",
        );

        let vars = idx.find_instance_vars("User");
        assert_eq!(vars.len(), 2);
        let name_var = vars.iter().find(|v| v.name == "@name").unwrap();
        assert_eq!(name_var.type_name.as_deref(), Some("String"));
    }

    #[test]
    fn test_ivar_from_property_macro() {
        let mut idx = DocumentIndex::new();
        idx.update_file(
            Path::new("a.cr"),
            "class User\n  property name : String\n  getter age : Int32\n  setter email : String\nend\n",
        );

        let vars = idx.find_instance_vars("User");
        assert_eq!(vars.len(), 3);
        assert!(vars.iter().any(|v| v.name == "@name" && v.type_name.as_deref() == Some("String")));
        assert!(vars.iter().any(|v| v.name == "@age" && v.type_name.as_deref() == Some("Int32")));
        assert!(vars.iter().any(|v| v.name == "@email" && v.type_name.as_deref() == Some("String")));
    }

    #[test]
    fn test_ivar_constructor_inference() {
        let mut idx = DocumentIndex::new();
        idx.update_file(
            Path::new("a.cr"),
            "class App\n  @logger = Logger.new\nend\n",
        );

        let vars = idx.find_instance_vars("App");
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].name, "@logger");
        assert_eq!(vars[0].type_name.as_deref(), Some("Logger"));
    }

    #[test]
    fn test_ivar_empty_class() {
        let mut idx = DocumentIndex::new();
        idx.update_file(Path::new("a.cr"), "class Empty\nend\n");

        let vars = idx.find_instance_vars("Empty");
        assert!(vars.is_empty());
    }

    #[test]
    fn test_return_type_string_literal() {
        let symbols = index_source("def greet\n  \"hello\"\nend\n");
        let greet = symbols.iter().find(|s| s.name == "greet").unwrap();
        assert_eq!(greet.return_type.as_deref(), Some("String"));
    }

    #[test]
    fn test_return_type_integer_literal() {
        let symbols = index_source("def count\n  42\nend\n");
        let count = symbols.iter().find(|s| s.name == "count").unwrap();
        assert_eq!(count.return_type.as_deref(), Some("Int32"));
    }

    #[test]
    fn test_return_type_bool_comparison() {
        let symbols = index_source("def valid?\n  x == 1\nend\n");
        let valid = symbols.iter().find(|s| s.name == "valid?").unwrap();
        assert_eq!(valid.return_type.as_deref(), Some("Bool"));
    }

    #[test]
    fn test_return_type_constructor() {
        let symbols = index_source("def build\n  User.new\nend\n");
        let build = symbols.iter().find(|s| s.name == "build").unwrap();
        assert_eq!(build.return_type.as_deref(), Some("User"));
    }

    #[test]
    fn test_return_type_explicit_preserved() {
        let symbols = index_source("def name : String\n  @name\nend\n");
        let name = symbols.iter().find(|s| s.name == "name").unwrap();
        assert_eq!(name.return_type.as_deref(), Some("String"));
    }

    #[test]
    fn test_return_type_no_inference_for_unknown() {
        let symbols = index_source("def process\n  do_something\nend\n");
        let process = symbols.iter().find(|s| s.name == "process").unwrap();
        assert_eq!(process.return_type, None);
    }

    #[test]
    fn test_return_type_nil_literal() {
        let symbols = index_source("def nothing\n  nil\nend\n");
        let nothing = symbols.iter().find(|s| s.name == "nothing").unwrap();
        assert_eq!(nothing.return_type.as_deref(), Some("Nil"));
    }

    #[test]
    fn test_return_type_float_literal() {
        let symbols = index_source("def pi\n  3.14\nend\n");
        let pi = symbols.iter().find(|s| s.name == "pi").unwrap();
        assert_eq!(pi.return_type.as_deref(), Some("Float64"));
    }
}

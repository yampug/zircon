use std::collections::HashMap;
use tree_sitter::{Node, Tree};
use tree_sitter_crystal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrystalType {
    Nil,
    Bool,
    Int32,
    Float64,
    Char,
    String,
    Symbol,
    Array(Box<CrystalType>),
    Hash(Box<CrystalType>, Box<CrystalType>),
    Tuple(Vec<CrystalType>),
    NamedTuple(HashMap<String, CrystalType>),
    Custom(String),
    Unknown,
}

impl CrystalType {
    pub fn to_string(&self) -> String {
        match self {
            CrystalType::Nil => "Nil".to_string(),
            CrystalType::Bool => "Bool".to_string(),
            CrystalType::Int32 => "Int32".to_string(),
            CrystalType::Float64 => "Float64".to_string(),
            CrystalType::Char => "Char".to_string(),
            CrystalType::String => "String".to_string(),
            CrystalType::Symbol => "Symbol".to_string(),
            CrystalType::Array(t) => format!("Array({})", t.to_string()),
            CrystalType::Hash(k, v) => format!("Hash({}, {})", k.to_string(), v.to_string()),
            CrystalType::Tuple(ts) => {
                let types: Vec<String> = ts.iter().map(|t| t.to_string()).collect();
                format!("{{{}}}", types.join(", "))
            }
            CrystalType::NamedTuple(m) => {
                let mut entries: Vec<String> = m.iter().map(|(k, v)| format!("{}: {}", k, v.to_string())).collect();
                entries.sort();
                format!("{{{}}}", entries.join(", "))
            }
            CrystalType::Custom(s) => s.clone(),
            CrystalType::Unknown => "Unknown".to_string(),
        }
    }
}

pub struct Scope {
    pub variables: HashMap<String, CrystalType>,
}

impl Scope {
    pub fn new() -> Self {
        Self {
            variables: HashMap::new(),
        }
    }
}

pub struct Analyzer {
    pub scopes: Vec<Scope>,
}

impl Analyzer {
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope::new()],
        }
    }

    pub fn analyze(&mut self, tree: &Tree, source: &[u8]) {
        self.walk(tree.root_node(), source);
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope::new());
    }

    fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    fn current_scope_mut(&mut self) -> &mut Scope {
        self.scopes.last_mut().expect("no scopes")
    }

    fn find_variable(&self, name: &str) -> Option<&CrystalType> {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.variables.get(name) {
                return Some(t);
            }
        }
        None
    }

    fn walk(&mut self, node: Node, source: &[u8]) {
        match node.kind() {
            "assign" => {
                self.handle_assign(node, source);
            }
            "method_def" | "class_def" | "module_def" | "struct_def" | "block" | "do" => {
                self.push_scope();
                self.walk_children(node, source);
                self.pop_scope();
            }
            _ => {
                self.walk_children(node, source);
            }
        }
    }

    fn walk_children(&mut self, node: Node, source: &[u8]) {
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                self.walk(cursor.node(), source);
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    fn handle_assign(&mut self, node: Node, source: &[u8]) {
        let mut lhs = None;
        let mut rhs = None;
        let mut encountered_eq = false;

        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                match child.kind() {
                    "=" => encountered_eq = true,
                    _ => {
                        if !encountered_eq {
                            if lhs.is_none() {
                                lhs = Some(child);
                            }
                        } else {
                            if rhs.is_none() {
                                rhs = Some(child);
                            }
                        }
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }

        if let (Some(l), Some(r)) = (lhs, rhs) {
            let var_name = l.utf8_text(source).unwrap_or("").to_string();
            let inferred_type = self.infer_type(r, source);
            if !var_name.is_empty() && inferred_type != CrystalType::Unknown {
                self.current_scope_mut().variables.insert(var_name, inferred_type);
            }
        }
    }

    fn infer_type(&self, node: Node, source: &[u8]) -> CrystalType {
        match node.kind() {
            "nil" => CrystalType::Nil,
            "true" | "false" => CrystalType::Bool,
            "integer" => CrystalType::Int32,
            "float" => CrystalType::Float64,
            "char" => CrystalType::Char,
            "string" | "chained_string" => CrystalType::String,
            "symbol" => CrystalType::Symbol,
            "array" => {
                let mut first_elem = None;
                let mut cursor = node.walk();
                if cursor.goto_first_child() {
                    loop {
                        let child = cursor.node();
                        if child.kind() != "[" && child.kind() != "]" && child.kind() != "," && child.kind() != "of" {
                            first_elem = Some(child);
                            break;
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                if let Some(elem) = first_elem {
                    CrystalType::Array(Box::new(self.infer_type(elem, source)))
                } else {
                    CrystalType::Array(Box::new(CrystalType::Unknown))
                }
            }
            "hash" => {
                let mut first_entry = None;
                let mut cursor = node.walk();
                if cursor.goto_first_child() {
                    loop {
                        let child = cursor.node();
                        if child.kind() == "hash_entry" {
                            first_entry = Some(child);
                            break;
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                if let Some(entry) = first_entry {
                    let mut k = CrystalType::Unknown;
                    let mut v = CrystalType::Unknown;
                    let mut entry_cursor = entry.walk();
                    if entry_cursor.goto_first_child() {
                        k = self.infer_type(entry_cursor.node(), source);
                        if entry_cursor.goto_next_sibling() {
                            if entry_cursor.goto_next_sibling() {
                                v = self.infer_type(entry_cursor.node(), source);
                            }
                        }
                    }
                    CrystalType::Hash(Box::new(k), Box::new(v))
                } else {
                    CrystalType::Hash(Box::new(CrystalType::Unknown), Box::new(CrystalType::Unknown))
                }
            }
            "tuple" => {
                let mut types = Vec::new();
                let mut cursor = node.walk();
                if cursor.goto_first_child() {
                    loop {
                        let child = cursor.node();
                        if child.kind() != "{" && child.kind() != "}" && child.kind() != "," {
                            types.push(self.infer_type(child, source));
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                CrystalType::Tuple(types)
            }
            "identifier" => {
                let var_name = node.utf8_text(source).unwrap_or("");
                self.find_variable(var_name).cloned().unwrap_or(CrystalType::Unknown)
            }
            "call" => {
                let mut method_name = "";
                let mut cursor = node.walk();
                if cursor.goto_first_child() {
                    loop {
                        let child = cursor.node();
                        if child.kind() == "identifier" {
                            method_name = child.utf8_text(source).unwrap_or("");
                            break;
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                match method_name {
                    "to_s" => CrystalType::String,
                    "to_i" | "to_i32" => CrystalType::Int32,
                    "to_f" | "to_f64" => CrystalType::Float64,
                    _ => CrystalType::Unknown,
                }
            }
            _ => CrystalType::Unknown,
        }
    }

    pub fn completions(&self) -> Vec<(String, String)> {
        let mut all_completions = HashMap::new();
        for scope in &self.scopes {
            for (name, crystal_type) in &scope.variables {
                all_completions.insert(name.clone(), crystal_type.to_string());
            }
        }
        let mut result: Vec<(String, String)> = all_completions.into_iter().collect();
        result.sort();
        result
    }
}

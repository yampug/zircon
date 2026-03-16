use serde_json::Value;
use zed_extension_api::lsp::{CompletionKind, SymbolKind};

#[derive(Debug)]
pub struct CrystalDiagnostic {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub message: String,
    pub severity: String,
}

/// Parses the JSON output from `crystal build --no-codegen -f json`.
pub fn parse_diagnostics(json_output: &str) -> Vec<CrystalDiagnostic> {
    let Ok(value) = serde_json::from_str::<Value>(json_output) else {
        return Vec::new();
    };

    let Some(arr) = value.as_array() else {
        return Vec::new();
    };

    arr.iter()
        .filter_map(|entry| {
            Some(CrystalDiagnostic {
                file: entry.get("file")?.as_str()?.to_string(),
                line: entry.get("line")?.as_u64()? as u32,
                column: entry.get("column")?.as_u64()? as u32,
                message: entry.get("message")?.as_str()?.to_string(),
                severity: entry
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .unwrap_or("error")
                    .to_string(),
            })
        })
        .collect()
}

/// Returns true if the type string is exactly `Nil`.
pub fn is_nil_type(type_str: &str) -> bool {
    type_str.trim() == "Nil"
}

/// Returns true if the type string is a union containing `Nil` (e.g. `String | Nil`).
pub fn is_nilable_union(type_str: &str) -> bool {
    type_str.contains("Nil") && type_str.contains('|')
}

/// Returns the tree-sitter highlight name appropriate for a Crystal type string.
/// Nil gets `constant.builtin`, nilable unions get `type.builtin`, others get `type`.
pub fn highlight_for_type(type_str: &str) -> &'static str {
    if is_nil_type(type_str) {
        "constant.builtin"
    } else if is_nilable_union(type_str) {
        "type.builtin"
    } else {
        "type"
    }
}

/// Maps an LSP CompletionKind to the appropriate tree-sitter highlight name.
pub fn highlight_for_completion_kind(kind: &CompletionKind) -> &'static str {
    match kind {
        CompletionKind::Method | CompletionKind::Function => "function.method",
        CompletionKind::Constructor => "function.method",
        CompletionKind::Variable => "variable",
        CompletionKind::Constant => "type",
        CompletionKind::Class | CompletionKind::Module | CompletionKind::Struct => "type",
        CompletionKind::Interface => "type",
        CompletionKind::Enum | CompletionKind::EnumMember => "type",
        CompletionKind::Keyword => "keyword",
        CompletionKind::Property | CompletionKind::Field => "property",
        CompletionKind::Snippet => "keyword",
        _ => "variable",
    }
}

/// Maps an LSP SymbolKind to the appropriate tree-sitter highlight name.
pub fn highlight_for_symbol_kind(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Method | SymbolKind::Function | SymbolKind::Constructor => "function.method",
        SymbolKind::Variable => "variable",
        SymbolKind::Constant => "type",
        SymbolKind::Class | SymbolKind::Module | SymbolKind::Struct => "type",
        SymbolKind::Interface | SymbolKind::Namespace => "type",
        SymbolKind::Enum | SymbolKind::EnumMember => "type",
        SymbolKind::Property | SymbolKind::Field => "property",
        _ => "variable",
    }
}

/// Returns true if a diagnostic message indicates a Nil-risk issue.
pub fn is_nil_risk(message: &str) -> bool {
    (message.contains("for Nil") || message.contains("not Nil") || message.contains("| Nil"))
        && message.contains("Nil")
}

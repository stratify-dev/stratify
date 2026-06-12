use serde::{Deserialize, Serialize};
use crate::confidence::Confidence;

/// Stable identifier for a symbol within one IrGraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SymbolId(pub u32);

/// A source location, half-open byte range plus 1-based line for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub file: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    File,
    Module,
    Class,
    Function,
    Constant,
    Dependency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Private,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub kind: SymbolKind,
    pub name: String,
    /// Fully-qualified path, e.g. "com.acme.Foo#bar".
    pub fqn: String,
    pub span: Span,
    pub visibility: Visibility,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Defines,
    Calls,
    Imports,
    Inherits,
    References,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub from: SymbolId,
    pub to: SymbolId,
    pub kind: RefKind,
    pub span: Span,
    pub confidence: Confidence,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::Confidence;

    #[test]
    fn symbol_round_trips_through_serde() {
        let sym = Symbol {
            id: SymbolId(1),
            kind: SymbolKind::Class,
            name: "Foo".into(),
            fqn: "com.acme.Foo".into(),
            span: Span { file: "Foo.java".into(), start_byte: 0, end_byte: 10, start_line: 1 },
            visibility: Visibility::Public,
            confidence: Confidence::Certain,
        };
        let json = serde_json::to_string(&sym).unwrap();
        let back: Symbol = serde_json::from_str(&json).unwrap();
        assert_eq!(sym, back);
    }
}

use crate::ir::{Reference, Symbol, SymbolId, Token};

/// The whole repository as one language-agnostic graph.
#[derive(Debug, Default, Clone)]
pub struct IrGraph {
    symbols: Vec<Symbol>,
    references: Vec<Reference>,
    entrypoints: Vec<SymbolId>,
    tokens: Vec<Token>,
    complexity: Vec<(SymbolId, u32)>,
}

impl IrGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a symbol and return its assigned id. The caller-provided id on the
    /// symbol is overwritten with the next sequential id to keep ids dense.
    pub fn add_symbol(&mut self, mut symbol: Symbol) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        symbol.id = id;
        self.symbols.push(symbol);
        id
    }

    pub fn add_reference(&mut self, reference: Reference) {
        self.references.push(reference);
    }

    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }

    pub fn references(&self) -> &[Reference] {
        &self.references
    }

    pub fn symbol(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.get(id.0 as usize)
    }

    /// Mark a symbol as an analysis entrypoint (a reachability root).
    /// Adapters decide what is an entrypoint (e.g. Java `main`, Ruby file scope).
    pub fn mark_entrypoint(&mut self, id: SymbolId) {
        self.entrypoints.push(id);
    }

    pub fn entrypoints(&self) -> &[SymbolId] {
        &self.entrypoints
    }

    pub fn add_token(&mut self, token: Token) {
        self.tokens.push(token);
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Record a function's cyclomatic complexity. Set by adapters.
    pub fn set_complexity(&mut self, id: SymbolId, value: u32) {
        self.complexity.push((id, value));
    }

    pub fn complexity_of(&self, id: SymbolId) -> Option<u32> {
        self.complexity
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, v)| *v)
    }

    pub fn complexities(&self) -> &[(SymbolId, u32)] {
        &self.complexity
    }

    /// Merge another graph into this one, remapping the other graph's ids so
    /// they stay unique. Returns nothing; used to combine per-file graphs.
    pub fn merge(&mut self, other: IrGraph) {
        let offset = self.symbols.len() as u32;
        for mut sym in other.symbols {
            sym.id = SymbolId(sym.id.0 + offset);
            self.symbols.push(sym);
        }
        for mut r in other.references {
            r.from = SymbolId(r.from.0 + offset);
            r.to = SymbolId(r.to.0 + offset);
            self.references.push(r);
        }
        for e in other.entrypoints {
            self.entrypoints.push(SymbolId(e.0 + offset));
        }
        self.tokens.extend(other.tokens);
        for (id, v) in other.complexity {
            self.complexity.push((SymbolId(id.0 + offset), v));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::Confidence;
    use crate::ir::{RefKind, Span, SymbolId, SymbolKind, Visibility};

    fn tok(file: &str, norm: &str) -> crate::ir::Token {
        crate::ir::Token {
            file: file.into(),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            norm: norm.into(),
        }
    }

    #[test]
    fn add_and_read_tokens() {
        let mut g = IrGraph::new();
        g.add_token(tok("a.rb", "ID"));
        assert_eq!(g.tokens().len(), 1);
        assert_eq!(g.tokens()[0].norm, "ID");
    }

    #[test]
    fn merge_concatenates_tokens() {
        let mut g1 = IrGraph::new();
        g1.add_token(tok("a.rb", "if"));
        let mut g2 = IrGraph::new();
        g2.add_token(tok("b.rb", "ID"));
        g1.merge(g2);
        assert_eq!(g1.tokens().len(), 2);
    }

    fn sym(name: &str) -> Symbol {
        Symbol {
            id: SymbolId(0),
            kind: SymbolKind::Function,
            name: name.into(),
            fqn: name.into(),
            span: Span {
                file: "x".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            visibility: Visibility::Public,
            confidence: Confidence::Certain,
        }
    }

    #[test]
    fn add_symbol_assigns_sequential_ids() {
        let mut g = IrGraph::new();
        let a = g.add_symbol(sym("a"));
        let b = g.add_symbol(sym("b"));
        assert_eq!(a, SymbolId(0));
        assert_eq!(b, SymbolId(1));
        assert_eq!(g.symbol(b).unwrap().name, "b");
    }

    #[test]
    fn mark_and_read_entrypoints() {
        let mut g = IrGraph::new();
        let a = g.add_symbol(sym("a"));
        g.mark_entrypoint(a);
        assert_eq!(g.entrypoints(), &[a]);
    }

    #[test]
    fn merge_remaps_entrypoints() {
        let mut g1 = IrGraph::new();
        g1.add_symbol(sym("a"));

        let mut g2 = IrGraph::new();
        let x = g2.add_symbol(sym("x"));
        g2.mark_entrypoint(x);

        g1.merge(g2);
        // x was id 0 in g2, becomes id 1 after merge (offset 1).
        assert_eq!(g1.entrypoints(), &[SymbolId(1)]);
    }

    #[test]
    fn set_and_read_complexity() {
        let mut g = IrGraph::new();
        let a = g.add_symbol(sym("a"));
        g.set_complexity(a, 7);
        assert_eq!(g.complexity_of(a), Some(7));
        assert_eq!(g.complexity_of(SymbolId(999)), None);
    }

    #[test]
    fn merge_remaps_complexity() {
        let mut g1 = IrGraph::new();
        g1.add_symbol(sym("a"));
        let mut g2 = IrGraph::new();
        let x = g2.add_symbol(sym("x"));
        g2.set_complexity(x, 5);
        g1.merge(g2);
        // x was id 0 in g2, becomes id 1 after merge (offset 1).
        assert_eq!(g1.complexity_of(SymbolId(1)), Some(5));
    }

    #[test]
    fn merge_remaps_reference_ids() {
        let mut g1 = IrGraph::new();
        g1.add_symbol(sym("a"));

        let mut g2 = IrGraph::new();
        let x = g2.add_symbol(sym("x"));
        let y = g2.add_symbol(sym("y"));
        g2.add_reference(Reference {
            from: x,
            to: y,
            kind: RefKind::Calls,
            span: Span {
                file: "x".into(),
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
            },
            confidence: Confidence::Certain,
        });

        g1.merge(g2);
        assert_eq!(g1.symbols().len(), 3);
        // x was id 0 in g2, becomes id 1 after merge (offset 1).
        let r = &g1.references()[0];
        assert_eq!(r.from, SymbolId(1));
        assert_eq!(r.to, SymbolId(2));
    }
}

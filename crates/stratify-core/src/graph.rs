use crate::ir::{Reference, Symbol, SymbolId};

/// The whole repository as one language-agnostic graph.
#[derive(Debug, Default, Clone)]
pub struct IrGraph {
    symbols: Vec<Symbol>,
    references: Vec<Reference>,
    entrypoints: Vec<SymbolId>,
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::Confidence;
    use crate::ir::{RefKind, Span, SymbolId, SymbolKind, Visibility};

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

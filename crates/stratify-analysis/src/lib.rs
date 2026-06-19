pub mod boundaries;
pub mod complexity;
pub mod cycles;
pub mod deadcode;
pub mod duplication;
pub mod hotspot;
pub mod ignore;
pub mod imports;
pub mod resolve;

#[cfg(test)]
pub(crate) mod test_support {
    use stratify_core::ir::{Reference, Span, Symbol, SymbolId, Visibility};
    use stratify_core::{Confidence, IrGraph, RefKind, SymbolKind};

    /// A trivial span in `file`; byte offsets are irrelevant to graph-shape tests.
    pub(crate) fn span(file: &str) -> Span {
        Span {
            file: file.into(),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
        }
    }

    /// Add a symbol spanning `file`. `fqn` is the resolved name (often equal to
    /// `name`, but distinct for Go where it carries the package).
    pub(crate) fn add_sym(
        g: &mut IrGraph,
        kind: SymbolKind,
        name: &str,
        fqn: &str,
        file: &str,
    ) -> SymbolId {
        g.add_symbol(Symbol {
            id: SymbolId(0),
            kind,
            name: name.into(),
            fqn: fqn.into(),
            span: span(file),
            visibility: Visibility::Unknown,
            confidence: Confidence::Certain,
        })
    }

    /// Add a reference edge spanning `file`.
    pub(crate) fn add_ref(
        g: &mut IrGraph,
        from: SymbolId,
        to: SymbolId,
        kind: RefKind,
        file: &str,
    ) {
        g.add_reference(Reference {
            from,
            to,
            kind,
            span: span(file),
            confidence: Confidence::Certain,
        });
    }

    /// Add a `File` symbol at `path` (name, fqn, and span all `path`).
    pub(crate) fn file_sym(g: &mut IrGraph, path: &str) -> SymbolId {
        add_sym(g, SymbolKind::File, path, path, path)
    }

    /// Add a `Dependency` keyed by `key` and an `Imports` edge from `from`.
    pub(crate) fn add_import(g: &mut IrGraph, from: SymbolId, key: &str) {
        let d = add_sym(g, SymbolKind::Dependency, key, key, "x");
        add_ref(g, from, d, RefKind::Imports, "x");
    }
}

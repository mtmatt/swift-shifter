# Vendored mitex Typst scope

The Typst definitions that the `mitex` crate's TeX→Typst math output refers
to (`mitexsqrt`, `pmatrix`, …). They are copied from
<https://github.com/mitex-rs/mitex/tree/f69eb43b52372e141b35661c049ce522567adbf0/packages/mitex>,
the commit the `mitex` 0.2.4 crate was published from, so the scope matches
the converter's output. Licensed Apache-2.0 (see `LICENSE`), like Swift
Shifter.

Local changes:

- `specs/latex/standard.typ` imports `../../xarrow.typ` instead of
  `@preview/xarrow:0.2.0`, which is GPL-3.0 and would need a network fetch.
- `xarrow.typ` is our own implementation of the one function used.
- `\KaTeX` renders as `K A T E X`: the original used `kai`, a symbol typst
  0.15 removed.

When upgrading the `mitex` crate, re-copy `specs/` from the commit the new
version was published from (see its `.cargo_vcs_info.json`) and reapply the
import change.

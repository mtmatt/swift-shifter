# Benchmark documents

Test documents for the conversion benchmark (#150): LaTeX → Typst, and
PDF → Markdown / LaTeX / Typst. Each document is LaTeX source, compiled
with XeLaTeX into a PDF, so every PDF has its source as the answer key.
References come from the `.bbl` uploaded with the paper; where there's
none, BibTeX makes it from the paper's `.bib`.

`documents.toml` lists every document: where it came from, its licence,
when it was fetched, which file to compile, and what it covers (math,
macros, tables, two columns, multiple files, bibliographies, Chinese).

## Where they come from

- **arXiv papers** (`documents/arxiv-*`): only papers whose authors chose
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) or
  [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/), as
  shown on each paper's abstract page. The source is the version named
  in `source`. Files the build doesn't use (unreferenced figures, drafts,
  editor files, BibTeX styles where the authors uploaded a `.bbl`) were
  left out, as were bundled copies of classes TeX Live has (REVTeX).
  Anything else changed is listed under `changes`. Each paper's
  copyright stays with its authors, under its licence.
- **Class and style files bundled with a paper** keep their own
  licences, stated in each file (LPPL for those here). A paper isn't
  included if a bundled file doesn't state a licence, or if any of its
  files (often a publisher's template the paper started from) restricts
  distributing it.
- **Documents written for this benchmark** (`documents/en-*`,
  `documents/zh-*`): part of this repository, under its licence
  (GPL-3.0-or-later). The `zh-*` ones mix Chinese and English and use
  `ctexart`; `en-tables` covers the kinds of tables papers use.

Nothing goes in without a clear licence.

## Adding a document

1. Check the licence on the arXiv abstract page (`https://arxiv.org/abs/<id>`):
   CC BY or CC0 only.
2. Download the source from `https://arxiv.org/e-print/<id>`, and keep
   only the files the build needs. Check the licence of any class or
   style file it bundles.
3. Add its entry to `documents.toml`, with the version, the licence link
   and the date. `cargo test` checks that every directory has an entry,
   and every entry a directory and main file.

//! [`Document`] → a standalone LaTeX document. Latin-script documents
//! compile with pdfLaTeX, XeLaTeX or LuaLaTeX; anything beyond Latin needs
//! XeLaTeX or LuaLaTeX, and pdfLaTeX stops with a message saying so. Image
//! paths are used as given: the converter copies images next to the output
//! first (see `native::copy_images_for_latex`), so they're relative,
//! ASCII-named and in formats every engine reads.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use super::{BlockWriter, Footnotes, HeadingIds};
use crate::converter::document::native::ast::{
    Align, Block, Document, Inline, List, Meta, RawFormat, Table, contains_cjk, inline_text,
};
use crate::converter::document::native::{read, typst};

pub fn write(doc: &Document) -> String {
    let (notes, note_bodies) = Footnotes::collect(doc);
    let w = Writer {
        notes,
        note_bodies,
        notes_written: RefCell::new(HashSet::new()),
        ids: RefCell::new(HeadingIds::default()),
        in_note: Cell::new(false),
        itemize_depth: Cell::new(0),
        enumerate_depth: Cell::new(0),
        nesting: Cell::new(0),
    };
    let opens_with_title = matches!(
        doc.blocks.first(),
        Some(Block::Heading { content, .. })
            if Some(inline_text(content).as_str()) == doc.meta.title.as_deref()
    );
    let body = w.blocks(&doc.blocks);
    // Everything that gets typeset (code and alt text included), so a CJK
    // comment in a code block still gets a CJK font.
    let typeset: String = [body.as_str(), doc.meta.title.as_deref().unwrap_or("")]
        .into_iter()
        .chain(doc.meta.authors.iter().map(String::as_str))
        .collect();
    let script = Script {
        beyond_latin: typeset.chars().any(needs_unicode_engine),
        cjk_font: contains_cjk(&typeset).then(|| cjk_font(doc.effective_lang().as_deref())),
    };
    format!(
        "{}\\begin{{document}}\n{}\n{body}\n\n\\end{{document}}\n",
        preamble(&doc.meta, &script),
        if doc.meta.title.is_some() && !opens_with_title {
            "\\maketitle\n"
        } else {
            ""
        }
    )
}

/// What the text needs from the engine.
struct Script {
    /// Characters pdfLaTeX's `inputenc` can't typeset.
    beyond_latin: bool,
    /// The xeCJK font, when the text has CJK characters.
    cjk_font: Option<String>,
}

/// Characters past Latin (Greek, Cyrillic, arrows, emoji, CJK…) that
/// pdfLaTeX can't typeset without extra setup. The common punctuation of
/// General Punctuation and the euro sign are fine.
fn needs_unicode_engine(c: char) -> bool {
    c > '\u{024F}' && !('\u{2000}'..='\u{206F}').contains(&c) && c != '€'
}

/// The CJK font for XeLaTeX: the family the PDF path would use for this
/// language, else Noto (installed with most TeX distributions' CJK support
/// and on most Linux systems). It's chosen from the fonts on the computer
/// converting, so a `.tex` compiled elsewhere may need it changed.
fn cjk_font(lang: Option<&str>) -> String {
    // `text_fonts` lists the Latin font first, then installed CJK families.
    typst::text_fonts(lang)
        .into_iter()
        .nth(1)
        .unwrap_or_else(|| "Noto Serif CJK SC".to_string())
}

fn preamble(meta: &Meta, script: &Script) -> String {
    let mut p = String::from(PREAMBLE);
    if script.beyond_latin {
        p.push_str(
            "\\ifPDFTeX\n  \\errmessage{This document has non-Latin text: compile it with XeLaTeX or LuaLaTeX}\n\\fi\n",
        );
    }
    if let Some(font) = &script.cjk_font {
        p.push_str(&format!(
            "\\ifXeTeX\n  \\usepackage{{xeCJK}}\n  \\setCJKmainfont{{{}}}\n\\else\\ifLuaTeX\n  \\usepackage{{luatexja}}\n\\fi\\fi\n",
            escape(font)
        ));
    }
    if let Some(title) = &meta.title {
        p.push_str(&format!("\\title{{{}}}\n", escape(title)));
    }
    if !meta.authors.is_empty() {
        let authors: Vec<String> = meta.authors.iter().map(|a| escape(a)).collect();
        p.push_str(&format!("\\author{{{}}}\n", authors.join(" \\and ")));
    }
    // An empty date keeps \maketitle from printing today's.
    p.push_str(&format!(
        "\\date{{{}}}\n",
        meta.date.as_deref().map(escape).unwrap_or_default()
    ));
    p
}

/// Engine-neutral setup: fonts by engine, the packages the body uses,
/// images no wider than the text, and unnumbered sections like the source.
const PREAMBLE: &str = r"\documentclass{article}
\usepackage{iftex}
\ifPDFTeX
  \usepackage[T1]{fontenc}
  \usepackage[utf8]{inputenc}
  \usepackage{lmodern}
  \usepackage{textcomp}
\else
  \usepackage{fontspec}
\fi
\usepackage{amsmath,amssymb}
\usepackage{graphicx}
\makeatletter
\def\maxwidth{\ifdim\Gin@nat@width>\linewidth\linewidth\else\Gin@nat@width\fi}
\makeatother
\setkeys{Gin}{width=\maxwidth,keepaspectratio}
\usepackage{longtable,booktabs,array}
\usepackage[normalem]{ulem}
\usepackage{xurl}
\usepackage[hidelinks]{hyperref}
\providecommand{\tightlist}{\setlength{\itemsep}{0pt}\setlength{\parskip}{0pt}}
\setcounter{secnumdepth}{0}
\setlength{\parindent}{0pt}
\setlength{\parskip}{6pt plus 2pt minus 1pt}
";

/// LaTeX nests at most four lists of each kind, and six list-like
/// environments (quotes included) in all. One level is left for the
/// `center`/`flushleft` a block inside may open.
const MAX_LIST_DEPTH: usize = 4;
const MAX_NESTING: usize = 5;

/// Display-math environments that can't go inside `\[ … \]`.
const DISPLAY_ENVIRONMENTS: &[&str] = &[
    "align",
    "align*",
    "alignat",
    "alignat*",
    "flalign",
    "flalign*",
    "gather",
    "gather*",
    "multline",
    "multline*",
    "equation",
    "equation*",
    "eqnarray",
    "eqnarray*",
];

struct Writer {
    notes: Footnotes,
    note_bodies: Vec<Vec<Block>>,
    notes_written: RefCell<HashSet<usize>>,
    ids: RefCell<HeadingIds>,
    /// Inside a `\footnote{…}` argument, where verbatim, floats and
    /// longtables can't go.
    in_note: Cell<bool>,
    itemize_depth: Cell<usize>,
    enumerate_depth: Cell<usize>,
    /// Lists and quotes around the block being written.
    nesting: Cell<usize>,
}

impl BlockWriter for Writer {
    fn block(&self, block: &Block) -> Option<String> {
        Some(match block {
            Block::Heading { level, content } => self.heading(*level, content),
            Block::Paragraph(content) => match content.as_slice() {
                [Inline::Image { url, alt, .. }] => self.figure(url, alt),
                _ => self.inlines(content),
            },
            Block::Quote(children) => {
                self.nesting.set(self.nesting.get() + 1);
                let inner = self.blocks(children);
                self.nesting.set(self.nesting.get() - 1);
                format!("\\begin{{quote}}\n{inner}\n\\end{{quote}}")
            }
            Block::List(list) => self.list(list),
            Block::Code { code, .. } => {
                if self.in_note.get() || code.contains("\\end{verbatim}") {
                    typewriter_lines(code)
                } else {
                    format!("\\begin{{verbatim}}\n{code}\n\\end{{verbatim}}")
                }
            }
            Block::Math(tex) => display_math(tex),
            Block::Table(table) => self.table(table),
            Block::ThematicBreak => {
                "\\begin{center}\\rule{0.5\\linewidth}{0.5pt}\\end{center}".to_string()
            }
            Block::FootnoteDefinition { .. } => return None,
            // Raw HTML contributes its text (see the plain writer).
            Block::Raw {
                format: RawFormat::Html,
                text,
            } => {
                let blocks: Vec<Block> = read::html::read_fragment(text)
                    .into_iter()
                    .map(|block| match block {
                        Block::Raw { text, .. } => {
                            Block::Paragraph(vec![Inline::Text(read::html::text_of(&text))])
                        }
                        other => other,
                    })
                    .collect();
                self.blocks(&blocks)
            }
        })
    }
}

impl Writer {
    /// `\section[plain]{\texorpdfstring{rich}{plain}}`: the table of
    /// contents, running heads and PDF bookmarks get plain text, so notes
    /// and formatting in a heading can't break them.
    fn heading(&self, level: u8, content: &[Inline]) -> String {
        let command = match level {
            1 => "section",
            2 => "subsection",
            3 => "subsubsection",
            4 => "paragraph",
            _ => "subparagraph",
        };
        let id = self.ids.borrow_mut().next(content);
        let plain = escape(inline_text(content).trim());
        format!(
            "\\{command}[{plain}]{{\\texorpdfstring{{{}}}{{{plain}}}}}\\label{{{}}}",
            self.inlines(content),
            label(&id)
        )
    }

    fn list(&self, list: &List) -> String {
        let (env, depth) = match list.start {
            Some(_) => ("enumerate", &self.enumerate_depth),
            None => ("itemize", &self.itemize_depth),
        };
        if depth.get() >= MAX_LIST_DEPTH || self.nesting.get() >= MAX_NESTING {
            return self.flat_list(list);
        }
        depth.set(depth.get() + 1);
        self.nesting.set(self.nesting.get() + 1);
        let mut out = format!("\\begin{{{env}}}\n");
        if let Some(start) = list.start.filter(|s| *s != 1) {
            // enumi … enumiv: the counter of this nesting level.
            let counter = ["enumi", "enumii", "enumiii", "enumiv"][depth.get() - 1];
            out.push_str(&format!(
                "\\setcounter{{{counter}}}{{{}}}\n",
                start.saturating_sub(1)
            ));
        }
        if list.tight {
            out.push_str("\\tightlist\n");
        }
        for item in &list.items {
            let marker = match item.task {
                Some(true) => "\\item[$\\boxtimes$] ",
                Some(false) => "\\item[$\\square$] ",
                None => "\\item ",
            };
            let sep = if list.tight { "\n" } else { "\n\n" };
            out.push_str(marker);
            // `{}` keeps a leading `[` or `*` from being read as an option.
            out.push_str(&guard(&self.blocks_joined(&item.content, sep)));
            out.push('\n');
        }
        out.push_str(&format!("\\end{{{env}}}"));
        depth.set(depth.get() - 1);
        self.nesting.set(self.nesting.get() - 1);
        out
    }

    /// A list nested deeper than LaTeX allows: one paragraph per item,
    /// with its marker as text.
    fn flat_list(&self, list: &List) -> String {
        let items: Vec<String> = list
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let marker = match (item.task, list.start) {
                    (Some(true), _) => "$\\boxtimes$".to_string(),
                    (Some(false), _) => "$\\square$".to_string(),
                    (None, Some(start)) => format!("{}.", start + i as u64),
                    (None, None) => "\\textbullet".to_string(),
                };
                format!("{marker}~{}", self.blocks_joined(&item.content, "\n\n"))
            })
            .collect();
        items.join("\n\n")
    }

    fn table(&self, table: &Table) -> String {
        let columns = table
            .header
            .len()
            .max(table.rows.iter().map(Vec::len).max().unwrap_or(0))
            .max(1);
        let align: Vec<char> = (0..columns)
            .map(
                |i| match table.align.get(i).copied().unwrap_or(Align::Default) {
                    Align::Default | Align::Left => 'l',
                    Align::Center => 'c',
                    Align::Right => 'r',
                },
            )
            .collect();
        let row = |cells: &[Vec<Inline>]| {
            let cells: Vec<String> = align
                .iter()
                .enumerate()
                .map(|(i, column)| {
                    cells
                        .get(i)
                        .map(|c| self.cell(c, *column))
                        .unwrap_or_default()
                })
                .collect();
            format!("{} \\\\\n", guard(&cells.join(" & ")))
        };
        let spec: String = align.iter().collect();
        // A longtable breaks across pages, but can't go in a footnote.
        let (env, head_end) = if self.in_note.get() {
            ("tabular", "")
        } else {
            ("longtable", "\\endhead\n")
        };
        let mut out = format!("\\begin{{{env}}}{{@{{}}{spec}@{{}}}}\n\\toprule\n");
        if !table.header.is_empty() {
            out.push_str(&row(&table.header));
            out.push_str(&format!("\\midrule\n{head_end}"));
        }
        for cells in &table.rows {
            out.push_str(&row(cells));
        }
        out.push_str(&format!("\\bottomrule\n\\end{{{env}}}"));
        out
    }

    /// A table cell. `\newline` doesn't work in `l`/`c`/`r` columns, so a
    /// cell with line breaks becomes a one-column tabular of its lines,
    /// unless it has a footnote, which a tabular would swallow: then its
    /// lines are joined with spaces.
    fn cell(&self, content: &[Inline], column: char) -> String {
        if !content.contains(&Inline::LineBreak) {
            return self.inlines(content);
        }
        let lines: Vec<String> = content
            .split(|i| *i == Inline::LineBreak)
            .map(|line| guard(self.inlines(line).trim()))
            .collect();
        if content.iter().any(|i| matches!(i, Inline::FootnoteRef(_))) {
            return lines.join(" ");
        }
        format!(
            "\\begin{{tabular}}[t]{{@{{}}{column}@{{}}}}{}\\end{{tabular}}",
            lines.join(" \\\\ ")
        )
    }

    /// An image on its own: centred, with its alt text as the caption (or
    /// just the image inside a footnote, where floats can't go).
    fn figure(&self, url: &str, alt: &str) -> String {
        if !is_local(url) {
            return alt_text(alt);
        }
        let image = format!("\\includegraphics{{{}}}", url_arg(url));
        if self.in_note.get() {
            image
        } else if alt.is_empty() {
            format!("\\begin{{center}}\n{image}\n\\end{{center}}")
        } else {
            format!(
                "\\begin{{figure}}[htbp]\n\\centering\n{image}\n\\caption{{{}}}\n\\end{{figure}}",
                escape(alt)
            )
        }
    }

    fn inlines(&self, inlines: &[Inline]) -> String {
        let mut out = String::new();
        for inline in inlines {
            self.inline(&mut out, inline);
        }
        out
    }

    fn inline(&self, out: &mut String, inline: &Inline) {
        let wrap = |out: &mut String, command: &str, content: &[Inline]| {
            out.push_str(&format!("\\{command}{{{}}}", self.inlines(content)));
        };
        match inline {
            Inline::Text(t) => out.push_str(&escape(t)),
            Inline::Emph(c) => wrap(out, "emph", c),
            Inline::Strong(c) => wrap(out, "textbf", c),
            Inline::Strikethrough(c) => wrap(out, "sout", c),
            Inline::Superscript(c) => wrap(out, "textsuperscript", c),
            Inline::Subscript(c) => wrap(out, "textsubscript", c),
            Inline::Code(code) => out.push_str(&format!("\\texttt{{{}}}", escape_code(code))),
            Inline::Math(tex) => out.push_str(&format!("\\({}\\)", tex.trim())),
            Inline::Link { url, content, .. } => {
                if let Some(id) = url.strip_prefix('#') {
                    out.push_str(&format!(
                        "\\hyperref[{}]{{{}}}",
                        label(id),
                        self.inlines(content)
                    ));
                } else if content.as_slice() == [Inline::Text(url.clone())] {
                    out.push_str(&format!("\\url{{{}}}", url_arg(url)));
                } else {
                    out.push_str(&format!(
                        "\\href{{{}}}{{{}}}",
                        url_arg(url),
                        self.inlines(content)
                    ));
                }
            }
            Inline::Image { url, alt, .. } => {
                if is_local(url) {
                    out.push_str(&format!(
                        "\\includegraphics[height=1.2em]{{{}}}",
                        url_arg(url)
                    ));
                } else {
                    out.push_str(&alt_text(alt));
                }
            }
            Inline::FootnoteRef(name) => match self.notes.number(name) {
                Some(n) => out.push_str(&self.footnote(n)),
                None => out.push_str(&escape(&format!("[^{name}]"))),
            },
            Inline::LineBreak => out.push_str("\\newline\n"),
            Inline::SoftBreak => out.push('\n'),
            Inline::Raw { text, .. } => out.push_str(&escape(&read::html::text_of(text))),
        }
    }

    /// The first reference to a note carries its body; later ones show the
    /// same number through a label.
    fn footnote(&self, n: usize) -> String {
        if !self.notes_written.borrow_mut().insert(n) {
            return format!("\\textsuperscript{{\\ref{{fn:{n}}}}}");
        }
        let was_in_note = self.in_note.replace(true);
        let body = self
            .note_bodies
            .get(n - 1)
            .map(|b| self.blocks(b))
            .unwrap_or_default();
        self.in_note.set(was_in_note);
        format!("\\footnote{{\\label{{fn:{n}}}{body}}}")
    }
}

/// Display math in `\[ … \]`, unless the formula is itself a display
/// environment (`align`, `equation`, …), which can't nest there.
fn display_math(tex: &str) -> String {
    let tex = tex.trim();
    let is_environment = tex
        .strip_prefix("\\begin{")
        .and_then(|rest| rest.split_once('}'))
        .is_some_and(|(name, _)| DISPLAY_ENVIRONMENTS.contains(&name));
    if is_environment {
        tex.to_string()
    } else {
        format!("\\[\n{tex}\n\\]")
    }
}

/// Code as escaped typewriter lines, for where verbatim can't go (inside a
/// footnote, or code containing `\end{verbatim}`). Indentation is kept
/// with hard spaces, and an empty line stays a line.
fn typewriter_lines(code: &str) -> String {
    let lines: Vec<String> = code
        .lines()
        .map(|line| {
            let indent = line.len() - line.trim_start_matches(' ').len();
            let rest = &line[indent..];
            if rest.is_empty() {
                "\\mbox{}".to_string()
            } else {
                format!("{}\\texttt{{{}}}", "\\ ".repeat(indent), escape_code(rest))
            }
        })
        .collect();
    format!(
        "\\begin{{flushleft}}\n{}\n\\end{{flushleft}}",
        lines.join("\\\\\n")
    )
}

/// `[image]`, or the alt text in brackets: for images no engine can show.
fn alt_text(alt: &str) -> String {
    escape(&format!("[{}]", if alt.is_empty() { "image" } else { alt }))
}

/// `{}` in front of a line starting with `*`, which `\\` would otherwise
/// read as its starred form (`[` is always escaped as `{[}`).
fn guard(line: &str) -> String {
    if line.starts_with('*') {
        format!("{{}}{line}")
    } else {
        line.to_string()
    }
}

/// Whether an image URL names a file (the converter made local images
/// relative); remote and `data:` images can't be included.
fn is_local(url: &str) -> bool {
    !url.is_empty() && !url.contains("://") && !url.starts_with("data:")
}

/// A heading id as a `\label` key: no characters TeX treats specially
/// (`\ { } # % ~ ^ & $ _`), no spaces, and no `,`, which separates keys in
/// `\ref` lists.
fn label(id: &str) -> String {
    id.chars()
        .map(|c| match c {
            '\\' | '{' | '}' | '#' | '%' | '~' | '^' | '&' | '$' | '_' | ' ' | ',' => '-',
            c => c,
        })
        .collect()
}

/// A URL or path for `\href`, `\url` and `\includegraphics`: `%` and `#`
/// escaped, the characters that would end the argument percent-encoded.
fn url_arg(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.chars() {
        match c {
            '%' => out.push_str("\\%"),
            '#' => out.push_str("\\#"),
            '\\' => out.push_str("%5C"),
            '{' => out.push_str("%7B"),
            '}' => out.push_str("%7D"),
            c => out.push(c),
        }
    }
    out
}

/// LaTeX text: the ten special characters escaped, and `[`/`]` braced so
/// they can't be read as an optional argument (after `\item` or `\\`).
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '$' => out.push_str("\\$"),
            '&' => out.push_str("\\&"),
            '%' => out.push_str("\\%"),
            '#' => out.push_str("\\#"),
            '_' => out.push_str("\\_"),
            '~' => out.push_str("\\textasciitilde{}"),
            '^' => out.push_str("\\textasciicircum{}"),
            '[' => out.push_str("{[}"),
            ']' => out.push_str("{]}"),
            c => out.push(c),
        }
    }
    out
}

/// [`escape`] for `\texttt`, where hyphen runs and quotes must stay as
/// typed rather than becoming dashes and curly quotes.
fn escape_code(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    for c in escape(code).chars() {
        match c {
            '-' => out.push_str("-{}"),
            '`' => out.push_str("\\textasciigrave{}"),
            '\'' => out.push_str("\\textquotesingle{}"),
            '<' => out.push_str("\\textless{}"),
            '>' => out.push_str("\\textgreater{}"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::read::markdown;

    fn body(md: &str) -> String {
        let tex = write(&markdown::read(md));
        let start = tex.find("\\begin{document}\n").unwrap() + "\\begin{document}\n".len();
        let end = tex.find("\\end{document}").unwrap();
        tex[start..end].trim().to_string()
    }

    #[test]
    fn escapes_special_characters() {
        assert_eq!(
            body("a \\\\ {b} $c & 50% #d e\\_f ~g ^h [i]\n"),
            "a \\textbackslash{} \\{b\\} \\$c \\& 50\\% \\#d e\\_f \\textasciitilde{}g \\textasciicircum{}h {[}i{]}"
        );
        assert_eq!(
            body("`a--b 'q' <x>`\n"),
            "\\texttt{a-{}-{}b \\textquotesingle{}q\\textquotesingle{} \\textless{}x\\textgreater{}}"
        );
    }

    #[test]
    fn brackets_and_stars_after_items_and_rows_stay_text() {
        let tex =
            body("- [link] text\n- *em* first\n\n| [h] | b |\n|---|---|\n| \\*x | y<br>[w] |\n");
        assert!(tex.contains("\\item {[}link{]} text"), "{tex}");
        assert!(tex.contains("\\item \\emph{em} first"), "{tex}");
        assert!(tex.contains("\\toprule\n{[}h{]} & b \\\\"), "{tex}");
        assert!(tex.contains("\\endhead\n{}*x & "), "{tex}");
        assert!(tex.contains("y \\\\ {[}w{]}"), "{tex}");
    }

    #[test]
    fn headings_get_plain_short_forms_labels_and_links() {
        let tex = body(
            "# Intro *Part*[^n]\n\n## Next\n\nSee [intro](#intro-part) and [site](https://e.org/a_b?x=1%20#top).\n\n[^n]: Note.\n",
        );
        assert!(
            tex.starts_with(
                "\\section[Intro Part]{\\texorpdfstring{Intro \\emph{Part}\\footnote{\\label{fn:1}Note.}}{Intro Part}}\\label{intro-part}"
            ),
            "{tex}"
        );
        assert!(
            tex.contains("\\subsection[Next]{\\texorpdfstring{Next}{Next}}\\label{next}"),
            "{tex}"
        );
        assert!(tex.contains("\\hyperref[intro-part]{intro}"), "{tex}");
        assert!(
            tex.contains("\\href{https://e.org/a_b?x=1\\%20\\#top}{site}"),
            "{tex}"
        );
        assert!(body("<https://e.org>\n").contains("\\url{https://e.org}"));
    }

    #[test]
    fn writes_lists_tasks_quotes_and_rules() {
        assert_eq!(
            body("3. a\n4. b\n\n- [x] done\n- [ ] todo\n\n> q\n\n---\n"),
            "\\begin{enumerate}\n\\setcounter{enumi}{2}\n\\tightlist\n\\item a\n\\item b\n\\end{enumerate}\n\n\
             \\begin{itemize}\n\\tightlist\n\\item[$\\boxtimes$] done\n\\item[$\\square$] todo\n\\end{itemize}\n\n\
             \\begin{quote}\nq\n\\end{quote}\n\n\\begin{center}\\rule{0.5\\linewidth}{0.5pt}\\end{center}"
        );
    }

    #[test]
    fn nested_lists_use_their_own_counter_and_flatten_past_four() {
        let tex = body("1. outer\n\n   3. inner\n\n2. next\n");
        assert!(
            tex.contains("\\setcounter{enumii}{2}") && !tex.contains("{enumi}"),
            "{tex}"
        );
        let deep = body("- 1\n  - 2\n    - 3\n      - 4\n        - 5\n");
        assert_eq!(deep.matches("\\begin{itemize}").count(), 4, "{deep}");
        // Alternating kinds stay within LaTeX's overall limit.
        let mixed = body("- a\n  1. b\n     - c\n       1. d\n          - e\n            1. f\n");
        let opened =
            mixed.matches("\\begin{itemize}").count() + mixed.matches("\\begin{enumerate}").count();
        assert_eq!(opened, MAX_NESTING, "{mixed}");
        assert!(deep.contains("\\textbullet~5"), "{deep}");
    }

    #[test]
    fn writes_math_code_and_tables() {
        let tex = body(
            "Inline $x^2$.\n\n$$\\frac{a}{b}$$\n\n$$\\begin{align}a&=b\\end{align}$$\n\n```rust\nfn x() {}\n```\n\n| a | b |\n|:-:|--:|\n| 1 | 2 |\n",
        );
        assert!(tex.contains("Inline \\(x^2\\)."), "{tex}");
        assert!(tex.contains("\\[\n\\frac{a}{b}\n\\]"), "{tex}");
        assert!(
            tex.contains("\n\\begin{align}a&=b\\end{align}\n"),
            "no \\[ around align: {tex}"
        );
        assert!(
            tex.contains("\\begin{verbatim}\nfn x() {}\n\\end{verbatim}"),
            "{tex}"
        );
        assert!(
            tex.contains("\\begin{longtable}{@{}cr@{}}\n\\toprule\na & b \\\\\n\\midrule\n\\endhead\n1 & 2 \\\\\n\\bottomrule"),
            "{tex}"
        );
        let multiline = body("| a | b |\n|---|--:|\n| x<br>y | 2<br>3 |\n");
        assert!(
            multiline.contains("\\begin{tabular}[t]{@{}l@{}}x \\\\ y\\end{tabular} & \\begin{tabular}[t]{@{}r@{}}2 \\\\ 3\\end{tabular} \\\\"),
            "{multiline}"
        );
        let nasty = body("```\n\\end{verbatim}\n\n  indented\n```\n");
        assert!(
            nasty.contains("\\texttt{\\textbackslash{}end\\{verbatim\\}}\\\\\n\\mbox{}\\\\\n\\ \\ \\texttt{indented}"),
            "{nasty}"
        );
    }

    #[test]
    fn footnotes_inline_and_refer_back() {
        let tex = body("One[^a] two[^a].\n\n[^a]: Note *here*.\n");
        assert_eq!(
            tex,
            "One\\footnote{\\label{fn:1}Note \\emph{here}.} two\\textsuperscript{\\ref{fn:1}}."
        );
    }

    #[test]
    fn note_bodies_avoid_what_cant_go_in_an_argument() {
        let tex = body(
            "Ref[^n].\n\n[^n]: Text\n\n    ```\n    code\n    ```\n\n    ![pic](i/1.png)\n\n    | a |\n    |---|\n    | 1 |\n",
        );
        let note = &tex[tex.find("\\footnote").unwrap()..];
        assert!(
            !note.contains("verbatim") && note.contains("\\texttt{code}"),
            "{note}"
        );
        assert!(
            !note.contains("figure") && note.contains("\\includegraphics{i/1.png}"),
            "{note}"
        );
        assert!(
            !note.contains("longtable") && note.contains("\\begin{tabular}"),
            "{note}"
        );
    }

    #[test]
    fn images_need_a_local_path() {
        let tex = body(
            "![A cat](images/1.png)\n\n![](https://e.org/x.png) ![data](data:image/png;base64,AA)\n",
        );
        assert!(
            tex.contains("\\includegraphics{images/1.png}\n\\caption{A cat}"),
            "{tex}"
        );
        assert!(tex.contains("{[}image{]} {[}data{]}"), "{tex}");
    }

    #[test]
    fn preamble_has_metadata_and_script_support() {
        let tex = write(&markdown::read(
            "---\ntitle: T & U\nauthor: [Ann, Bo]\n---\n\n中文內容\n",
        ));
        assert!(
            tex.contains("\\title{T \\& U}\n\\author{Ann \\and Bo}"),
            "{tex}"
        );
        assert!(tex.contains("\\maketitle"), "{tex}");
        assert!(
            tex.contains("\\usepackage{xeCJK}\n  \\setCJKmainfont{"),
            "{tex}"
        );
        assert!(tex.contains("compile it with XeLaTeX or LuaLaTeX"), "{tex}");
        let greek = write(&markdown::read("α → β\n"));
        assert!(
            greek.contains("\\errmessage") && !greek.contains("xeCJK"),
            "{greek}"
        );
        let titled = write(&markdown::read(
            "---\ntitle: Same\n---\n\n# Same\n\nText — “quoted” café €5\n",
        ));
        assert!(
            !titled.contains("\\maketitle"),
            "the document opens with its title"
        );
        assert!(
            !titled.contains("\\errmessage"),
            "Latin text works with pdfLaTeX"
        );
        // Code is typeset too: its characters count.
        let code_only = write(&markdown::read("```\n├── 入口\n```\n"));
        assert!(
            code_only.contains("xeCJK") && code_only.contains("\\errmessage"),
            "{code_only}"
        );
    }
}

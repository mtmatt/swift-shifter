//! [`Document`] → a standalone LaTeX document. Latin-script documents
//! compile with pdfLaTeX, XeLaTeX or LuaLaTeX; anything beyond Latin needs
//! XeLaTeX or LuaLaTeX, and pdfLaTeX stops with a message saying so. Image
//! paths are used as given: the converter copies images next to the output
//! first (see `native::copy_images_for_latex`), so they're relative,
//! ASCII-named and in formats every engine reads.

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};

use super::latex_math::{self, Fallbacks};
use super::{BlockWriter, DocumentIds, Footnotes, lone_anchor};
use crate::converter::document::native::ast::{
    Align, Block, Document, Inline, List, Meta, RawFormat, Table, contains_cjk, inline_text,
};
use crate::converter::document::native::{read, typst};

pub fn write(doc: &Document) -> String {
    let opens_with_title = matches!(
        doc.blocks.first(),
        Some(Block::Heading { content, .. })
            if Some(inline_text(content).as_str()) == doc.meta.title.as_deref()
    );
    // The title, authors and date are typeset only by `\maketitle`.
    let make_title = doc.meta.title.is_some() && !opens_with_title;
    let mut w = Writer::new(doc, Punctuation::AsTyped);
    let mut body = w.blocks(&doc.blocks);
    let mut titles = w.titles(&doc.meta);
    let script = Script::of(doc, &[&body, if make_title { &titles } else { "" }]);
    if script.cjk_font.is_some() {
        // Whether xeCJK is loaded depends on all of the text, so for it
        // the text is written again.
        w = Writer::new(doc, Punctuation::ForXeCjk);
        body = w.blocks(&doc.blocks);
        titles = w.titles(&doc.meta);
    }
    format!(
        "{}{titles}\\begin{{document}}\n{}\n{body}\n\n\\end{{document}}\n",
        preamble(&script, &w.fallbacks.borrow()),
        if make_title { "\\maketitle\n" } else { "" }
    )
}

/// What the text needs from the engine.
struct Script {
    /// Characters pdfLaTeX's `inputenc` can't typeset.
    beyond_latin: bool,
    /// The xeCJK font, when the text has CJK characters.
    cjk_font: Option<&'static str>,
}

impl Script {
    /// What `doc` needs, written as the `typeset` LaTeX. Everything that
    /// gets typeset counts (code and alt text included), so a CJK comment
    /// in a code block still gets a CJK font.
    fn of(doc: &Document, typeset: &[&str]) -> Self {
        Script {
            beyond_latin: typeset
                .iter()
                .any(|text| text.chars().any(needs_unicode_engine)),
            cjk_font: typeset
                .iter()
                .any(|text| contains_cjk(text))
                .then(|| typst::cjk_family(doc.effective_lang().as_deref())),
        }
    }
}

/// Characters past Latin (Greek, Cyrillic, arrows, emoji, CJK…) that
/// pdfLaTeX can't typeset without extra setup. The common punctuation of
/// General Punctuation and the euro sign are fine.
fn needs_unicode_engine(c: char) -> bool {
    c > '\u{024F}' && !('\u{2000}'..='\u{206F}').contains(&c) && c != '€'
}

/// The preamble up to the title, authors and date (see
/// [`Writer::titles`]).
fn preamble(script: &Script, fallbacks: &Fallbacks) -> String {
    let mut p = String::from(PREAMBLE);
    p.push_str(&fallbacks.preamble());
    if script.beyond_latin {
        p.push_str(
            "\\ifPDFTeX\n  \\errmessage{This document has non-Latin text: compile it with XeLaTeX or LuaLaTeX}\n\\fi\n",
        );
    }
    if let Some(font) = script.cjk_font {
        p.push_str(&format!(
            "\\ifXeTeX\n  \\usepackage{{xeCJK}}\n  \\setCJKmainfont{{{}}}\n\\else\\ifLuaTeX\n  \\usepackage{{luatexja}}\n\\fi\\fi\n",
            escape(font)
        ));
    }
    p
}

/// Engine-neutral setup: fonts by engine, the packages the body uses,
/// images no wider than the text, unnumbered sections like the source, and
/// lists and quotes nested as deep as the source's. For that, `\@toodeep`,
/// which LaTeX calls past six list-like environments to stop with "Too
/// deeply nested", counts the level instead (as enumitem's `\setlistdepth`
/// does) and indents it itself: by 1em on the left only, as typst indents
/// a quote, and only while the line is at least 10em wide. With the
/// sixth level's margins, which a quote takes on both sides, thirteen
/// quotes would leave no line at all. itemize and enumerate call it too,
/// past four of their kind, which the writer never nests. The math's
/// commands that these packages lack are defined after it (see
/// [`Fallbacks`]).
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
\makeatletter
\def\@toodeep{\global\advance\@listdepth\@ne \leftmargin\z@
  \ifdim\linewidth<10em \else
    \advance\@totalleftmargin1em \advance\linewidth-1em
  \fi}
\makeatother
\setcounter{secnumdepth}{0}
\setlength{\parindent}{0pt}
\setlength{\parskip}{6pt plus 2pt minus 1pt}
";

/// LaTeX nests at most four lists of each kind. (Its limit of six
/// list-like environments in all, quotes included, is lifted in the
/// preamble.)
const MAX_LIST_DEPTH: usize = 4;

/// How many notes deep the writer writes a note's text at its first
/// reference, each note in the text of the one before (see
/// [`Writer::footnote`]): deeper than notes nest, but for chains of notes
/// that each refer to the next, and shallow enough for the 2 MiB stack
/// conversions run on. That stack holds a few hundred levels of the
/// writer's quotes and lists, so this leaves each note's text dozens.
const MAX_NOTE_NESTING: usize = 16;

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
    /// The notes' numbers by label (see [`Footnotes`]), which the writer
    /// knows them by, and their labels in the LaTeX (`fn:3`) too. LaTeX
    /// shows numbers of its own (see [`NoteState`]).
    notes: Footnotes,
    /// The notes' bodies, note `n`'s at `n - 1`.
    note_bodies: Vec<Vec<Block>>,
    note_state: RefCell<NoteState>,
    ids: RefCell<DocumentIds>,
    /// Writing blocks read out of raw HTML, whose headings are in no part.
    in_raw: Cell<bool>,
    itemize_depth: Cell<usize>,
    enumerate_depth: Cell<usize>,
    /// The commands the math uses that the preamble may have to define.
    fallbacks: RefCell<Fallbacks>,
    punctuation: Punctuation,
}

/// How the writer writes [`SHARED_PUNCTUATION`].
#[derive(Clone, Copy, PartialEq)]
enum Punctuation {
    /// As typed, for a document without xeCJK, where every engine sets it
    /// as Latin.
    AsTyped,
    /// For xeCJK, which would set all of it as CJK: as Latin where it's
    /// among Latin text (see [`latin_punctuation`]).
    ForXeCjk,
}

/// Which plain text of a heading [`Writer::plain`] writes.
#[derive(Clone, Copy)]
enum PlainText {
    /// The short form, which the table of contents and running heads
    /// typeset, and which hyperref makes the section's bookmark from.
    ShortForm,
    /// The text of the PDF bookmark.
    Bookmark,
}

/// What the writer has done with the notes. LaTeX numbers them in the
/// order their first references are written (see [`Writer::footnote`]).
/// Where a note's text can't go (see [`Writer::marking_notes_only`]), the
/// note is only marked, under its number given explicitly, and its text is
/// pending until the writer is out of every such place.
#[derive(Default)]
struct NoteState {
    /// The notes, by their numbers in [`Footnotes`], whose first reference
    /// has been written.
    referenced: HashSet<usize>,
    /// The notes whose texts are being written, each inside the one before.
    writing: Vec<usize>,
    /// How many places that [`Writer::mark_notes_only_in`] writes the text
    /// being written is in.
    places: usize,
    /// The texts of the notes only marked, each with its LaTeX number,
    /// until they're set (see [`Writer::flush_pending_notes`]). There are
    /// none where a note's text can go, but between a longtable's head and
    /// its first row (see [`Writer::table`]).
    pending_texts: Vec<(usize, String)>,
    /// The notes first referred to [`MAX_NOTE_NESTING`] notes deep, whose
    /// texts are written when the pending ones are set. There are none
    /// where a note's text can go, but between a longtable's head and its
    /// first row, as for the pending texts.
    deferred: VecDeque<DeferredNote>,
}

/// A note first referred to [`MAX_NOTE_NESTING`] notes deep, marked but its
/// text not written yet (see [`NoteState::deferred`]).
struct DeferredNote {
    /// The number LaTeX's mark for it shows.
    mark: usize,
    /// Its number in [`Footnotes`].
    note: usize,
}

impl NoteState {
    /// The LaTeX number of note `n`'s mark at its first reference, `None`
    /// where it's been referred to before.
    fn first_reference(&mut self, n: usize) -> Option<usize> {
        self.referenced.insert(n).then(|| self.last_number())
    }

    /// The LaTeX number of the last note marked.
    fn last_number(&self) -> usize {
        self.referenced.len()
    }
}

/// One flag for each [`SHARED_PUNCTUATION`] character that a run of text
/// shows, in the order [`for_each_shown`] shows them, saying whether xeCJK
/// is to set it as Latin (see [`latin_punctuation`]). Every writer of the
/// run takes the flags in that order, each piece of the run those of its
/// own characters (see [`with_latin`]). Without xeCJK there are none, and
/// the punctuation is written as typed.
type LatinFlags = [bool];

impl BlockWriter for Writer {
    // Notes nest as deep as they refer to one another, up to
    // [`MAX_NOTE_NESTING`], each in a paragraph of the one before:
    // paragraphs and note references are written by functions of their
    // own, so a note takes the stack those take, not what every kind of
    // block and inline takes.
    fn block(&self, block: &Block) -> Option<String> {
        match block {
            Block::Paragraph(content) => Some(self.paragraph(content)),
            other => self.other_block(other),
        }
    }
}

impl Writer {
    /// A paragraph, or a figure for an image on its own.
    fn paragraph(&self, content: &[Inline]) -> String {
        match content {
            [Inline::Image { url, alt, .. }] => self.figure(url, alt),
            _ => self.inlines(content),
        }
    }

    /// Any other block (see [`Writer::block`]).
    fn other_block(&self, block: &Block) -> Option<String> {
        Some(match block {
            Block::Heading { level, content } => self.heading(*level, content),
            Block::Paragraph(content) => self.paragraph(content),
            Block::Quote(children) => format!(
                "\\begin{{quote}}\n{}{}\n\\end{{quote}}",
                heading_opener(children),
                self.blocks(children)
            ),
            Block::List(list) => self.list(list),
            Block::Code { code, .. } => {
                if self.in_note() || code.contains("\\end{verbatim}") {
                    self.typewriter_lines(code)
                } else {
                    format!("\\begin{{verbatim}}\n{code}\n\\end{{verbatim}}")
                }
            }
            // An empty `\[ \]` would hold a blank line, which ends the
            // paragraph TeX's display math is in.
            Block::Math(tex) if tex.trim().is_empty() => return None,
            Block::Math(tex) => display_math(&self.math(tex)),
            Block::Table(table) => self.table(table),
            Block::ThematicBreak => {
                "\\begin{center}\\rule{0.5\\linewidth}{0.5pt}\\end{center}".to_string()
            }
            Block::FootnoteDefinition { .. } => return None,
            // Raw HTML contributes its text (see the plain writer).
            // An empty anchor, as the Typst reader puts before a numbered
            // equation, is where links to its id go.
            Block::Raw {
                format: RawFormat::Html,
                text,
            } if let Some(id) = lone_anchor(text) => {
                format!("\\phantomsection\\label{{{}}}", label(&id))
            }
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
                let was_in_raw = self.in_raw.replace(true);
                let text = self.blocks(&blocks);
                self.in_raw.set(was_in_raw);
                text
            }
        })
    }

    fn new(doc: &Document, punctuation: Punctuation) -> Self {
        let (notes, note_bodies) = Footnotes::collect(doc);
        Writer {
            ids: RefCell::new(DocumentIds::new(&doc.blocks, &note_bodies)),
            notes,
            note_bodies,
            note_state: RefCell::default(),
            in_raw: Cell::new(false),
            itemize_depth: Cell::new(0),
            enumerate_depth: Cell::new(0),
            fallbacks: RefCell::new(Fallbacks::default()),
            punctuation,
        }
    }

    /// Whether a note's text is being written: inside a `\footnote{…}`
    /// argument, verbatim, floats and longtables can't go.
    fn in_note(&self) -> bool {
        !self.note_state.borrow().writing.is_empty()
    }

    /// The note whose text is being written, 0 outside notes (see
    /// [`DocumentIds::next`]).
    fn current_note(&self) -> usize {
        self.note_state
            .borrow()
            .writing
            .last()
            .copied()
            .unwrap_or(0)
    }

    /// Math TeX with its Unicode characters under their LaTeX names.
    fn math(&self, tex: &str) -> String {
        latex_math::math(tex.trim(), &mut self.fallbacks.borrow_mut())
    }

    /// The [`LatinFlags`] of the run of `inlines`.
    fn latin(&self, inlines: &[Inline]) -> Vec<bool> {
        if self.punctuation == Punctuation::AsTyped {
            return Vec::new();
        }
        let mut chars = Vec::new();
        for inline in inlines {
            for_each_shown(inline, &mut |c| chars.push(c));
        }
        latin_punctuation(&chars)
    }

    /// The [`LatinFlags`] of `text`, a run on its own.
    fn latin_alone(&self, text: &str) -> Vec<bool> {
        if self.punctuation == Punctuation::AsTyped {
            return Vec::new();
        }
        latin_punctuation(&text.chars().collect::<Vec<_>>())
    }

    /// `\title`, `\author` and `\date`, for `\maketitle`. An empty date
    /// keeps it from printing today's.
    fn titles(&self, meta: &Meta) -> String {
        let field = |text: &str| self.field(text, &self.latin_alone(text));
        let mut p = String::new();
        if let Some(title) = &meta.title {
            p.push_str(&format!("\\title{{{}}}\n", field(title)));
        }
        if !meta.authors.is_empty() {
            let authors: Vec<String> = meta.authors.iter().map(|a| field(a)).collect();
            p.push_str(&format!("\\author{{{}}}\n", authors.join(" \\and ")));
        }
        p.push_str(&format!(
            "\\date{{{}}}\n",
            meta.date.as_deref().map(field).unwrap_or_default()
        ));
        p
    }

    /// The text of a field that holds only text (a title, an author, an
    /// image's alt text or caption), where readers flatten math to its TeX
    /// (a typst title's `$f := g$` is `f ≔ g`): as [`latin_text`] writes
    /// it, but with the math symbols that LaTeX's text fonts lack set as
    /// math, under their names (see [`is_math_symbol`]). Those would stop
    /// pdfLaTeX, and XeLaTeX has no glyphs for them. `latin` holds the
    /// text's flags.
    fn field(&self, text: &str, latin: &LatinFlags) -> String {
        let mut out = String::with_capacity(text.len());
        let mut latin = latin.iter().copied();
        for c in text.chars() {
            let name = is_math_symbol(c)
                .then(|| latex_math::symbol(c, &mut self.fallbacks.borrow_mut()))
                .flatten();
            match name {
                Some(name) => out.push_str(&format!("\\ensuremath{{{name}}}")),
                None => push_text_char(&mut out, c, &mut latin),
            }
        }
        out
    }

    /// `\section[plain]{\texorpdfstring{rich}{bookmark}}`: the table of
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
        let id = if self.in_raw.get() {
            self.ids.borrow_mut().extra(content)
        } else {
            self.ids.borrow_mut().next(self.current_note(), content)
        };
        let latin = self.latin(content);
        format!(
            "\\{command}[{}]{{\\texorpdfstring{{{}}}{{{}}}}}\\label{{{}}}",
            self.plain(content, PlainText::ShortForm, &latin).trim(),
            self.inlines_with(content, &latin),
            self.plain(content, PlainText::Bookmark, &latin).trim(),
            label(&id)
        )
    }

    /// `content`, a heading's text, without its formatting and notes, as
    /// `form` takes it. Math is under its LaTeX names, so a Latin heading
    /// stays ASCII: TeX in a bookmark, and in the short form math, in
    /// `\texorpdfstring` with that TeX for the bookmark, protected where it
    /// defines commands (see [`latex_math::moving`]). `latin` holds
    /// `content`'s flags.
    fn plain(&self, content: &[Inline], form: PlainText, latin: &LatinFlags) -> String {
        let mut out = String::new();
        for (inline, latin) in with_latin(content, latin) {
            match inline {
                Inline::Text(text) | Inline::Code(text) | Inline::Image { alt: text, .. } => {
                    out.push_str(&latin_text(text, latin));
                }
                Inline::Math(tex) => {
                    let math = self.math(tex);
                    let bookmark = escape(&math);
                    out.push_str(&match form {
                        PlainText::ShortForm => format!(
                            "\\texorpdfstring{{{}}}{{{bookmark}}}",
                            inline_math(&latex_math::moving(&math))
                        ),
                        PlainText::Bookmark => bookmark,
                    });
                }
                Inline::Emph(c)
                | Inline::Strong(c)
                | Inline::Strikethrough(c)
                | Inline::Superscript(c)
                | Inline::Subscript(c)
                | Inline::Link { content: c, .. } => {
                    out.push_str(&self.plain(c, form, latin));
                }
                Inline::LineBreak | Inline::SoftBreak => out.push(' '),
                Inline::FootnoteRef(_) | Inline::Raw { .. } => {}
            }
        }
        out
    }

    fn list(&self, list: &List) -> String {
        let (env, depth) = match list.start {
            Some(_) => ("enumerate", &self.enumerate_depth),
            None => ("itemize", &self.itemize_depth),
        };
        if depth.get() >= MAX_LIST_DEPTH {
            return self.flat_list(list);
        }
        depth.set(depth.get() + 1);
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
            out.push_str(heading_opener(&item.content));
            // `{}` keeps a leading `[` or `*` from being read as an option.
            out.push_str(&guard(&self.blocks_joined(&item.content, sep)));
            out.push('\n');
        }
        out.push_str(&format!("\\end{{{env}}}"));
        depth.set(depth.get() - 1);
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
                // `\par` starts each item afresh, even where the list
                // follows its parent item's text with a single newline.
                format!(
                    "\\par {marker}~{}",
                    self.blocks_joined(&item.content, "\n\n")
                )
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
            guard(&cells.join(" & "))
        };
        let spec: String = align.iter().collect();
        // A longtable breaks across pages, but can't go in a footnote.
        let (env, head_end) = if self.in_note() {
            ("tabular", "")
        } else {
            ("longtable", "\\endhead\n")
        };
        let mut out = format!("\\begin{{{env}}}{{@{{}}{spec}@{{}}}}\n\\toprule\n");
        if !table.header.is_empty() {
            // A longtable sets its head once, for every page, without the
            // texts of the notes in it: they're set at the start of its
            // first row, on the page the table starts on and before that
            // row's notes, or after a table without rows.
            let head = self.mark_notes_only_in(|| row(&table.header));
            out.push_str(&format!("{head} \\\\\n\\midrule\n{head_end}"));
        }
        for (i, cells) in table.rows.iter().enumerate() {
            let head_notes = if i == 0 {
                self.flush_pending_notes()
            } else {
                String::new()
            };
            out.push_str(&format!("{head_notes}{} \\\\\n", row(cells)));
        }
        out.push_str(&format!("\\bottomrule\n\\end{{{env}}}"));
        out.push_str(&self.flush_pending_notes());
        out
    }

    /// A table cell. `\newline` doesn't work in `l`/`c`/`r` columns, so a
    /// cell with line breaks becomes a one-column tabular of its lines,
    /// unless it has a footnote, even in its formatting, which a tabular
    /// would swallow: then its lines are joined with spaces.
    fn cell(&self, content: &[Inline], column: char) -> String {
        if !content.contains(&Inline::LineBreak) {
            return self.inlines(content);
        }
        let lines: Vec<String> = content
            .split(|i| *i == Inline::LineBreak)
            .map(|line| guard(self.inlines(line).trim()))
            .collect();
        if has_footnote(content) {
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
        let latin = self.latin_alone(alt);
        if !is_local(url) {
            return self.alt_text(alt, &latin);
        }
        let image = format!("\\includegraphics{{{}}}", url_arg(url));
        if self.in_note() {
            image
        } else if alt.is_empty() {
            format!("\\begin{{center}}\n{image}\n\\end{{center}}")
        } else {
            format!(
                "\\begin{{figure}}[htbp]\n\\centering\n{image}\n\\caption{{{}}}\n\\end{{figure}}",
                self.field(alt, &latin)
            )
        }
    }

    /// `[image]`, or the alt text in brackets: for images no engine can
    /// show. `latin` holds the alt text's flags.
    fn alt_text(&self, alt: &str, latin: &LatinFlags) -> String {
        let alt = if alt.is_empty() { "image" } else { alt };
        format!("{{[}}{}{{]}}", self.field(alt, latin))
    }

    /// A run of inlines: a paragraph, a heading or a table cell.
    fn inlines(&self, inlines: &[Inline]) -> String {
        self.inlines_with(inlines, &self.latin(inlines))
    }

    /// `inlines`, part of a run, of which `latin` holds their flags.
    fn inlines_with(&self, inlines: &[Inline], latin: &LatinFlags) -> String {
        let mut out = String::new();
        for (inline, latin) in with_latin(inlines, latin) {
            match inline {
                // See [`Writer::block`].
                Inline::FootnoteRef(name) => out.push_str(&self.footnote_ref(name)),
                other => self.inline(&mut out, other, latin),
            }
        }
        out
    }

    /// A note's mark, with its text where it's the first reference to it
    /// (see [`Writer::footnote`]).
    fn footnote_ref(&self, name: &str) -> String {
        match self.notes.number(name) {
            Some(n) => self.footnote(n),
            None => escape(&format!("[^{name}]")),
        }
    }

    /// Appends `inline`, of which `latin` holds the flags.
    fn inline(&self, out: &mut String, inline: &Inline, latin: &LatinFlags) {
        let content = |inlines: &[Inline]| self.inlines_with(inlines, latin);
        let wrapped = |command: &str, c: &[Inline]| format!("\\{command}{{{}}}", content(c));
        let wrap = |out: &mut String, command: &str, c: &[Inline]| {
            out.push_str(&wrapped(command, c));
        };
        // For a place that can't hold a note's text (see
        // [`Writer::mark_notes_only_in`]), followed by the texts of the
        // notes first referred to there.
        let wrap_marking_notes = |out: &mut String, command: &str, c: &[Inline]| {
            out.push_str(&self.mark_notes_only_in(|| wrapped(command, c)));
            out.push_str(&self.flush_pending_notes());
        };
        match inline {
            Inline::Text(t) => out.push_str(&latin_text(t, latin)),
            Inline::Emph(c) => wrap(out, "emph", c),
            Inline::Strong(c) => wrap(out, "textbf", c),
            Inline::Strikethrough(c) => wrap_marking_notes(out, "sout", c),
            Inline::Superscript(c) => wrap_marking_notes(out, "textsuperscript", c),
            Inline::Subscript(c) => wrap_marking_notes(out, "textsubscript", c),
            Inline::Code(code) => {
                out.push_str(&format!("\\texttt{{{}}}", escape_code(code, latin)));
            }
            Inline::Math(tex) => out.push_str(&inline_math(&self.math(tex))),
            Inline::Link {
                url, content: c, ..
            } => {
                if let Some(id) = url.strip_prefix('#') {
                    out.push_str(&format!("\\hyperref[{}]{{{}}}", label(id), content(c)));
                } else if c.as_slice() == [Inline::Text(url.clone())] {
                    out.push_str(&format!("\\url{{{}}}", url_arg(url)));
                } else {
                    out.push_str(&format!("\\href{{{}}}{{{}}}", url_arg(url), content(c)));
                }
            }
            Inline::Image { url, alt, .. } => {
                if is_local(url) {
                    out.push_str(&format!(
                        "\\includegraphics[height=1.2em]{{{}}}",
                        url_arg(url)
                    ));
                } else {
                    out.push_str(&self.alt_text(alt, latin));
                }
            }
            Inline::FootnoteRef(name) => out.push_str(&self.footnote_ref(name)),
            Inline::LineBreak => out.push_str("\\newline\n"),
            Inline::SoftBreak => out.push('\n'),
            // An anchor's opening tag, as text has one, is where links to
            // its id go.
            Inline::Raw { text, .. } => match lone_anchor(&format!("{text}</a>")) {
                Some(id) => out.push_str(&format!("\\phantomsection\\label{{{}}}", label(&id))),
                None => out.push_str(&latin_text(&read::html::text_of(text), latin)),
            },
        }
    }

    /// The mark of note `n`, whose first reference carries its text; later
    /// ones show its number through its label. LaTeX drops the text of a
    /// `\footnote` inside another's, so a note first referred to inside a
    /// note, or in another place that can't hold its text (see
    /// [`Writer::mark_notes_only_in`]), is only marked there, and its text
    /// is set after the outermost of them (`\footnotetext`, see
    /// [`Writer::flush_pending_notes`]). Its text is written at its mark, so
    /// the notes inside it are numbered right after it, as typst numbers
    /// them, unless it's first referred to [`MAX_NOTE_NESTING`] notes deep:
    /// then its text is written when the others are set, and the notes
    /// first referred to in it are numbered after all of those. The numbers
    /// of the notes only marked are explicit, which hyperref leaves
    /// unlinked (it links a mark to the next note text set, which here
    /// isn't its own), and `\phantomsection` gives the label in each text
    /// somewhere to link to. LaTeX's counter is then set past them (see
    /// [`Writer::note_counter`]).
    fn footnote(&self, n: usize) -> String {
        let Some(number) = self.note_state.borrow_mut().first_reference(n) else {
            return format!("\\textsuperscript{{\\ref{{fn:{n}}}}}");
        };
        if !self.marking_notes_only() {
            let mut out = format!("\\footnote{{{}}}", self.note_text(n));
            out.push_str(&self.flush_pending_notes());
            return out;
        }
        if self.note_state.borrow().writing.len() < MAX_NOTE_NESTING {
            // Written before the rest of the text around it, whose later
            // notes take the numbers after the ones inside this one.
            let text = self.note_text(n);
            self.note_state
                .borrow_mut()
                .pending_texts
                .push((number, text));
        } else {
            let deferred = DeferredNote {
                mark: number,
                note: n,
            };
            self.note_state.borrow_mut().deferred.push_back(deferred);
        }
        // In a box, which ulem's `\sout` takes as one piece.
        format!("\\mbox{{\\footnotemark[{number}]}}")
    }

    /// Whether a note's text can't go where the text being written is, so
    /// that a note first referred to there is only marked: in a note's
    /// text, or in a place [`Writer::mark_notes_only_in`] writes.
    fn marking_notes_only(&self) -> bool {
        let notes = self.note_state.borrow();
        !notes.writing.is_empty() || notes.places > 0
    }

    /// What `write` writes for a place other than a note's text that can't
    /// hold a note's text: an argument of ulem's `\sout`, which stops at a
    /// `\footnote`, of a superscript or subscript, which drops its text, or
    /// a longtable's head (see [`Writer::table`]). The notes first referred
    /// to there are only marked, and their texts left pending (see
    /// [`Writer::footnote`]).
    fn mark_notes_only_in(&self, write: impl FnOnce() -> String) -> String {
        self.note_state.borrow_mut().places += 1;
        let out = write();
        self.note_state.borrow_mut().places -= 1;
        out
    }

    /// The pending texts of the notes only marked, each under its number,
    /// and LaTeX's counter set past them: nothing where a note's text can't
    /// go, which leaves them pending, or where none are. The texts of the
    /// deferred notes (see [`Writer::footnote`]) are written first, outside
    /// every note's, so the notes inside them nest [`MAX_NOTE_NESTING`]
    /// deep again, deferring those deeper.
    fn flush_pending_notes(&self) -> String {
        if self.marking_notes_only() {
            return String::new();
        }
        loop {
            let deferred = self.note_state.borrow_mut().deferred.pop_front();
            let Some(DeferredNote {
                mark: number,
                note: n,
            }) = deferred
            else {
                break;
            };
            let text = self.note_text(n);
            self.note_state
                .borrow_mut()
                .pending_texts
                .push((number, text));
        }
        let mut pending = std::mem::take(&mut self.note_state.borrow_mut().pending_texts);
        if pending.is_empty() {
            return String::new();
        }
        pending.sort_by_key(|&(number, _)| number);
        let mut out = String::new();
        for (number, text) in &pending {
            out.push_str(&format!(
                "\\footnotetext[{number}]{{\\phantomsection{text}}}"
            ));
        }
        out.push_str(&self.note_counter());
        out
    }

    /// LaTeX's footnote counter set to the last number marked, for the
    /// next `\footnote` to take the number after it. It's set where that's
    /// done at once, outside every note's text, which a longtable sets only
    /// after its row, and with `\setcounter`, which pandoc, reading the
    /// LaTeX back, takes for part of the paragraph, where `\addtocounter`
    /// would end it.
    fn note_counter(&self) -> String {
        format!(
            "\\setcounter{{footnote}}{{{}}}",
            self.note_state.borrow().last_number()
        )
    }

    /// Note `n`'s label and body.
    fn note_text(&self, n: usize) -> String {
        self.note_state.borrow_mut().writing.push(n);
        let body = self
            .note_bodies
            .get(n - 1)
            .map(|b| self.blocks(b))
            .unwrap_or_default();
        self.note_state.borrow_mut().writing.pop();
        format!("\\label{{fn:{n}}}{body}")
    }

    /// Code as escaped typewriter lines, for where verbatim can't go (inside
    /// a footnote, or code containing `\end{verbatim}`). Indentation is kept
    /// with hard spaces, and an empty line stays a line.
    fn typewriter_lines(&self, code: &str) -> String {
        let lines: Vec<String> = code
            .lines()
            .map(|line| {
                let indent = line.len() - line.trim_start_matches(' ').len();
                let rest = &line[indent..];
                if rest.is_empty() {
                    "\\mbox{}".to_string()
                } else {
                    let code = escape_code(rest, &self.latin_alone(rest));
                    format!("{}\\texttt{{{code}}}", "\\ ".repeat(indent))
                }
            })
            .collect();
        format!(
            "\\begin{{flushleft}}\n{}\n\\end{{flushleft}}",
            lines.join("\\\\\n")
        )
    }
}

/// Whether `inlines` hold a footnote reference, in their formatting too.
fn has_footnote(inlines: &[Inline]) -> bool {
    inlines.iter().any(|inline| match inline {
        Inline::FootnoteRef(_) => true,
        Inline::Emph(c)
        | Inline::Strong(c)
        | Inline::Strikethrough(c)
        | Inline::Superscript(c)
        | Inline::Subscript(c)
        | Inline::Link { content: c, .. } => has_footnote(c),
        _ => false,
    })
}

/// `\mbox{}` before a heading that opens a quote or list item: a sectioning
/// command can't be the first thing in LaTeX's list machinery ("perhaps a
/// missing \item").
fn heading_opener(blocks: &[Block]) -> &'static str {
    if matches!(blocks.first(), Some(Block::Heading { .. })) {
        "\\mbox{}\n"
    } else {
        ""
    }
}

/// Inline math in `\( … \)`, the `\)` on a line of its own where the
/// formula ends in a comment (`a % note`), which would take it.
fn inline_math(math: &str) -> String {
    if latex_math::ends_in_comment(math) {
        format!("\\({math}\n\\)")
    } else {
        format!("\\({math}\\)")
    }
}

/// Display math in `\[ … \]`, unless the formula is itself a display
/// environment (`align`, `equation`, …), which can't nest there. One that
/// ends in a comment ends its line, which would take what follows the
/// block (the `}` of a note's text) for part of it.
fn display_math(tex: &str) -> String {
    let tex = tex.trim();
    let is_environment = tex
        .strip_prefix("\\begin{")
        .and_then(|rest| rest.split_once('}'))
        .is_some_and(|(name, _)| DISPLAY_ENVIRONMENTS.contains(&name));
    if !is_environment {
        format!("\\[\n{tex}\n\\]")
    } else if latex_math::ends_in_comment(tex) {
        format!("{tex}\n")
    } else {
        tex.to_string()
    }
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
pub(crate) fn is_local(url: &str) -> bool {
    !url.is_empty() && !url.contains("://") && !url.starts_with("data:")
}

/// A heading id as a `\label` key, a different one for each id. It's
/// ASCII, as every engine takes it, without the characters TeX treats
/// specially (`\ { } # % ~ ^ & $ _`), spaces or `,`, which separates keys
/// in `\ref` lists: ASCII letters, digits and `-` stay, and every other
/// character is its code point in hex between `:`s (`α` is `:3b1:`).
fn label(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for c in id.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            out.push(c);
        } else {
            out.push_str(&format!(":{:x}:", u32::from(c)));
        }
    }
    out
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
        push_escaped(&mut out, c);
    }
    out
}

/// Appends `c` to `out` as [`escape`] writes it.
fn push_escaped(out: &mut String, c: char) {
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

/// Punctuation that Latin text shares with CJK text, which xeCJK sets as
/// CJK punctuation: from the CJK font, and with the spaces next to it
/// dropped, as CJK text has none. Their text commands it sets as Latin.
/// (xeCJK takes `‥`, `‧` and `⸺` for CJK too, but LaTeX's text fonts have
/// no glyphs for them.)
const SHARED_PUNCTUATION: &[(char, &str)] = &[
    ('‘', "\\textquoteleft{}"),
    ('’', "\\textquoteright{}"),
    ('“', "\\textquotedblleft{}"),
    ('”', "\\textquotedblright{}"),
    ('–', "\\textendash{}"),
    ('—', "\\textemdash{}"),
    ('…', "\\textellipsis{}"),
    ('·', "\\textperiodcentered{}"),
];

/// The text command xeCJK sets [`SHARED_PUNCTUATION`] `c` as Latin with.
fn latin_form(c: char) -> Option<&'static str> {
    // All of it is `·` or in `–` … `…`, which most text never is.
    if c != '·' && !('–'..='…').contains(&c) {
        return None;
    }
    SHARED_PUNCTUATION
        .iter()
        .find_map(|&(shared, latin)| (shared == c).then_some(latin))
}

/// `text`, [`escape`]d, with its [`SHARED_PUNCTUATION`] written as its
/// flags, `latin`, say.
fn latin_text(text: &str, latin: &LatinFlags) -> String {
    let mut out = String::with_capacity(text.len());
    let mut latin = latin.iter().copied();
    for c in text.chars() {
        push_text_char(&mut out, c, &mut latin);
    }
    out
}

/// Appends `c` to `out` as [`latin_text`] writes it, taking its flag from
/// `latin`, the flags of its text from `c` on, where it's
/// [`SHARED_PUNCTUATION`].
fn push_text_char(out: &mut String, c: char, latin: &mut impl Iterator<Item = bool>) {
    match latin_form(c) {
        Some(form) if latin.next() == Some(true) => out.push_str(form),
        Some(_) => out.push(c),
        None => push_escaped(out, c),
    }
}

/// How xeCJK is to set the [`SHARED_PUNCTUATION`] in a run of text with
/// `chars` (see [`for_each_shown`]): for each such character in turn,
/// whether as Latin. A space next to it must stay, as typst keeps it, so
/// punctuation with a space beside it is Latin. Otherwise it's CJK beside
/// CJK text, and so is a doubled `……` or `——`, which is how CJK text writes
/// them. A pair of quotes is decided as one by what's outside it, and by
/// what's inside where that tells nothing (`“你好”` on its own is CJK), so
/// both quotes come from one font. The text around punctuation is the
/// nearest that isn't such punctuation itself.
fn latin_punctuation(chars: &[char]) -> Vec<bool> {
    let shared: Vec<bool> = chars.iter().map(|&c| latin_form(c).is_some()).collect();
    // What's before and after each character, past shared punctuation.
    let mut before = vec![Side::Edge; chars.len()];
    let mut side = Side::Edge;
    for (i, &c) in chars.iter().enumerate() {
        before[i] = side;
        if !shared[i] {
            side = Side::of(c);
        }
    }
    let mut after = vec![Side::Edge; chars.len()];
    side = Side::Edge;
    for (i, &c) in chars.iter().enumerate().rev() {
        after[i] = side;
        if !shared[i] {
            side = Side::of(c);
        }
    }
    let mut latin: Vec<Option<bool>> = vec![None; chars.len()];
    let (mut doubles, mut singles) = (Vec::new(), Vec::new());
    for (i, &c) in chars.iter().enumerate() {
        let open = match c {
            '“' => {
                doubles.push(i);
                continue;
            }
            '‘' => {
                singles.push(i);
                continue;
            }
            '”' => doubles.pop(),
            // Between letters it's an apostrophe (`it’s`).
            '’' if !is_apostrophe(chars, i) => singles.pop(),
            _ => None,
        };
        if let Some(open) = open {
            let pair = Side::decide([before[open], after[i]], false)
                .or_else(|| Side::decide([after[open], before[i]], false))
                .unwrap_or(true);
            (latin[open], latin[i]) = (Some(pair), Some(pair));
        }
    }
    // The rest, run by run.
    let mut i = 0;
    while i < chars.len() {
        if !shared[i] || latin[i].is_some() {
            i += 1;
            continue;
        }
        let start = i;
        while shared.get(i) == Some(&true) && latin[i].is_none() {
            i += 1;
        }
        let run = &chars[start..i];
        let doubled = run
            .windows(2)
            .any(|w| w[0] == w[1] && matches!(w[0], '…' | '—'));
        let set = Side::decide([before[start], after[i - 1]], doubled).unwrap_or(true);
        latin[start..i].fill(Some(set));
    }
    shared
        .into_iter()
        .zip(latin)
        .filter(|&(shared, _)| shared)
        .map(|(_, latin)| latin.unwrap_or(true))
        .collect()
}

/// What's beside a piece of [`SHARED_PUNCTUATION`], for
/// [`latin_punctuation`].
#[derive(Clone, Copy, PartialEq)]
enum Side {
    /// The start or end of the run.
    Edge,
    Space,
    Cjk,
    /// Latin text, or anything else.
    Other,
}

impl Side {
    fn of(c: char) -> Side {
        if is_cjk(c) {
            Side::Cjk
        } else if c.is_whitespace() {
            Side::Space
        } else {
            Side::Other
        }
    }

    /// Whether punctuation with these `sides` is Latin, if they tell: a
    /// space says Latin, and CJK text, or punctuation that's `cjk` itself,
    /// says CJK.
    fn decide(sides: [Side; 2], cjk: bool) -> Option<bool> {
        if sides.contains(&Side::Space) {
            Some(true)
        } else if cjk || sides.contains(&Side::Cjk) {
            Some(false)
        } else {
            None
        }
    }
}

/// Whether the `’` at `chars[i]` is an apostrophe, between two letters or
/// digits.
fn is_apostrophe(chars: &[char], i: usize) -> bool {
    let alphanumeric = |j: Option<usize>| {
        j.and_then(|j| chars.get(j))
            .is_some_and(|c| c.is_alphanumeric())
    };
    alphanumeric(i.checked_sub(1)) && alphanumeric(Some(i + 1))
}

/// Whether `c` is CJK text, as xeCJK classes it: a CJK letter, or CJK or
/// fullwidth punctuation, symbols and forms.
fn is_cjk(c: char) -> bool {
    contains_cjk(c.encode_utf8(&mut [0; 4]))
        || matches!(c,
            '\u{3000}'..='\u{303F}'     // CJK symbols and punctuation
            | '\u{3200}'..='\u{33FF}'   // enclosed CJK letters, compatibility
            | '\u{A960}'..='\u{A97F}'   // Hangul jamo extended A
            | '\u{D7B0}'..='\u{D7FF}'   // Hangul jamo extended B
            | '\u{FE10}'..='\u{FE1F}'   // vertical forms
            | '\u{FE30}'..='\u{FE4F}'   // CJK compatibility forms
            | '\u{FF00}'..='\u{FFEF}'   // halfwidth and fullwidth forms
            | '\u{1B000}'..='\u{1B16F}' // kana supplement and extended
            | '\u{1F200}'..='\u{1F2FF}' // enclosed ideographic supplement
            | '\u{2F800}'..='\u{2FA1F}' // CJK compatibility supplement
        )
}

/// Calls `f` with each character `inline` shows, as [`latin_punctuation`]
/// sees them: its text (an image's alt text, raw HTML's text), a space for
/// a break, and U+FFFC, neither CJK nor a space, for math.
fn for_each_shown(inline: &Inline, f: &mut dyn FnMut(char)) {
    match inline {
        Inline::Text(text) | Inline::Code(text) | Inline::Image { alt: text, .. } => {
            text.chars().for_each(f);
        }
        Inline::Raw { text, .. } => read::html::text_of(text).chars().for_each(f),
        Inline::Math(_) => f('\u{FFFC}'),
        Inline::Emph(c)
        | Inline::Strong(c)
        | Inline::Strikethrough(c)
        | Inline::Superscript(c)
        | Inline::Subscript(c)
        | Inline::Link { content: c, .. } => {
            for inline in c {
                for_each_shown(inline, f);
            }
        }
        Inline::LineBreak | Inline::SoftBreak => f(' '),
        Inline::FootnoteRef(_) => {}
    }
}

/// Each of `inlines` with its own flags, of `latin`, theirs: one for each
/// [`SHARED_PUNCTUATION`] character it shows.
fn with_latin<'a>(
    inlines: &'a [Inline],
    latin: &'a LatinFlags,
) -> impl Iterator<Item = (&'a Inline, &'a LatinFlags)> {
    let mut rest = latin;
    inlines.iter().map(move |inline| {
        let mut count = 0;
        if !rest.is_empty() {
            for_each_shown(inline, &mut |c| {
                count += usize::from(latin_form(c).is_some())
            });
        }
        let (own, after) = rest.split_at(count.min(rest.len()));
        rest = after;
        (inline, own)
    })
}

/// Whether `c` is a math symbol that LaTeX's text fonts lack: past what
/// pdfLaTeX typesets, and an operator, relation or arrow, or a letter of
/// the letterlike or mathematical alphanumeric symbols (`ℝ`, `𝔸`), but not
/// a letter of a script (`α`), which is text.
fn is_math_symbol(c: char) -> bool {
    needs_unicode_engine(c)
        && (!c.is_alphabetic() || matches!(c, '\u{2100}'..='\u{214F}' | '\u{1D400}'..='\u{1D7FF}'))
}

/// [`escape`] for `\texttt`, where hyphen runs and quotes must stay as
/// typed rather than becoming dashes and curly quotes. Its
/// [`SHARED_PUNCTUATION`] is written as its flags, `latin`, say, as in
/// [`latin_text`].
fn escape_code(code: &str, latin: &LatinFlags) -> String {
    let mut out = String::with_capacity(code.len());
    let mut latin = latin.iter().copied();
    for c in code.chars() {
        match c {
            '-' => out.push_str("-{}"),
            '`' => out.push_str("\\textasciigrave{}"),
            '\'' => out.push_str("\\textquotesingle{}"),
            '<' => out.push_str("\\textless{}"),
            '>' => out.push_str("\\textgreater{}"),
            c => push_text_char(&mut out, c, &mut latin),
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

    /// An empty anchor alone becomes a label links reach; raw HTML with
    /// more in it keeps its text.
    #[test]
    fn only_a_lone_empty_anchor_becomes_a_label() {
        let raw = |text: &str| Document {
            meta: Default::default(),
            blocks: vec![Block::Raw {
                format: RawFormat::Html,
                text: text.into(),
            }],
        };
        let anchor = write(&raw("<a id=\"equation:1\"></a>"));
        assert!(
            anchor.contains("\\phantomsection\\label{equation:3a:1}"),
            "{anchor}"
        );
        let more = write(&raw("<a id=\"x\">Important</a><a id=\"y\"></a>"));
        assert!(
            more.contains("Important") && !more.contains("\\label"),
            "{more}"
        );
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

    /// Regression for #58: math in a heading, typed (`α`) or converted
    /// (typst's `:=` is `≔`), is under its LaTeX names in the short form,
    /// the bookmark text and the label too, and is math in the references
    /// that show the heading's text, so a Latin document stays one that
    /// pdfLaTeX takes.
    #[test]
    fn heading_math_keeps_latin_headings_ascii() {
        let tex = write(&read::typst::read(
            "#set heading(numbering: \"1.\")\n= Definitions $f := g$ <d>\n\n\
             = Heading with $α ∈ ℝ$ <h>\n\nSee @d, @h and #ref(<h>).\n",
        ));
        assert!(!tex.contains("\\errmessage"), "{tex}");
        let text = &tex[tex.find("\\begin{document}").unwrap()..];
        assert!(text.is_ascii(), "{text}");
        assert!(
            text.contains(
                "See \\hyperref[definitions-f-g]{Definitions \\(f \\coloneqq g\\)}, \
                 \\hyperref[heading-with-:3b1:-:211d:]{Heading with \\(\\alpha \\in \\mathbb{R}\\)} and"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "\\section[Heading with \\texorpdfstring{\\(\\alpha \\in \\mathbb{R}\\)}\
                 {\\textbackslash{}alpha \\textbackslash{}in \\textbackslash{}mathbb\\{R\\}}]\
                 {\\texorpdfstring{Heading with \\(\\alpha \\in \\mathbb{R}\\)}\
                 {Heading with \\textbackslash{}alpha \\textbackslash{}in \\textbackslash{}mathbb\\{R\\}}}\
                 \\label{heading-with-:3b1:-:211d:}"
            ),
            "{text}"
        );
        // A link reaches a label like that through the same key, and ids
        // that differ get keys that differ, whatever characters they have.
        let linked = body("# Café\n\n[back](#café)\n");
        assert!(
            linked.contains("\\label{caf:e9:}") && linked.contains("\\hyperref[caf:e9:]{back}"),
            "{linked}"
        );
        let ids = [
            "α", "ux3b1", "αb", "\u{3b1b}", "a:3b1:", "a b", "a-b", "a_b",
        ];
        let labels: HashSet<String> = ids.iter().map(|id| label(id)).collect();
        assert_eq!(labels.len(), ids.len(), "{labels:?}");
        assert!(labels.iter().all(|l| l.is_ascii()), "{labels:?}");
    }

    /// Regression for #58: readers flatten the math in a title or a
    /// figure's caption, which hold only text, to its TeX (typst's
    /// `$f := g$` is `f ≔ g`). There its symbols are math, under their
    /// names, and a title that's never typeset doesn't count, so a Latin
    /// document stays one that pdfLaTeX takes. A letter of a script stays
    /// text.
    #[test]
    fn math_flattened_into_titles_and_captions_stays_latin() {
        let tex = write(&read::typst::read(
            "#set document(title: [Sets $f := g$ in $ℝ$])\n\n\
             #figure(image(\"x.png\"), caption: [Caption $a <= b$])\n",
        ));
        assert!(!tex.contains("\\errmessage") && tex.is_ascii(), "{tex}");
        assert!(
            tex.contains("\\title{Sets f \\ensuremath{\\coloneqq} g in \\ensuremath{\\mathbb{R}}}"),
            "{tex}"
        );
        assert!(tex.contains("\\def\\coloneqq{"), "{tex}");
        assert!(
            tex.contains("\\caption{Caption a \\ensuremath{\\leq} b}"),
            "{tex}"
        );
        let opening = write(&read::typst::read(
            "#set document(title: [Sets $α$])\n\n= Sets $α$\n\nText.\n",
        ));
        assert!(
            !opening.contains("\\maketitle") && !opening.contains("\\errmessage"),
            "{opening}"
        );
        let greek = write(&markdown::read("---\ntitle: Ελληνικά ≤\n---\n\nText.\n"));
        assert!(
            greek.contains("\\title{Ελληνικά \\ensuremath{\\leq}}")
                && greek.contains("\\errmessage"),
            "{greek}"
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
        // Alternating kinds nest past LaTeX's six lists in all, which the
        // preamble lifts, until one kind passes four.
        let mixed = body(
            "- a\n  1. b\n     - c\n       1. d\n          - e\n            1. f\n\
             \x20              - g\n                 1. h\n                    - i\n",
        );
        let opened =
            mixed.matches("\\begin{itemize}").count() + mixed.matches("\\begin{enumerate}").count();
        assert_eq!(opened, 2 * MAX_LIST_DEPTH, "{mixed}");
        assert!(mixed.contains("\\par \\textbullet~i"), "{mixed}");
        assert!(deep.contains("\\par \\textbullet~5"), "{deep}");
    }

    /// Regression for #67: a quote nested past LaTeX's six lists is still a
    /// `quote`, so the code, rules and list labels inside are indented with
    /// it, as typst indents them, and pandoc reads it back as one. Lists go
    /// on nesting inside it too. The preamble indents these levels past six
    /// by 1em on the left only, while the line is at least 10em wide, so no
    /// depth leaves the text without room.
    #[test]
    fn quotes_nest_past_the_latex_list_limit() {
        let tex = write(&markdown::read("> > > > > > > plus seven\n"));
        assert!(
            tex.contains(
                "\\def\\@toodeep{\\global\\advance\\@listdepth\\@ne \\leftmargin\\z@\n  \
                 \\ifdim\\linewidth<10em \\else\n    \
                 \\advance\\@totalleftmargin1em \\advance\\linewidth-1em\n  \\fi}"
            ),
            "{tex}"
        );
        let deep = body("> > > > > > > plus seven\n>\n> back to one\n");
        assert_eq!(deep.matches("\\begin{quote}").count(), 7, "{deep}");
        assert!(
            deep.contains("\\begin{quote}\nplus seven\n\\end{quote}"),
            "{deep}"
        );
        let list = body("> > > > > > - item\n");
        assert!(
            list.contains("\\begin{itemize}\n\\tightlist\n\\item item"),
            "{list}"
        );
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

    /// Regression for #44: LaTeX drops the text of a `\footnote` inside
    /// another's. A note first referred to inside a note is marked there
    /// and set after the outermost note, numbered as typst numbers it: the
    /// notes inside a note come right after it, before the next one, which
    /// `\setcounter` numbers past them.
    #[test]
    fn notes_inside_notes_are_set_after_the_outermost() {
        let tex = body(
            "A[^1] B[^4].\n\n[^1]: Outer[^2] and[^3].\n[^2]: Inner note[^5].\n\
             [^3]: Second.\n[^4]: Next.\n[^5]: Level two.\n",
        );
        assert_eq!(
            tex,
            "A\\footnote{\\label{fn:1}Outer\\mbox{\\footnotemark[2]} and\\mbox{\\footnotemark[4]}.}\
             \\footnotetext[2]{\\phantomsection\\label{fn:2}Inner note\\mbox{\\footnotemark[3]}.}\
             \\footnotetext[3]{\\phantomsection\\label{fn:3}Level two.}\
             \\footnotetext[4]{\\phantomsection\\label{fn:4}Second.}\
             \\setcounter{footnote}{4} B\\footnote{\\label{fn:5}Next.}."
        );
    }

    /// A longtable sets its head once, for every page, so the notes first
    /// referred to there are only marked. LaTeX sets the notes' texts in
    /// the order it meets them, so theirs come at the start of the first
    /// row, before that row's own notes, or after a table without rows.
    #[test]
    fn notes_in_a_table_head_are_set_in_number_order() {
        let tex = body(
            "A[^a].\n\n| H[^h1] | I[^h2] |\n|---|---|\n| b[^b] | c |\n| d | e |\n\n\
             [^a]: Before.\n[^h1]: Head one.\n[^h2]: Head two.\n[^b]: Body.\n",
        );
        assert!(
            tex.contains(
                "H\\mbox{\\footnotemark[2]} & I\\mbox{\\footnotemark[3]} \\\\\n\\midrule\n\
                 \\endhead\n\\footnotetext[2]{\\phantomsection\\label{fn:2}Head one.}\
                 \\footnotetext[3]{\\phantomsection\\label{fn:3}Head two.}\
                 \\setcounter{footnote}{3}b\\footnote{\\label{fn:4}Body.} & c \\\\\nd & e \\\\\n"
            ),
            "{tex}"
        );
        let head_only = body("| H[^h] |\n|---|\n\n[^h]: Head.\n");
        assert!(
            head_only.ends_with(
                "\\end{longtable}\\footnotetext[1]{\\phantomsection\\label{fn:1}Head.}\
                 \\setcounter{footnote}{1}"
            ),
            "{head_only}"
        );
    }

    /// Notes that each refer to the next nest as deep as the chain is long,
    /// which the writer follows only [`MAX_NOTE_NESTING`] notes deep, on the
    /// 2 MiB stack conversions run on: a note first referred to deeper has
    /// its text written when the others are set. Every note has its text
    /// once, under a number of its own, its reference plain or deep in
    /// quotes, a list and formatting, whose frames each note takes again.
    #[test]
    fn chained_notes_stay_within_the_stack() {
        const NOTES: usize = 5000;
        for nesting in ["", "> > > > > > > > - *a **b [c](u) "] {
            let mut md = String::from("Start[^1].\n\n");
            for i in 1..NOTES {
                md.push_str(&format!("[^{i}]:\n    {nesting}Note {i}[^{}].\n\n", i + 1));
            }
            md.push_str(&format!("[^{NOTES}]: Last.\n"));
            let tex = std::thread::Builder::new()
                .stack_size(2 << 20)
                .spawn(move || body(&md))
                .unwrap()
                .join()
                .unwrap();
            let numbers = |command: &str, end: char| -> Vec<usize> {
                tex.split(command)
                    .skip(1)
                    .map(|rest| rest[..rest.find(end).unwrap()].parse().unwrap())
                    .collect()
            };
            assert_eq!(numbers("\\label{fn:", '}'), (1..=NOTES).collect::<Vec<_>>());
            assert_eq!(
                numbers("\\footnotemark[", ']'),
                (2..=NOTES).collect::<Vec<_>>()
            );
            assert_eq!(
                numbers("\\footnotetext[", ']'),
                (2..=NOTES).collect::<Vec<_>>()
            );
            assert!(
                tex.ends_with(&format!("\\setcounter{{footnote}}{{{NOTES}}}.")),
                "{}",
                &tex[tex.len() - 200..]
            );
        }
    }

    /// Past [`MAX_NOTE_NESTING`], the notes inside a note are numbered
    /// after the others that the outermost note holds, not right after it:
    /// here the last note of a chain comes after the sibling of its first.
    #[test]
    fn notes_past_the_nesting_cap_are_numbered_last() {
        let last = MAX_NOTE_NESTING + 2;
        let mut md = String::from("Start[^1].\n\n[^1]: First[^c2] and[^s].\n[^s]: Sibling.\n");
        for i in 2..last {
            md.push_str(&format!("[^c{i}]: Chain {i}[^c{}].\n", i + 1));
        }
        md.push_str(&format!("[^c{last}]: Chain end.\n"));
        let tex = body(&md);
        // The labels' numbers, from [`Footnotes`], put the whole chain
        // right after the first note: chain note `i`'s is `i`, and the
        // sibling's comes after the chain's end.
        let (deferred, sibling, end) = (last - 1, last, last + 1);
        for written in [
            format!("First\\mbox{{\\footnotemark[2]}} and\\mbox{{\\footnotemark[{sibling}]}}."),
            format!(
                "\\footnotetext[{deferred}]{{\\phantomsection\\label{{fn:{deferred}}}Chain {deferred}\
                 \\mbox{{\\footnotemark[{end}]}}.}}\
                 \\footnotetext[{sibling}]{{\\phantomsection\\label{{fn:{end}}}Sibling.}}\
                 \\footnotetext[{end}]{{\\phantomsection\\label{{fn:{last}}}Chain end.}}\
                 \\setcounter{{footnote}}{{{end}}}."
            ),
        ] {
            assert!(tex.contains(&written), "{written} in {tex}");
        }
    }

    /// A formula that ends in a comment (`% note`) ends its line, which
    /// would take the `\)` after it, or the `}` of a note's text after a
    /// display environment, for part of the comment: inline, in a heading,
    /// and in a note. An escaped `%` or one in `\verb` is no comment.
    #[test]
    fn math_ending_in_a_comment_ends_its_line() {
        let tex = body(
            "# Head $a % c$\n\nText $b % note$, $50\\%$ and $\\verb|%|$.\n\nRef[^n].\n\n\
             [^n]: $$\\begin{align}x &= y\\end{align} % end$$\n",
        );
        for written in [
            "\\section[Head \\texorpdfstring{\\(a % c\n\\)}{a \\% c}]\
             {\\texorpdfstring{Head \\(a % c\n\\)}{Head a \\% c}}",
            "Text \\(b % note\n\\), \\(50\\%\\) and \\(\\verb|%|\\).",
            "\\begin{align}x &= y\\end{align} % end\n}",
        ] {
            assert!(tex.contains(written), "{written} in {tex}");
        }
    }

    /// A heading's short form, which LaTeX writes to the table of contents
    /// and reads back, protects each control word of its math where the
    /// math defines a command, which would be expanded on the way.
    #[test]
    fn heading_math_defining_commands_is_protected() {
        let tex = body("# New $\\newcommand{\\coloneqq}{\\equiv} a \\coloneqq b$\n");
        assert!(
            tex.starts_with(
                "\\section[New \\texorpdfstring{\\(\\protect\\renewcommand\
                 {\\protect\\coloneqq}{\\protect\\equiv} a \\protect\\coloneqq b\\)}"
            ),
            "{tex}"
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

    /// Regression for #53: xeCJK sets “ ” ‘ ’ – — … · as CJK punctuation,
    /// which takes the spaces next to it. In a document with CJK text, the
    /// ones among Latin text are its text commands, which it sets as
    /// Latin; next to CJK text, even across formatting, they stay CJK.
    #[test]
    fn shared_punctuation_is_latin_among_latin_text() {
        let tex = body(
            "The “raw” data — and *“draft”* ones… it’s a ‘test’ – a · b.\n\n\
             他说“你好”——**“好”**……\n",
        );
        assert!(
            tex.contains(
                "The \\textquotedblleft{}raw\\textquotedblright{} data \\textemdash{} and \
                 \\emph{\\textquotedblleft{}draft\\textquotedblright{}} ones\\textellipsis{} \
                 it\\textquoteright{}s a \\textquoteleft{}test\\textquoteright{} \\textendash{} \
                 a \\textperiodcentered{} b."
            ),
            "{tex}"
        );
        assert!(tex.contains("他说“你好”——\\textbf{“好”}……"), "{tex}");
        // A space beside it stays, so it's Latin, CJK quoted in English
        // included. A pair of quotes is one font's, at a paragraph's edge
        // too, and a doubled `……` is CJK. Code is text in this too.
        let tex = body(
            "He said “你好” to me. “中文” is Chinese, and ‘字’ too. 中文… English.\n\n\
             我們稱之為“machine learning”\n\n“Python”是一種語言\n\n\
             更多的API……\n\n他说：“Hello……”\n\n`say “hi” — now` 中文\n",
        );
        for written in [
            "He said \\textquotedblleft{}你好\\textquotedblright{} to me.",
            "\\textquotedblleft{}中文\\textquotedblright{} is Chinese, and \
             \\textquoteleft{}字\\textquoteright{} too.",
            "中文\\textellipsis{} English.",
            "我們稱之為“machine learning”",
            "“Python”是一種語言",
            "更多的API……",
            "他说：“Hello……”",
            "\\texttt{say \\textquotedblleft{}hi\\textquotedblright{} \\textemdash{} now} 中文",
        ] {
            assert!(tex.contains(written), "{written} in {tex}");
        }
        // CJK in code alone loads xeCJK as well.
        let code = body("“Quoted” — text.\n\n```\n// 註解\n```\n");
        assert!(
            code.starts_with(
                "\\textquotedblleft{}Quoted\\textquotedblright{} \\textemdash{} text."
            ),
            "{code}"
        );
        // Without CJK text, xeCJK isn't loaded and the text stays as typed.
        assert_eq!(body("The “raw” data — and\n"), "The “raw” data — and");
    }

    /// Regression for #53: naming the xeCJK font mustn't cost the system
    /// font scan, which the CLI never warms up. The scan happens once per
    /// process and other tests trigger it, so the test runs itself again in
    /// a fresh process.
    #[test]
    fn naming_the_cjk_font_skips_the_font_scan() {
        const CHILD: &str = "SWIFT_SHIFTER_COLD_FONTS_TEST";
        if std::env::var_os(CHILD).is_some() {
            let tex = write(&markdown::read("中文內容\n"));
            assert!(tex.contains("\\setCJKmainfont{"), "{tex}");
            assert!(!typst::fonts_scanned(), "writing LaTeX scanned fonts");
            return;
        }
        // Test names leave out the crate.
        let name = concat!(module_path!(), "::naming_the_cjk_font_skips_the_font_scan");
        let name = name.split_once("::").map_or(name, |(_, path)| path);
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([name, "--exact", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let report = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && report.contains("1 passed"),
            "{report}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

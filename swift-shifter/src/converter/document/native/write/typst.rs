//! [`Document`] → typst markup with Swift Shifter's page template. Used to
//! typeset PDFs in-process and to write `.typ` files.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;

use super::{BlockWriter, Footnotes, indent_rest, renderable_tex};
use crate::converter::document::native::ast::{
    Align, Block, Document, Inline, List, Meta, RawFormat, Table, inline_text,
};
use crate::converter::document::native::typst::{VIRTUAL_DIR, tex_to_typst};
use crate::converter::document::native::{local_path, percent_decode, read};

/// What the markup is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// Compiled by Swift Shifter: math uses the embedded mitex scope and
    /// local images are registered as virtual files.
    Pdf,
    /// A standalone `.typ` file for the user's own typst: math uses the
    /// `@preview/mitex` package and image paths are kept as written.
    TypstFile,
}

pub struct Options<'a> {
    pub target: Target,
    /// The `text(font: …)` list.
    pub fonts: Vec<String>,
    /// Typst paper name, e.g. "a4" or "us-letter".
    pub paper: &'static str,
    /// Where relative image paths resolve.
    pub base_dir: PathBuf,
    /// Formulas (by index in document order) to show as TeX code because
    /// they didn't compile.
    pub tex_fallback: &'a HashSet<usize>,
}

pub struct Output {
    pub source: String,
    /// Byte range in `source` of each formula, by index.
    pub formulas: Vec<Range<usize>>,
    /// Virtual image paths → files on disk (PDF target).
    pub images: HashMap<String, PathBuf>,
}

/// The mitex package version `.typ` output imports.
const MITEX_PACKAGE: &str = "@preview/mitex:0.2.7";

pub fn write(doc: &Document, options: &Options) -> Output {
    let (notes, note_bodies) = Footnotes::collect(doc);
    let w = Writer {
        options,
        notes,
        note_bodies,
        notes_written: HashSet::new().into(),
        formulas: Vec::new().into(),
        images: HashMap::new().into(),
    };
    let body = w.blocks(&doc.blocks);
    let formulas = w.formulas.into_inner();
    let opens_with_title = matches!(
        doc.blocks.first(),
        Some(Block::Heading { content, .. }) if Some(inline_text(content).as_str()) == doc.meta.title.as_deref()
    );
    let extras = PreambleExtras {
        has_math: !formulas.is_empty(),
        opens_with_title,
    };
    let preamble = preamble(&doc.meta, doc.effective_lang().as_deref(), options, extras);
    let source = format!("{preamble}\n{body}\n");
    // Locate each formula now that the text is final. Formulas appear in
    // order, so each search starts after the previous match; the marker
    // makes the text unique, except possibly inside earlier code blocks.
    let mut from = 0;
    let formulas = formulas
        .iter()
        .map(|expr| {
            let start = from
                + source[from..]
                    .find(expr.as_str())
                    .expect("formula is in the source");
            from = start + expr.len();
            start..from
        })
        .collect();
    Output {
        source,
        formulas,
        images: w.images.into_inner(),
    }
}

/// What the preamble needs to know about the body that follows it.
#[derive(Clone, Copy, Default)]
pub struct PreambleExtras {
    /// The body has formulas (import the math scope or package).
    pub has_math: bool,
    /// The body starts with a heading equal to the title (skip the title
    /// block).
    pub opens_with_title: bool,
}

/// Imports, page setup and styles, then a title block from `meta`. Also used
/// in front of typst written by pandoc (epub/tex → pdf), which needs
/// `horizontalrule`.
pub fn preamble(
    meta: &Meta,
    lang: Option<&str>,
    options: &Options,
    extras: PreambleExtras,
) -> String {
    let PreambleExtras {
        has_math,
        opens_with_title,
    } = extras;
    let mut p = String::new();
    if has_math {
        match options.target {
            Target::Pdf => p.push_str(&format!(
                "#import \"{VIRTUAL_DIR}/mitex/specs/mod.typ\": mitex-scope\n"
            )),
            Target::TypstFile => p.push_str(&format!("#import \"{MITEX_PACKAGE}\": mitex, mi\n")),
        }
    }
    p.push_str(
        "#let horizontalrule = align(center, line(length: 30%, stroke: 0.5pt + luma(150)))\n",
    );
    let title = meta.title.as_deref().unwrap_or("");
    let authors: Vec<String> = meta.authors.iter().map(|a| typst_str(a)).collect();
    p.push_str(&format!(
        "#set document(title: {}, author: ({}))\n",
        typst_str(title),
        authors.join(", ") + if authors.len() == 1 { "," } else { "" }
    ));
    p.push_str(&format!(
        "#set page(paper: {}, margin: 2.5cm, numbering: \"1\")\n",
        typst_str(options.paper)
    ));
    let fonts: Vec<String> = options.fonts.iter().map(|f| typst_str(f)).collect();
    let (lang, region) = lang_region(lang);
    let mut text = format!("font: ({},), size: 11pt", fonts.join(", "));
    if let Some(lang) = lang {
        text.push_str(&format!(", lang: {}", typst_str(&lang)));
    }
    if let Some(region) = region {
        text.push_str(&format!(", region: {}", typst_str(&region)));
    }
    p.push_str(&format!("#set text({text})\n"));
    p.push_str(
        "#set par(justify: true, leading: 0.75em)\n\
         #show raw: set text(font: \"DejaVu Sans Mono\", size: 0.9em)\n\
         #show raw.where(block: true): block.with(fill: luma(245), inset: 8pt, radius: 4pt, width: 100%)\n\
         #show link: set text(fill: rgb(\"#0a66c2\"))\n\
         #show heading: set block(above: 1.4em, below: 0.8em)\n\
         #show quote.where(block: true): it => block(inset: (left: 1em, y: 0.2em), stroke: (left: 2pt + luma(200)), it.body)\n",
    );
    if !title.is_empty() && !opens_with_title {
        p.push_str(&format!(
            "#align(center)[#text(size: 1.7em, weight: \"bold\", {})",
            typst_str(title)
        ));
        if !meta.authors.is_empty() {
            p.push_str(&format!(" \\ #{}", typst_str(&meta.authors.join(", "))));
        }
        p.push_str("]\n");
    }
    p
}

/// Typst's `lang` (ISO 639) and `region` (ISO 3166) from a BCP 47 tag.
fn lang_region(tag: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(tag) = tag else {
        return (None, None);
    };
    let mut parts = tag.split(['-', '_']);
    let lang = parts
        .next()
        .map(str::to_ascii_lowercase)
        .filter(|l| (2..=3).contains(&l.len()));
    let mut region = None;
    for part in parts {
        match part.len() {
            2 if part.chars().all(|c| c.is_ascii_alphabetic()) => {
                region = Some(part.to_ascii_uppercase())
            }
            // Script subtags: Traditional Chinese implies Taiwan conventions.
            4 if part.eq_ignore_ascii_case("hant") && region.is_none() => {
                region = Some("TW".into())
            }
            _ => {}
        }
    }
    (lang, region)
}

struct Writer<'a> {
    options: &'a Options<'a>,
    notes: Footnotes,
    note_bodies: Vec<Vec<Block>>,
    /// Notes whose body has been written once; later references point to it.
    notes_written: std::cell::RefCell<HashSet<usize>>,
    /// Each formula's emitted expression, in order (unique by its marker).
    formulas: std::cell::RefCell<Vec<String>>,
    images: std::cell::RefCell<HashMap<String, PathBuf>>,
}

impl BlockWriter for Writer<'_> {
    fn block(&self, block: &Block) -> Option<String> {
        Some(match block {
            Block::Heading { level, content } => {
                format!(
                    "{} {}",
                    "=".repeat(usize::from(*level.clamp(&1, &6))),
                    self.inlines(content, Ctx::Heading)
                )
            }
            Block::Paragraph(content) => match content.as_slice() {
                // An image on its own is a figure.
                [Inline::Image { url, alt, .. }] => self.figure(url, alt),
                _ => self.inlines(content, Ctx::Block),
            },
            Block::Quote(children) => format!("#quote(block: true)[\n{}\n]", self.blocks(children)),
            Block::List(list) => self.list(list),
            Block::Code { lang, code } => {
                let fence = "`".repeat(longest_run(code, '`').max(2) + 1);
                format!("{fence}{}\n{code}\n{fence}", lang.as_deref().unwrap_or(""))
            }
            Block::Math(tex) => self.math(tex, true),
            Block::Table(table) => self.table(table),
            Block::ThematicBreak => "#horizontalrule".to_string(),
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

/// Where inline content is written: headings must stay on one line.
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Block,
    Heading,
}

impl Writer<'_> {
    fn list(&self, list: &List) -> String {
        let marker = if list.start.is_some() { "+ " } else { "- " };
        let sep = if list.tight { "\n" } else { "\n\n" };
        let items: Vec<String> = list
            .items
            .iter()
            .map(|item| {
                // Text-font ballot boxes (DejaVu has them), not whichever
                // colour emoji font fallback would pick.
                let task = match item.task {
                    Some(true) => "#text(font: \"DejaVu Sans Mono\")[☑] ",
                    Some(false) => "#text(font: \"DejaVu Sans Mono\")[☐] ",
                    None => "",
                };
                let body = self.blocks_joined(&item.content, sep);
                format!("{marker}{task}{}", indent_rest(&body, "  "))
            })
            .collect();
        let items = items.join(sep);
        match list.start {
            Some(start) if start != 1 => format!("#[\n#set enum(start: {start})\n{items}\n]"),
            _ => items,
        }
    }

    fn table(&self, table: &Table) -> String {
        let columns = table
            .header
            .len()
            .max(table.rows.iter().map(Vec::len).max().unwrap_or(0))
            .max(1);
        let align: Vec<&str> = (0..columns)
            .map(
                |i| match table.align.get(i).copied().unwrap_or(Align::Default) {
                    Align::Default => "auto",
                    Align::Left => "left",
                    Align::Center => "center",
                    Align::Right => "right",
                },
            )
            .collect();
        let cell = |c: &[Inline]| format!("[{}]", self.inlines(c, Ctx::Block));
        let mut out = format!(
            "#table(\n  columns: {columns},\n  align: ({},),\n",
            align.join(", ")
        );
        if !table.header.is_empty() {
            let cells: Vec<String> = (0..columns)
                .map(|i| {
                    table
                        .header
                        .get(i)
                        .map(|c| cell(c))
                        .unwrap_or_else(|| "[]".into())
                })
                .collect();
            out.push_str(&format!("  table.header({}),\n", cells.join(", ")));
        }
        for row in &table.rows {
            let cells: Vec<String> = (0..columns)
                .map(|i| row.get(i).map(|c| cell(c)).unwrap_or_else(|| "[]".into()))
                .collect();
            out.push_str(&format!("  {},\n", cells.join(", ")));
        }
        out.push(')');
        out
    }

    fn figure(&self, url: &str, alt: &str) -> String {
        match self.image_path(url) {
            Some(path) if alt.is_empty() => format!("#image({})", typst_str(&path)),
            Some(path) => format!(
                "#figure(image({}, alt: {}), caption: [{}])",
                typst_str(&path),
                typst_str(alt),
                escape(alt)
            ),
            None => format!(
                "\\[{}\\]",
                escape(if alt.is_empty() { "image" } else { alt })
            ),
        }
    }

    /// The path to give typst for an image, or `None` when it can't be
    /// typeset (remote, missing, or a format typst can't read).
    fn image_path(&self, url: &str) -> Option<String> {
        if self.options.target == Target::TypstFile {
            return (!url.contains("://") || url.starts_with("file://")).then(|| {
                url::Url::parse(url)
                    .ok()
                    .and_then(|u| u.to_file_path().ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| {
                        let path = url.split(['?', '#']).next().unwrap_or(url);
                        percent_decode(path)
                    })
            });
        }
        let path = local_path(url, &self.options.base_dir)?;
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if !matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp"
        ) {
            return None;
        }
        let mut images = self.images.borrow_mut();
        let vpath = format!("{VIRTUAL_DIR}/img/{}.{ext}", images.len());
        images.insert(vpath.clone(), path);
        Some(vpath)
    }

    /// A formula as an embedded expression. Every form carries a
    /// `/*fN*/` marker comment that makes its text unique, so its position
    /// in the final source can be found (see `write`).
    fn math(&self, tex: &str, block: bool) -> String {
        let mut formulas = self.formulas.borrow_mut();
        let index = formulas.len();
        let marker = format!("/*f{index}*/");
        let tex_code = || {
            format!(
                "#raw({marker}block: {block}, lang: \"latex\", {})",
                typst_str(tex)
            )
        };
        let expr = match self.options.target {
            _ if self.options.tex_fallback.contains(&index) => tex_code(),
            Target::Pdf => match tex_to_typst(&renderable_tex(tex)) {
                Ok(code) => format!(
                    "#math.equation(block: {block}, eval({marker}{}, mode: \"math\", scope: mitex-scope))",
                    typst_str(&code)
                ),
                Err(_) => tex_code(),
            },
            Target::TypstFile => {
                let func = if block { "mitex" } else { "mi" };
                format!("#{func}({marker}{})", typst_str(&renderable_tex(tex)))
            }
        };
        formulas.push(expr.clone());
        expr
    }

    fn inlines(&self, inlines: &[Inline], ctx: Ctx) -> String {
        let mut out = String::new();
        let mut after_expr = false;
        for inline in inlines {
            after_expr = self.inline(&mut out, inline, ctx, after_expr);
        }
        out
    }

    /// Append `inline`; returns whether it ended with an embedded
    /// expression (so a following `.` or `(` must be escaped).
    fn inline(&self, out: &mut String, inline: &Inline, ctx: Ctx, after_expr: bool) -> bool {
        let wrap = |out: &mut String, func: &str, content: &[Inline]| {
            out.push_str(&format!("#{func}[{}]", self.inlines(content, ctx)));
            true
        };
        let text = |out: &mut String, text: &str| {
            escape_into(out, text);
            false
        };
        match inline {
            Inline::Text(t) => {
                if after_expr && (t.starts_with('.') || t.starts_with('(')) {
                    out.push('\\');
                }
                text(out, t)
            }
            Inline::Emph(c) => wrap(out, "emph", c),
            Inline::Strong(c) => wrap(out, "strong", c),
            Inline::Strikethrough(c) => wrap(out, "strike", c),
            Inline::Superscript(c) => wrap(out, "super", c),
            Inline::Subscript(c) => wrap(out, "sub", c),
            Inline::Code(code) => {
                out.push_str(&format!("#raw({})", typst_str(code)));
                true
            }
            Inline::Math(tex) => {
                out.push_str(&self.math(tex, false));
                true
            }
            Inline::Link { url, content, .. } => {
                if content.as_slice() == [Inline::Text(url.clone())] {
                    out.push_str(&format!("#link({})", typst_str(url)));
                } else {
                    out.push_str(&format!(
                        "#link({})[{}]",
                        typst_str(url),
                        self.inlines(content, ctx)
                    ));
                }
                true
            }
            Inline::Image { url, alt, .. } => match self.image_path(url) {
                Some(path) => {
                    out.push_str(&format!("#box(image({}, height: 1.2em))", typst_str(&path)));
                    true
                }
                None => text(
                    out,
                    &format!("[{}]", if alt.is_empty() { "image" } else { alt }),
                ),
            },
            Inline::FootnoteRef(label) => match self.notes.number(label) {
                Some(n) => {
                    out.push_str(&self.footnote(n));
                    true
                }
                None => text(out, &format!("[^{label}]")),
            },
            Inline::LineBreak => {
                out.push_str(if ctx == Ctx::Heading { " " } else { " \\\n" });
                false
            }
            Inline::SoftBreak => {
                out.push_str(if ctx == Ctx::Heading { " " } else { "\n" });
                false
            }
            Inline::Raw { text: html, .. } => text(out, &read::html::text_of(html)),
        }
    }

    /// The first reference to a note carries its body and a label; later
    /// ones refer back to it.
    fn footnote(&self, n: usize) -> String {
        if !self.notes_written.borrow_mut().insert(n) {
            return format!("#footnote(<fn-{n}>)");
        }
        let body = self
            .note_bodies
            .get(n - 1)
            .map(|b| self.blocks(b))
            .unwrap_or_default();
        format!("#footnote[{body}] <fn-{n}>")
    }
}

/// A typst string literal.
pub(crate) fn typst_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    escape_into(&mut out, text);
    out
}

/// Append text as typst markup, escaping what typst would interpret.
/// Whether a character opens a line is read from what's already in `out`.
fn escape_into(out: &mut String, text: &str) {
    let chars: Vec<char> = text.chars().collect();
    let mut blank_line = out
        .rsplit('\n')
        .next()
        .unwrap_or("")
        .chars()
        .all(|c| c == ' ');
    let mut digits_only = {
        let line = out.rsplit('\n').next().unwrap_or("");
        !line.is_empty() && line.chars().all(|c| c.is_ascii_digit())
    };
    for (i, &c) in chars.iter().enumerate() {
        let next = chars.get(i + 1).copied();
        let needs = match c {
            '\\' | '#' | '$' | '*' | '_' | '`' | '<' | '>' | '@' | '[' | ']' | '~' | '/' => true,
            // "--" and "---" are dashes, "-?" a soft hyphen.
            '-' => {
                matches!(next, Some('-' | '?')) || (blank_line && matches!(next, None | Some(' ')))
            }
            '+' | '=' => blank_line,
            '.' => digits_only,
            _ => false,
        };
        if needs {
            out.push('\\');
        }
        out.push(c);
        if c == ' ' {
            digits_only = false;
        } else {
            digits_only = (blank_line || digits_only) && c.is_ascii_digit();
            blank_line = false;
        }
    }
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for ch in text.chars() {
        run = if ch == c { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::read::markdown;

    fn opts(target: Target, fallback: &HashSet<usize>) -> Options<'_> {
        Options {
            target,
            fonts: vec!["Libertinus Serif".into()],
            paper: "a4",
            base_dir: std::env::temp_dir(),
            tex_fallback: fallback,
        }
    }

    fn body(md: &str) -> String {
        let none = HashSet::new();
        let out = write(&markdown::read(md), &opts(Target::Pdf, &none));
        let start = out.source.find("#show quote").unwrap();
        let after = &out.source[start..];
        after[after.find('\n').unwrap() + 1..].trim().to_string()
    }

    #[test]
    fn escapes_typst_markup() {
        assert_eq!(
            body("a #b \\$c\\* \\_d `e` \\<f> @g \\[h\\] \\~i // j\n"),
            "a \\#b \\$c\\* \\_d #raw(\"e\") \\<f\\> \\@g \\[h\\] \\~i \\/\\/ j"
        );
        assert_eq!(body("\\- item\n\n\\+ plus\n"), "\\- item\n\n\\+ plus");
        assert_eq!(
            body("1\\. not a list -- or dash\n"),
            "1\\. not a list \\-- or dash"
        );
    }

    #[test]
    fn writes_blocks() {
        assert_eq!(
            body("## Head\n\n- a\n- [x] b\n\n3. c\n"),
            "== Head\n\n- a\n- #text(font: \"DejaVu Sans Mono\")[☑] b\n\n#[\n#set enum(start: 3)\n+ c\n]"
        );
        assert_eq!(body("> quoted\n"), "#quote(block: true)[\nquoted\n]");
        assert!(body("| a | b |\n|:-|-:|\n| 1 | 2 |\n").contains(
            "columns: 2,\n  align: (left, right,),\n  table.header([a], [b]),\n  [1], [2],"
        ));
    }

    #[test]
    fn styled_runs_and_links() {
        assert_eq!(
            body("*em* **st** ~~del~~ [l](https://x.org) <https://y.org>.\n"),
            "#emph[em] #strong[st] #strike[del] #link(\"https://x.org\")[l] #link(\"https://y.org\")\\."
        );
    }

    #[test]
    fn footnotes_inline_with_labels() {
        assert_eq!(
            body("a[^n] b[^n]\n\n[^n]: Note.\n"),
            "a#footnote[Note.] <fn-1> b#footnote(<fn-1>)"
        );
    }

    #[test]
    fn math_uses_the_mitex_scope_and_falls_back_to_tex() {
        let doc = markdown::read("Inline $x^2$ and\n\n$$\\frac{a}{b}$$\n");
        let none = HashSet::new();
        let out = write(&doc, &opts(Target::Pdf, &none));
        assert!(out.source.contains("mitex-scope"));
        assert_eq!(out.formulas.len(), 2);
        assert!(out.source[out.formulas[1].clone()].contains("block: true"));

        let fallback: HashSet<usize> = [0].into();
        let out = write(&doc, &opts(Target::Pdf, &fallback));
        assert!(
            &out.source[out.formulas[0].clone()]
                == "#raw(/*f0*/block: false, lang: \"latex\", \"x^2\")",
            "{}",
            out.source
        );
    }

    #[test]
    fn typst_files_import_the_mitex_package() {
        let none = HashSet::new();
        let out = write(&markdown::read("$x$\n"), &opts(Target::TypstFile, &none));
        assert!(
            out.source
                .contains("#import \"@preview/mitex:0.2.7\": mitex, mi")
        );
        assert!(out.source.contains("#mi(/*f0*/\"x\")"));
    }

    #[test]
    fn typst_files_decode_relative_image_paths() {
        let none = HashSet::new();
        let doc = markdown::read("![a](my%20img.png?v=1) ![b](https://x.test/b.png)\n");
        let out = write(&doc, &opts(Target::TypstFile, &none));
        assert!(
            out.source.contains("image(\"my img.png\""),
            "{}",
            out.source
        );
        assert!(
            !out.source.contains("x.test"),
            "remote images aren't embedded"
        );
    }

    #[test]
    fn lang_and_region_from_bcp47() {
        assert_eq!(
            lang_region(Some("zh-Hant")),
            (Some("zh".into()), Some("TW".into()))
        );
        assert_eq!(
            lang_region(Some("en-US")),
            (Some("en".into()), Some("US".into()))
        );
        assert_eq!(lang_region(Some("ja")), (Some("ja".into()), None));
        assert_eq!(lang_region(None), (None, None));
    }
}

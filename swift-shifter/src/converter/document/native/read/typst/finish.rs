//! What's settled once the whole document is read (see
//! [`Reader::finish`]), and the marks the reader leaves for it until then
//! (see [`LABEL_LINK`]): links to labels, the text references to headings
//! show, and smart quotes (see [`Quoter`]).

use std::collections::HashMap;

use super::{Reader, heading_mark, push_inline};
use crate::converter::document::native::ast::{Block, Document, Inline, inline_text};
use crate::converter::document::native::read::markdown::push_text;
use crate::converter::document::native::read::walk;
use crate::converter::document::native::write::{self, DocumentIds, Footnotes};

// While the document is being read, links whose URL starts with one of
// these mark what can only be settled once more of it is known. No real
// URL starts with a control character, so no link is taken for a mark.
// None may survive `read`: every writer would emit it as it is.

/// Prefix of a link to a label; resolved once every label is known (see
/// [`Reader::finish`]).
pub(super) const LABEL_LINK: char = '\u{1}';
/// Prefix of the empty link that ends each heading, naming the heading's
/// id (see [`Reader::finish`]).
pub(super) const HEADING_MARK: char = '\u{2}';
/// Prefix of the empty link standing in for a smart quote, `"` or `'`,
/// until its paragraph is complete (see [`Quoter`]).
pub(super) const QUOTE_MARK: char = '\u{3}';
/// The URL of the link around a box's content until its paragraph's
/// quotes are settled: typst lays a box out on its own (see [`Quoter`]).
pub(super) const FRAME_MARK: char = '\u{4}';

/// What references to headings may copy in all, in bytes: the heading's
/// text they show and the link to it (see [`reference_cost`]). It's this,
/// or [`REFERENCE_TEXT_PER_BYTE`] for each byte of a longer document. Past
/// it, a reference is its label's text: no short source (a long heading
/// referred to again and again) makes the reader copy a huge text.
pub(super) const MAX_REFERENCE_TEXT: usize = 1 << 20;
/// What references may copy for each byte of a long document (see
/// [`MAX_REFERENCE_TEXT`]).
pub(super) const REFERENCE_TEXT_PER_BYTE: usize = 32;
/// What each inline of a heading's text a reference copies costs, past its
/// text's bytes (see [`reference_cost`]): a writer may write one at length
/// (math as MathML).
const REFERENCE_INLINE_COST: usize = 64;
/// How many headings deep a heading shows the text of the headings it
/// references (see [`HeadingTexts`]), past which a reference shows its
/// label: deeper than documents refer from heading to heading, and shallow
/// enough that a chain of such references neither overflows the stack nor
/// makes each heading's text as long as the chain.
pub(super) const MAX_HEADING_REFERENCES: usize = 16;

impl Reader {
    /// The document, from its `blocks` and the notes read with them, its
    /// marks settled. It takes the reader, so nothing is read after: no
    /// mark is left for the writers to see.
    ///
    /// A bare reference to a note's label becomes the note's mark, and
    /// label links point at the ids the writers will give the headings. A
    /// bare reference to a heading shows the heading's text. A link to a
    /// label on anything else, or on a heading no writer writes (in a note
    /// nothing references), keeps only its text (or the label's name, for
    /// a bare `@label`). Smart quotes are settled last, as they see the
    /// text references show.
    pub(super) fn finish(mut self, mut blocks: Vec<Block>) -> Document {
        blocks.append(&mut self.notes);
        let mut doc = Document {
            meta: std::mem::take(&mut self.meta),
            blocks,
        };
        // Notes first: the references to them decide which notes are
        // written, and in what order, so which headings get ids, and in
        // what order.
        walk::for_each_inlines(&mut doc.blocks, &mut |run| {
            for inline in run.iter_mut() {
                if let Inline::Link { url, content, .. } = inline
                    && content.is_empty()
                    && let Some(label) = url.strip_prefix(LABEL_LINK)
                    && !self.heading_labels.contains_key(label)
                    && let Some(note) = self.note_labels.get(label)
                {
                    *inline = Inline::FootnoteRef(note.clone());
                }
            }
        });
        // Then the written headings' text, which their ids are made of,
        // with the references in it settled.
        let (_, note_bodies) = Footnotes::collect(&doc);
        let mut texts = HeadingTexts {
            labels: &self.heading_labels,
            unsettled: HashMap::new(),
            settled: HashMap::new(),
            text_left: self.reference_text_left,
        };
        let mut marks = Vec::new();
        write::for_each_heading(&doc.blocks, &note_bodies, &mut |_, content| {
            if let Some(mark) = heading_mark(content) {
                marks.push(mark);
                texts.unsettled.insert(mark, content.to_vec());
            }
        });
        for mark in marks {
            texts.settle(mark, 0);
        }
        walk::for_each_block(&mut doc.blocks, &mut |block| {
            if let Block::Heading { content, .. } = block
                && let Some(mark) = heading_mark(content)
                && let Some(settled) = texts.settled.remove(&mark)
            {
                *content = settled.content;
            }
        });
        let mut text_left = texts.text_left;
        // Id and text by heading mark, met in the order the writers give
        // out ids.
        let (_, note_bodies) = Footnotes::collect(&doc);
        let mut ids = DocumentIds::new(&doc.blocks, &note_bodies);
        let mut headings: HashMap<usize, ReferenceTarget> = HashMap::new();
        write::for_each_heading(&doc.blocks, &note_bodies, &mut |part, content| {
            let id = ids.next(part, content);
            if let Some(mark) = heading_mark(content) {
                let text = reference_text(content);
                let cost = reference_cost(&text);
                headings.insert(mark, ReferenceTarget { id, text, cost });
            }
        });
        walk::for_each_block(&mut doc.blocks, &mut |block| {
            if let Block::Heading { content, .. } = block
                && heading_mark(content).is_some()
            {
                content.pop();
            }
        });
        walk::resolve_links(&mut doc.blocks, LABEL_LINK, &mut |label, content| {
            let heading = self
                .heading_labels
                .get(label)
                .and_then(|id| headings.get(id));
            if let Some(target) = heading {
                // The link and the text it shows, if it has none of its
                // own; past what references may copy, its label's text.
                let shows_text = content.is_empty();
                let cost = target.id.len() + if shows_text { target.cost } else { 0 };
                if spend_on_reference(&mut text_left, cost) {
                    if shows_text {
                        *content = target.text.clone();
                    }
                    return Some(target.id.clone());
                }
            }
            if content.is_empty() {
                *content = vec![Inline::Text(format!("@{label}"))];
            }
            None
        });
        walk::for_each_run(&mut doc.blocks, &mut |run| Quoter::default().run(run));
        doc
    }
}

/// The written headings' content, by their mark ([`HEADING_MARK`]), with
/// the bare references in it settled (see [`HeadingTexts::settle`]).
struct HeadingTexts<'a> {
    /// Heading marks by the typst `<label>` on them.
    labels: &'a HashMap<String, usize>,
    /// Headings not settled yet.
    unsettled: HashMap<usize, Vec<Inline>>,
    /// Settled headings.
    settled: HashMap<usize, SettledHeading>,
    /// What's left of what references may copy, headings' text and links
    /// to them (see [`MAX_REFERENCE_TEXT`]).
    text_left: usize,
}

impl HeadingTexts<'_> {
    /// Heading `mark`, settled first, `level` headings deep. `None` for a
    /// heading that isn't written, is being settled (a reference back to it
    /// from its own text), or is past [`MAX_HEADING_REFERENCES`] levels.
    fn settle(&mut self, mark: usize, level: usize) -> Option<&SettledHeading> {
        if self.settled.contains_key(&mark) {
            return self.settled.get(&mark);
        }
        if level >= MAX_HEADING_REFERENCES {
            return None;
        }
        let mut content = self.unsettled.remove(&mark)?;
        let mut depth = 0;
        // A bare reference shows the text of the heading it names, settled
        // in turn, unless that would show headings more than
        // `MAX_HEADING_REFERENCES` deep; else its label's name.
        walk::for_each_nested(&mut content, &mut |run| {
            for inline in run.iter_mut() {
                if let Inline::Link {
                    url,
                    content: shown,
                    ..
                } = inline
                    && shown.is_empty()
                    && let Some(label) = url.strip_prefix(LABEL_LINK)
                {
                    let text = self.labels.get(label).and_then(|&target| {
                        let settled = self.settle(target, level + 1)?;
                        let (cost, target_depth) = (settled.cost, settled.depth);
                        if level + 1 + target_depth > MAX_HEADING_REFERENCES
                            || !spend_on_reference(&mut self.text_left, cost)
                        {
                            return None;
                        }
                        Some((self.settled[&target].text.clone(), target_depth))
                    });
                    match text {
                        Some((text, target_depth)) => {
                            *shown = text;
                            depth = depth.max(target_depth + 1);
                        }
                        None => *inline = Inline::Text(format!("@{label}")),
                    }
                }
            }
        });
        let text = reference_text(&content);
        let cost = reference_cost(&text);
        let settled = SettledHeading {
            content,
            text,
            cost,
            depth,
        };
        Some(self.settled.entry(mark).or_insert(settled))
    }
}

/// Whether what's `left` of what references may copy pays for `cost`
/// (see [`MAX_REFERENCE_TEXT`]), spending it if so.
fn spend_on_reference(left: &mut usize, cost: usize) -> bool {
    match left.checked_sub(cost) {
        Some(rest) => {
            *left = rest;
            true
        }
        None => false,
    }
}

/// What a reference showing `text`, a heading's, costs of what references
/// may copy: its bytes, and [`REFERENCE_INLINE_COST`] for each inline, those
/// nested in others included.
fn reference_cost(text: &[Inline]) -> usize {
    let mut inlines = 0;
    let mut runs = vec![text];
    while let Some(run) = runs.pop() {
        inlines += run.len();
        for inline in run {
            match inline {
                Inline::Emph(c)
                | Inline::Strong(c)
                | Inline::Strikethrough(c)
                | Inline::Superscript(c)
                | Inline::Subscript(c)
                | Inline::Link { content: c, .. } => runs.push(c),
                _ => {}
            }
        }
    }
    inline_text(text).len() + inlines * REFERENCE_INLINE_COST
}

/// A written heading, as references to it show it (see
/// [`Reader::finish`]).
struct ReferenceTarget {
    /// The id the writers give it.
    id: String,
    /// Its text as a reference shows it (see [`reference_text`]), and
    /// what a reference showing it costs (see [`reference_cost`]).
    text: Vec<Inline>,
    cost: usize,
}

/// A heading's content with the references in it settled (see
/// [`HeadingTexts`]).
struct SettledHeading {
    content: Vec<Inline>,
    /// Its text as a reference shows it (see [`reference_text`]), and
    /// what a reference showing it costs (see [`reference_cost`]).
    text: Vec<Inline>,
    cost: usize,
    /// How many headings deep it shows the text of headings it references.
    depth: usize,
}

/// The text of `inlines`, their quotes settled as a paragraph of their own.
pub(super) fn quoted_text(inlines: &[Inline]) -> String {
    let mut run = inlines.to_vec();
    Quoter::default().run(&mut run);
    inline_text(&run)
}

/// What a bare reference to a heading with `content` shows: its text, as
/// [`quoted_text`] has it, but with its math still math, which as text
/// would show its TeX.
fn reference_text(content: &[Inline]) -> Vec<Inline> {
    fn push_shown(out: &mut Vec<Inline>, inlines: &[Inline]) {
        for inline in inlines {
            match inline {
                Inline::Math(tex) => out.push(Inline::Math(tex.clone())),
                Inline::Emph(c)
                | Inline::Strong(c)
                | Inline::Strikethrough(c)
                | Inline::Superscript(c)
                | Inline::Subscript(c)
                | Inline::Link { content: c, .. } => push_shown(out, c),
                other => push_text(out, &inline_text(std::slice::from_ref(other))),
            }
        }
    }
    let mut run = content.to_vec();
    Quoter::default().run(&mut run);
    let mut shown = Vec::new();
    push_shown(&mut shown, &run);
    shown
}

/// Typst's smart quotes for one paragraph (its `SmartQuoter`): a quote
/// closes the quotation opened last if it's the same kind and doesn't
/// follow a space or an opening bracket, and opens one otherwise. After a
/// digit it's a prime instead, and a single quote after a letter or an
/// object (math, an image, a box) is an apostrophe. A box's content is a
/// paragraph of its own.
#[derive(Default)]
struct Quoter {
    /// Open quotations, innermost last: `true` for double quotes.
    open: Vec<bool>,
    /// The paragraph's last character so far.
    before: Option<char>,
}

impl Quoter {
    /// Replace the quote marks ([`QUOTE_MARK`]) in `inlines`, which
    /// continue the paragraph read so far, and unwrap its boxes
    /// ([`FRAME_MARK`]).
    fn run(&mut self, inlines: &mut Vec<Inline>) {
        let mut out = Vec::with_capacity(inlines.len());
        for mut inline in std::mem::take(inlines) {
            match &mut inline {
                Inline::Link { url, .. } if url.starts_with(QUOTE_MARK) => {
                    let quote = self.quote(url.ends_with('"'));
                    self.see(quote);
                    push_text(&mut out, &quote.to_string());
                    continue;
                }
                Inline::Link { url, content, .. } if url.starts_with(FRAME_MARK) => {
                    Quoter::default().run(content);
                    self.see('\u{FFFC}');
                    for inline in std::mem::take(content) {
                        push_inline(&mut out, inline);
                    }
                    continue;
                }
                Inline::Text(text) => text.chars().for_each(|c| self.see(c)),
                Inline::Emph(c)
                | Inline::Strong(c)
                | Inline::Strikethrough(c)
                | Inline::Superscript(c)
                | Inline::Subscript(c)
                | Inline::Link { content: c, .. } => self.run(c),
                Inline::Code(code) => code.chars().for_each(|c| self.see(c)),
                Inline::Math(_) | Inline::Image { .. } => self.see('\u{FFFC}'),
                // A note's mark is its number.
                Inline::FootnoteRef(_) => self.see('1'),
                Inline::SoftBreak => self.see(' '),
                Inline::LineBreak => self.see('\n'),
                Inline::Raw { .. } => {}
            }
            push_inline(&mut out, inline);
        }
        *inlines = out;
    }

    fn see(&mut self, c: char) {
        if !typst::text::is_default_ignorable(c) {
            self.before = Some(c);
        }
    }

    fn quote(&mut self, double: bool) -> char {
        let before = self.before.unwrap_or(' ');
        let opened = self.open.last().copied();
        if before.is_numeric() && opened != Some(double) {
            return if double { '″' } else { '′' };
        }
        if !double && opened != Some(false) && (before.is_alphabetic() || before == '\u{FFFC}') {
            return '’';
        }
        if opened == Some(double) && !before.is_whitespace() && !matches!(before, '(' | '[' | '{') {
            self.open.pop();
            return if double { '”' } else { '’' };
        }
        self.open.push(double);
        if double { '“' } else { '‘' }
    }
}

#[cfg(test)]
mod tests {
    use super::super::read;
    use super::super::tests::paragraph_texts;
    use super::*;
    use crate::converter::document::native::read::markdown;

    #[test]
    fn headings_in_footnotes_keep_heading_labels_in_step() {
        let doc = read(
            "Text#footnote[= Note heading <nh>]\n\n= Real <real>\n\n\
             See @real and @nh.#footnote[Back to @real.]\n",
        );
        let md = write::markdown::write(&doc);
        assert!(
            md.contains("See [Real](#real) and [Note heading](#note-heading)."),
            "{md}"
        );
        assert!(md.contains("Back to [Real](#real)."), "{md}");
    }

    /// Headings whose slugs share a base, in the text, in a note, in a note
    /// inside a note and in a note that's lost with the title it was in.
    /// Their texts differ only in punctuation, which slugs leave out.
    const SHARED_SLUGS: &str = "#set document(title: [T#footnote[= Dup... <lost>]])\n\n\
        A#footnote[Outer #footnote[= Dup? <inner>]] B#footnote[= Dup! <third>]\n\n\
        = Dup <main>\n\n= Dup. <later>\n\n\
        Refs @main, @inner, @third, @later, @lost and more.\n";

    /// Checks that every link to a heading in `pages` (file name and
    /// HTML) shows the text of the heading it goes to, and counts them.
    fn heading_links(pages: &[(String, String)]) -> usize {
        let select = |s| scraper::Selector::parse(s).unwrap();
        let mut headings = HashMap::new();
        for (file, html) in pages {
            let html = scraper::Html::parse_document(html);
            for h in html.select(&select("h1, h2, h3, h4, h5, h6")) {
                if let Some(id) = h.value().attr("id") {
                    headings.insert(format!("{file}#{id}"), h.text().collect::<String>());
                }
            }
        }
        let mut links = 0;
        for (file, html) in pages {
            let html = scraper::Html::parse_document(html);
            for a in html.select(&select("a[href*='#']")) {
                let href = a.value().attr("href").unwrap();
                let (page, id) = href.split_once('#').unwrap();
                if id.starts_with("fn") {
                    continue;
                }
                let target = format!("{}#{id}", if page.is_empty() { file } else { page });
                let text = a.text().collect::<String>();
                assert_eq!(
                    headings.get(&target),
                    Some(&text),
                    "{href} in {file}: {html:?}"
                );
                links += 1;
            }
        }
        links
    }

    /// [`heading_links`] for LaTeX: `\section[text]…\label{id}` and
    /// `\hyperref[id]{text}`.
    fn latex_heading_links(tex: &str) -> usize {
        let until = |s: &str, end: char| s[..s.find(end).unwrap()].to_string();
        let mut labels = HashMap::new();
        for (at, _) in tex.match_indices("section[") {
            let rest = &tex[at + "section[".len()..];
            let label = &rest[rest.find("\\label{").unwrap() + "\\label{".len()..];
            labels.insert(until(label, '}'), until(rest, ']'));
        }
        let mut links = 0;
        for (at, _) in tex.match_indices("\\hyperref[") {
            let rest = &tex[at + "\\hyperref[".len()..];
            let id = until(rest, ']');
            let text = until(&rest[id.len() + 2..], '}');
            assert_eq!(labels.get(&id), Some(&text), "{id}: {tex}");
            links += 1;
        }
        links
    }

    #[test]
    fn heading_labels_reach_their_heading_in_every_output() {
        let doc = read(SHARED_SLUGS);
        let md = write::markdown::write(&doc);
        assert!(md.contains("[Dup.](#dup-1), @lost and more."), "{md}");
        let html = write::html::write(&doc);
        assert_eq!(heading_links(&[(String::new(), html)]), 4);
        // Markdown ids are the renderer's: read it back to see them.
        let md_html = write::html::write(&markdown::read(&md));
        assert_eq!(heading_links(&[(String::new(), md_html)]), 4);
        assert_eq!(latex_heading_links(&write::latex::write(&doc)), 4);

        let options = write::epub::Options {
            base_dir: std::env::temp_dir(),
        };
        let epub = write::epub::write(&doc, &options).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(epub)).unwrap();
        let chapters: Vec<(String, String)> = (0..zip.len())
            .filter_map(|i| {
                let mut file = zip.by_index(i).unwrap();
                let name = file.name().strip_prefix("OEBPS/text/")?.to_string();
                let mut html = String::new();
                std::io::Read::read_to_string(&mut file, &mut html).unwrap();
                Some((name, html))
            })
            .collect();
        assert_eq!(heading_links(&chapters), 4);
    }

    /// Checks that every link to a heading reaches it in HTML, in Markdown
    /// read back and in LaTeX, and counts them.
    fn checked_heading_links(doc: &Document) -> [usize; 3] {
        let html = write::html::write(doc);
        let md_html = write::html::write(&markdown::read(&write::markdown::write(doc)));
        [
            heading_links(&[(String::new(), html)]),
            heading_links(&[(String::new(), md_html)]),
            latex_heading_links(&write::latex::write(doc)),
        ]
    }

    #[test]
    fn references_to_notes_before_them_keep_heading_labels_in_step() {
        // The writers number notes by their first reference, here before
        // the note itself, and give headings in notes ids in that order.
        for source in [
            "See @fn-a.\n\nX#footnote[= Dup! <b>] Y#footnote[= Dup? <a2>] <fn-a>\n\n\
             Refs @b and @a2.\n",
            "See #footnote(<fn-a>).\n\nX#footnote[= Dup! <b>] Y#footnote[= Dup? <a2>] <fn-a>\n\n\
             Refs @b and @a2.\n",
            // A note in the title, written only because `@fn` names it.
            "#set document(title: [T#footnote[= Dup? <a2>] <fn>])\n\nSee @fn.\n\n\
             X#footnote[= Dup! <b>]\n\nRefs @b and @a2.\n",
        ] {
            assert_eq!(checked_heading_links(&read(source)), [2; 3], "{source}");
        }
    }

    #[test]
    fn headings_holding_references_get_ids_from_their_text() {
        let doc = read("= Intro <intro>\n\n= See @intro <see>\n\nGo to @see.\n");
        let md = write::markdown::write(&doc);
        assert!(md.contains("Go to [See Intro](#see-intro)."), "{md}");
        assert_eq!(checked_heading_links(&doc), [2; 3]);
        // A reference back to a heading being read keeps its label.
        let doc = read("= A @b <a>\n\n= B @a <b>\n\n= Self @self <self>\n\n@a, @b, @self.\n");
        let md = write::markdown::write(&doc);
        assert!(
            md.contains("[A B @a](#a-b-a), [B @a](#b-a), [Self @self](#self-self)."),
            "{md}"
        );
        assert_eq!(checked_heading_links(&doc), [4; 3]);
    }

    /// Regression for #58: a reference to a heading shows the heading's
    /// math as math, as text it would show its TeX, through a heading that
    /// refers to it too.
    #[test]
    fn references_show_a_headings_math_as_math() {
        let doc = read("= Sets $f := g$ and $alpha$ <s>\n\n= See @s <t>\n\nGo to @s and @t.\n");
        let md = write::markdown::write(&doc);
        assert!(
            md.contains(
                "Go to [Sets $f ≔ g$ and $\\alpha$](#sets-f-g-and-alpha) and \
                 [See Sets $f ≔ g$ and $\\alpha$](#see-sets-f-g-and-alpha)."
            ),
            "{md}"
        );
    }

    #[test]
    fn references_resolve_in_every_container() {
        let doc = read(
            "= Target <t>\n\n#quote(block: true)[Quote @t.]\n\n- Item @t.\n\n\
             #table(columns: 1, [Cell @t.])\n\n*Strong _@t emph_*#footnote[Note @t.]\n",
        );
        let md = write::markdown::write(&doc);
        assert_eq!(md.matches("[Target](#target)").count(), 5, "{md}");
        assert!(!md.contains(LABEL_LINK), "{md}");
    }

    #[test]
    fn references_may_come_before_the_footnote_they_name() {
        let doc = read("See @fn-a and #footnote(<fn-a>).\n\nText#footnote[Note] <fn-a>\n");
        let md = write::markdown::write(&doc);
        assert!(md.contains("See [^1] and [^1]."), "{md}");
        assert!(md.contains("Text[^1]"), "{md}");
    }

    #[test]
    fn smart_quotes_follow_typst() {
        // As typst renders it: a quote opens unless it closes the last
        // quotation opened (so the one after `c`, with none open, opens);
        // after a digit (a note's mark too) it's a prime, and after a
        // letter or an object a single quote is an apostrophe. Each
        // paragraph starts afresh.
        let doc = read(
            "#set document(title: [\"Quoted\" title])\n\
             (\"quoted\") ['single'] --\"dash\" \"wait---\" 5' 5\" don't \
             A#footnote[n]'s x $y$'s *\"bold\"* \"a *b\"* c\" \"hi (there)\"\n\n\
             \"Unclosed\n\n\"Fresh\" paragraph.\n\n\"a (\"b\") c\"\n",
        );
        assert_eq!(doc.meta.title.as_deref(), Some("“Quoted” title"));
        assert_eq!(
            paragraph_texts(&doc),
            [
                "(“quoted”) [‘single’] –“dash” “wait—” 5′ 5″ don’t A′s x y’s “bold” “a b” c“ “hi (there)”",
                "“Unclosed",
                "“Fresh” paragraph.",
                // A quotation already open doesn't close after a bracket.
                "“a (“b”) c”",
            ]
        );
    }

    #[test]
    fn boxes_quote_on_their_own_and_transforms_are_blocks() {
        // As typst renders them: a box is laid out on its own, so quotes in
        // it start afresh, and the text around it sees an object there.
        // `rotate`, `move`, `scale`, `skew` and `repeat` are blocks.
        let doc = read(
            "x#box['y'] z\n\n\"a#box[\"x\"]b\"\n\n5#box[\"x\"]\n\n\
             a#box[\"b\"]c#box['d']e\n\n\"a #box[b\"] c\"\n\n\"x #box[(]\" y\n\n\
             #box[5]'s\n\n\"a#rotate(0deg)[\"x\"]b\"\n\n\
             \"a#move(dx: 0pt)[\"x\"]b\" #scale(100%)['y'] #skew(ax: 0deg)[z] #repeat[r]\n",
        );
        assert_eq!(
            paragraph_texts(&doc),
            [
                "x‘y’ z",
                "“a“x”b”",
                "5“x”",
                "a“b”c‘d’e",
                "“a b“ c”",
                "“x (” y",
                "5’s",
                "“a",
                "“x”",
                "b“",
                "“a",
                "“x”",
                "b“",
                "‘y’",
                "z",
                "r",
            ]
        );
    }

    #[test]
    fn control_characters_stay_text() {
        let doc = read("a\u{3}b\u{4}c #\"\\u{3}\\u{4}\"\n");
        assert_eq!(paragraph_texts(&doc), ["a\u{3}b\u{4}c \u{3}\u{4}"]);
    }

    /// Every output `doc` is written to, as text: Markdown, plain text,
    /// HTML, LaTeX, typst and the EPUB's files.
    fn every_output(doc: &Document) -> Vec<String> {
        let mut outputs = vec![
            write::markdown::write(doc),
            write::plain::write(doc),
            write::html::write(doc),
            write::latex::write(doc),
        ];
        let options = write::typst::Options {
            target: write::typst::Target::TypstFile,
            fonts: Vec::new(),
            paper: "a4",
            base_dir: std::env::temp_dir(),
            tex_fallback: &std::collections::HashSet::new(),
        };
        outputs.push(write::typst::write(doc, &options).source);
        let options = write::epub::Options {
            base_dir: std::env::temp_dir(),
        };
        let epub = write::epub::write(doc, &options).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(epub)).unwrap();
        for i in 0..zip.len() {
            let mut file = zip.by_index(i).unwrap();
            let mut text = String::new();
            if std::io::Read::read_to_string(&mut file, &mut text).is_ok() {
                outputs.push(text);
            }
        }
        outputs
    }

    #[test]
    fn no_placeholder_survives_into_any_output() {
        // References in notes, quotes, tables and headings, to headings,
        // notes and nothing, and smart quotes and boxes in each of them.
        let doc = read(
            "= Intro \"q\" #box['b'] <intro>\n\
             Text#footnote[See @intro and #link(<intro>)[\"there\"] and @nowhere.] <fn>\n\n\
             = About @intro <about>\n\n\
             #quote(block: true)[@about @fn #box[\"x\"]]\n\n\
             #table(columns: 1, [@intro #link(<nowhere>)[gone] 'c' #box[d]])\n\n\
             - *\"e\"* #text(red)[#box['f'] g\n\n  h #box[\"i\"]]\n",
        );
        let md = write::markdown::write(&doc);
        assert!(
            md.contains(
                "[^1]: See [Intro “q” ‘b’](#intro-q-b) and [“there”](#intro-q-b) and @nowhere."
            ),
            "{md}"
        );
        for output in every_output(&doc) {
            for mark in [LABEL_LINK, HEADING_MARK, QUOTE_MARK, FRAME_MARK] {
                assert!(!output.contains(mark), "{mark:?} in {output:?}");
            }
        }
    }

    #[test]
    fn references_copy_a_bounded_amount_of_heading_text() {
        // A long heading referred to again and again: past what references
        // may copy, a reference shows its label.
        let long = "word ".repeat(4000);
        let source = format!(
            "#set heading(numbering: \"1.\")\n= {long}<h>\n\n{}\n",
            "@h ".repeat(5000)
        );
        let doc = read(&source);
        let [_, Block::Paragraph(body)] = doc.blocks.as_slice() else {
            panic!("{:?}", doc.blocks.len());
        };
        let shown = inline_text(body);
        assert!(shown.starts_with("word word "), "{}", &shown[..40]);
        assert!(shown.ends_with("@h"), "{}", &shown[shown.len() - 40..]);
        // The links count too, whose targets are as long as the heading.
        let targets: usize = body
            .iter()
            .map(|inline| match inline {
                Inline::Link { url, .. } => url.len(),
                _ => 0,
            })
            .sum();
        let budget = MAX_REFERENCE_TEXT.max(source.len() * REFERENCE_TEXT_PER_BYTE);
        let copied = shown.len() + targets;
        assert!(copied < budget + 5000 * "@h ".len(), "{copied}");
        // So do the inlines, which a writer may write at length (math).
        let source = format!(
            "#set heading(numbering: \"1.\")\n= {}<h>\n\n{}\n",
            "$x$ ".repeat(5000),
            "@h ".repeat(5000)
        );
        let doc = read(&source);
        let [_, Block::Paragraph(body)] = doc.blocks.as_slice() else {
            panic!("{:?}", doc.blocks.len());
        };
        let math: usize = body
            .iter()
            .map(|inline| match inline {
                Inline::Link { content, .. } => content.len(),
                _ => 0,
            })
            .sum();
        let budget = MAX_REFERENCE_TEXT.max(source.len() * REFERENCE_TEXT_PER_BYTE);
        assert!(math <= budget / REFERENCE_INLINE_COST, "{math}");
    }
}

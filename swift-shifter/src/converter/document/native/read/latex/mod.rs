//! LaTeX → [`Document`] (#73): the document's body, as mitex-parser parses
//! it. It's given no commands and expands no macros, so it only splits the
//! source's own tokens and pairs braces and environments: which arguments a
//! command takes is the reader's to say, the parser's own table being
//! math's. `.tex` input still goes to pandoc until the reader reads what
//! pandoc does (see `graph::route_hop`).

use mitex_parser::CommandSpec;
use mitex_parser::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use crate::converter::document::native::Warning;
use crate::converter::document::native::ast::{Block, Document, Inline};

/// The stack reading takes per level the parser and the reader recurse
/// into, in a debug build, which takes more than a release one: 475 bytes
/// for `{` and `\(`, 570 for `\begin`, 953 for `^`, `_` and `'`, the
/// costliest (see the test `stack_per_level_is_measured`). This is well
/// over twice that.
const STACK_PER_LEVEL: usize = 4 << 10;

/// The stack reading takes besides, a shallow document's.
const STACK_BASE: usize = 1 << 20;

/// The most stack reading may take, address space it reserves rather than
/// memory it uses. A source that could need more, with more than 250,000
/// characters other than letters, digits and spaces, is read as plain
/// text, with a warning.
const MAX_STACK: usize = 1 << 30;

/// `source`, and, where it might nest too deep to parse, a warning that it
/// was read as plain text instead.
pub fn read(source: &str) -> (Document, Option<Warning>) {
    // The parser and the reader recurse into nested groups, environments,
    // math and scripts, as deep as they go, so reading gets a stack of its
    // own, as deep as `source` could need.
    let stack = STACK_BASE.saturating_add(nesting_bound(source).saturating_mul(STACK_PER_LEVEL));
    if stack <= MAX_STACK {
        let owned = source.to_string();
        let reading = std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || read_parsed(&owned));
        if let Ok(reading) = reading {
            match reading.join() {
                Ok(doc) => return (doc, None),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    }
    let warning = Warning {
        one: "document nested too deeply to read was read as plain text",
        many: "documents nested too deeply to read were read as plain text",
        count: 1,
        line: None,
    };
    (super::plain::read(source), Some(warning))
}

/// How deep the parser and the reader might recurse into `source` at most:
/// its characters other than letters, digits and spaces. Each level they
/// nest, whatever opens it (`{`, `\begin`, `\(`, `^`, `'`), takes at least
/// one of them, so however the parser splits `source`, it can't nest
/// deeper.
fn nesting_bound(source: &str) -> usize {
    source
        .chars()
        .filter(|c| !c.is_alphanumeric() && !c.is_whitespace())
        .count()
}

fn read_parsed(source: &str) -> Document {
    // Expanding macros, which may never end, would also parse more than
    // `source` holds, past `nesting_bound`.
    let root = mitex_parser::parse_without_macro(source, CommandSpec::new(Default::default()));
    let mut text = String::new();
    words(&document_body(&root).unwrap_or(root), &mut text);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Document {
        blocks: if text.is_empty() {
            Vec::new()
        } else {
            vec![Block::Paragraph(vec![Inline::Text(text)])]
        },
        ..Document::default()
    }
}

/// The `document` environment, past the preamble; a fragment has none.
fn document_body(root: &SyntaxNode) -> Option<SyntaxNode> {
    root.descendants().find(|node| {
        node.kind() == SyntaxKind::ItemEnv
            && node
                .first_child()
                .and_then(|begin| begin.first_token())
                .is_some_and(|name| name.text() == "document")
    })
}

/// The text of `node` as it's written, without commands, environments'
/// names, comments or braces, into `out`: the reader's first step, read
/// into paragraphs from #73's next part on.
fn words(node: &SyntaxNode, out: &mut String) {
    for item in node.children_with_tokens() {
        match item {
            SyntaxElement::Node(node) => match node.kind() {
                SyntaxKind::ItemCmd
                | SyntaxKind::ItemBegin
                | SyntaxKind::ItemEnd
                | SyntaxKind::ItemBlockComment
                | SyntaxKind::ItemTypstCode => out.push(' '),
                _ => words(&node, out),
            },
            SyntaxElement::Token(token) => match token.kind() {
                SyntaxKind::TokenComment | SyntaxKind::TokenLBrace | SyntaxKind::TokenRBrace => {}
                SyntaxKind::TokenWhiteSpace | SyntaxKind::TokenLineBreak => out.push(' '),
                _ => out.push_str(token.text()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_is_read_for_its_words() {
        let source = "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\
                      Some \\emph{text} % a comment\n\nhere.\n\\end{document}\n";
        let (doc, warning) = read(source);
        assert_eq!(
            doc.blocks,
            [Block::Paragraph(vec![Inline::Text(
                "Some text here.".into()
            )])]
        );
        assert!(warning.is_none());
    }

    /// Macros aren't expanded: one that expands forever doesn't hang it.
    #[test]
    fn a_macro_that_never_ends_is_read_as_written() {
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (doc, _) = read("\\newcommand{\\again}{\\again}\\again done");
            sent.send(doc).unwrap();
        });
        let doc = received
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("reading never ended");
        assert!(format!("{:?}", doc.blocks).contains("done"));
    }

    /// Punctuation counts, Unicode's too; letters, digits and spaces don't.
    #[test]
    fn the_nesting_bound_counts_what_is_no_word() {
        assert_eq!(nesting_bound("Ab 12 中文 é"), 0);
        // `{ } ^ ' _ \ ( … \ ) ，`
        assert_eq!(nesting_bound("{x}^'_\\(…\\)，"), 11);
    }

    #[test]
    fn deep_nesting_is_read_within_its_stack() {
        // A stack overflow aborts the process, so each runs in a child: as
        // `read` sizes its stack, and with half that, which keeps
        // `STACK_PER_LEVEL` twice what it takes. This deep, each needs more
        // than the 2 MiB conversions run on.
        const DEPTH: usize = 5000;
        let half = (STACK_BASE + DEPTH * STACK_PER_LEVEL) / 2;
        for construct in CONSTRUCTS {
            assert!(in_child(construct, DEPTH, None), "{construct}");
            assert!(in_child(construct, DEPTH, Some(half)), "{construct}");
        }
        // Past the most stack reading may take: plain text, with a warning.
        let source = format!("{}x", "{".repeat(MAX_STACK / STACK_PER_LEVEL));
        let (doc, warning) = read(&source);
        assert!(format!("{:?}", doc.blocks).contains('x'));
        assert_eq!(
            warning.unwrap().message(),
            "1 document nested too deeply to read was read as plain text"
        );
    }

    /// What the parser and the reader recurse into: groups, environments,
    /// `\(` math, and runs of scripts and primes.
    const CONSTRUCTS: [&str; 6] = ["{", "\\begin", "\\(", "^", "_", "'"];

    /// `construct` nested `depth` deep around an `x`.
    fn nested(construct: &str, depth: usize) -> String {
        let (open, close) = match construct {
            "{" => ("{", "}"),
            "\\begin" => ("\\begin{q}", "\\end{q}"),
            "\\(" => ("\\(", "\\)"),
            script => return format!("x{}", script.repeat(depth)),
        };
        format!("{}x{}", open.repeat(depth), close.repeat(depth))
    }

    /// Whether reading `construct` nested `depth` deep, on a thread with
    /// `stack` or as [`read`] sizes it, succeeds: in a child process, this
    /// test binary running [`nesting_child`] alone.
    fn in_child(construct: &str, depth: usize, stack: Option<usize>) -> bool {
        let stack = stack.map_or("read".to_string(), |stack| stack.to_string());
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "converter::document::native::read::latex::tests::nesting_child",
                "--exact",
                "-q",
            ])
            .env(CHILD, format!("{construct} {depth} {stack}"))
            .output()
            .unwrap()
            .status
            .success()
    }

    const CHILD: &str = "SWIFT_SHIFTER_LATEX_NESTING_CHILD";

    /// In a child of [`in_child`], reads what it's told to; otherwise
    /// nothing.
    #[test]
    fn nesting_child() {
        let Ok(task) = std::env::var(CHILD) else {
            return;
        };
        let [construct, depth, stack] = task.split(' ').collect::<Vec<_>>()[..] else {
            panic!("{task}");
        };
        let source = nested(construct, depth.parse().unwrap());
        let doc = match stack.parse::<usize>() {
            Ok(stack) => std::thread::Builder::new()
                .stack_size(stack)
                .spawn(move || read_parsed(&source))
                .unwrap()
                .join()
                .unwrap(),
            Err(_) => read(&source).0,
        };
        assert!(format!("{:?}", doc.blocks).contains('x'));
    }

    /// How [`STACK_PER_LEVEL`] is measured, in a debug build: for each
    /// construct, the least stack that reads it 5000 deep, found by halving
    /// in child processes, less what a shallow read takes, per level. The
    /// costliest, doubled, must fit. It takes a minute and a half, so it's
    /// run by hand, after mitex-parser or the reader's recursion changes:
    /// `cargo test stack_per_level -- --ignored --nocapture`.
    #[test]
    #[ignore = "slow: re-measures by hand"]
    fn stack_per_level_is_measured() {
        const DEPTH: usize = 5000;
        let least = |construct: &str, depth: usize| {
            let (mut fails, mut works) = (16 << 10, 1 << 30);
            while works - fails > 4 << 10 {
                let mid = (fails + works) / 2;
                if in_child(construct, depth, Some(mid)) {
                    works = mid;
                } else {
                    fails = mid;
                }
            }
            works
        };
        for construct in CONSTRUCTS {
            let per_level = (least(construct, DEPTH) - least(construct, 1)) / DEPTH;
            println!("{construct}: {per_level} bytes a level");
            assert!(2 * per_level <= STACK_PER_LEVEL, "{construct}: {per_level}");
        }
    }
}

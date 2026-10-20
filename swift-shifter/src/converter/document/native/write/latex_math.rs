//! Math TeX for LaTeX. The AST keeps math characters as they were typed
//! (`α`, `→`, `ℝ`), as every other consumer of its TeX takes them, but
//! LaTeX's own math fonts have no glyphs for them and pdfLaTeX rejects
//! them. [`math`] writes them under the names amsmath and amssymb give
//! them, and [`Fallbacks`] defines the few names those packages lack.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::Range;

use crate::converter::document::native::ast::{is_variation_selector, push_tex};

/// `tex` with its Unicode math characters under their LaTeX names (`α` is
/// `\alpha`), and the ones LaTeX has no name for in `\text`. Text arguments
/// like `\text{…}` are left as they are but for the math inside them, and
/// `\verb`'s entirely. The commands of [`FALLBACKS`] that the math uses are
/// noted in `fallbacks`, and the math's own definitions of them hold (see
/// [`Formula::definition`]).
pub(super) fn math(tex: &str, fallbacks: &mut Fallbacks) -> String {
    let chars: Vec<char> = tex.chars().collect();
    let mut out = String::with_capacity(tex.len());
    let mut formula = Formula {
        fallbacks,
        defined: Vec::new(),
        nesting: 0,
        global: false,
    };
    formula.math(&chars, &mut out);
    out
}

/// The LaTeX name of math character `c` (`≔` is `\coloneqq`), as [`math`]
/// writes it, noted in `fallbacks` like the math's.
pub(super) fn symbol(c: char, fallbacks: &mut Fallbacks) -> Option<String> {
    let name = name(c)?;
    if let Some(word) = name.strip_prefix('\\') {
        fallbacks.saw(word);
    }
    Some(name)
}

/// `math`, as [`math`] writes it, for a moving argument: a heading's short
/// form, which LaTeX writes to the table of contents and reads back. A
/// definition there (`\renewcommand{\coloneqq}{…}`) would be expanded on
/// the way, its name included, so in math defining a command each control
/// word is protected and written out as it is.
pub(super) fn moving(math: &str) -> Cow<'_, str> {
    let defines = math.split('\\').skip(1).any(|rest| {
        let end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let word = &rest[..end];
        word == "global"
            || DEFINITIONS.contains(&word)
            || NEW_DEFINITIONS.iter().any(|&(new, _)| new == word)
    });
    if !defines {
        return Cow::Borrowed(math);
    }
    let mut out = String::with_capacity(2 * math.len());
    let mut chars = math.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next_if(char::is_ascii_alphabetic) {
            Some(first) => {
                out.push_str("\\protect\\");
                out.push(first);
            }
            // A control symbol (`\\`, `\{`), whatever it's made of.
            None => {
                out.push(c);
                out.extend(chars.next());
            }
        }
    }
    Cow::Owned(out)
}

/// Whether `math`, as [`math`] writes it, ends in a comment, which takes
/// the rest of its line: whatever follows the math there (a `\)`) would be
/// part of it.
pub(super) fn ends_in_comment(math: &str) -> bool {
    let chars: Vec<char> = math.chars().collect();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let rest = &chars[i..];
        i += match c {
            '%' if !rest.contains(&'\n') => return true,
            '%' => comment_len(rest),
            // A control sequence, whose name may be `%` (`\%`), and
            // `\verb`'s argument, which may hold one.
            '\\' => {
                let name = name_len(&rest[1..]);
                let verbatim = match rest[1..1 + name] {
                    ['v', 'e', 'r', 'b'] => verbatim_argument(&rest[1 + name..]),
                    _ => 0,
                };
                1 + name + verbatim
            }
            _ => 1,
        };
    }
    false
}

/// How deep text arguments and options nest (`\text{$\text{…}$}`,
/// `\sqrt[{\sqrt[…]{x}}]{y}`) before the rest is copied as it is: as deep
/// as TeX nests groups, past which it can't typeset the math anyway, and
/// shallow enough for the 2 MiB stack conversions run on.
const MAX_NESTING: usize = 255;

/// A formula that [`math`] writes, and what it has seen of it.
struct Formula<'a> {
    fallbacks: &'a mut Fallbacks,
    /// The commands of [`FALLBACKS`] that the formula has defined so far,
    /// as the math being written sees them: its own definitions and, for
    /// the math in a text argument, those of the math around it, but not
    /// those made in a group that has ended (in braces, or math in a text
    /// argument), which TeX ends with it. Those made for the rest of the
    /// document (`\gdef`) are in [`Fallbacks`] as well.
    defined: Vec<&'static str>,
    /// How many text arguments and options the math being written is in
    /// (see [`Formula::nested`]).
    nesting: usize,
    /// Whether the last command was `\global`, which makes the definition
    /// after it hold past the formula.
    global: bool,
}

/// A group in braces that [`Formula::math`] is in.
struct Group {
    /// How many more arguments the command before took when it opened,
    /// the group being the first of them (see [`arguments`]).
    arguments: usize,
    /// How many of [`Formula::defined`] there were then. The others are
    /// made in the group, and end with it.
    defined: usize,
}

/// A control sequence that [`Formula::control_sequence`] copied.
struct ControlSequence {
    /// How many characters after its `\` it took: its name, and the
    /// argument, star or option copied with it.
    len: usize,
    /// How many arguments after those the command takes (see
    /// [`arguments`]).
    arguments: usize,
}

impl Formula<'_> {
    /// Writes math `chars` to `out` (see [`math`]).
    fn math(&mut self, chars: &[char], out: &mut String) {
        // After a `^` or `_`, whose argument is the next token, so a name
        // of more than one (`\mathbb{R}`) needs braces.
        let mut script = false;
        // How many more arguments the command before takes (see
        // [`arguments`]).
        let mut arguments = 0;
        // The groups in braces still open.
        let mut groups: Vec<Group> = Vec::new();
        let mut i = 0;
        while let Some(&c) = chars.get(i) {
            let rest = &chars[i..];
            // What `c` is written as where it's a symbol, with the
            // characters after it that join it in one script or `\text`,
            // and how many characters of `rest` that is. Everything else
            // is written here.
            let (symbol, len) = match c {
                '\\' => {
                    let command = self.control_sequence(&rest[1..], out);
                    script = false;
                    // One that takes none is an argument itself.
                    arguments = if command.arguments > 0 {
                        command.arguments
                    } else {
                        arguments.saturating_sub(1)
                    };
                    (None, 1 + command.len)
                }
                '%' => {
                    let len = comment_len(rest);
                    out.extend(&rest[..len]);
                    (None, len)
                }
                '^' | '_' => {
                    out.push(c);
                    script = true;
                    (None, 1)
                }
                '{' => {
                    groups.push(Group {
                        arguments,
                        defined: self.defined.len(),
                    });
                    arguments = 0;
                    script = false;
                    out.push(c);
                    (None, 1)
                }
                // The group was an argument of what is before it, and the
                // definitions made in it end with it.
                '}' => {
                    if let Some(group) = groups.pop() {
                        self.defined.truncate(group.defined);
                        arguments = group.arguments.saturating_sub(1);
                    } else {
                        arguments = 0;
                    }
                    script = false;
                    out.push(c);
                    (None, 1)
                }
                c if c.is_ascii_whitespace() => {
                    out.push(c);
                    (None, 1)
                }
                // It picks a glyph for the character before it, of which
                // LaTeX's math fonts have only one. As a script, it's an
                // empty one.
                c if is_variation_selector(c) => (script.then(|| "{}".to_string()), 1),
                c if c.is_ascii() => (Some(c.to_string()), 1),
                _ => {
                    let (tex, len) = script_run(rest)
                        .or_else(|| variant(rest).map(|name| (name.to_string(), 2)))
                        .or_else(|| name(c).map(|name| (name, 1)))
                        .unwrap_or_else(|| text_run(rest));
                    (Some(tex), len)
                }
            };
            i += len;
            let Some(written) = symbol else {
                continue;
            };
            if let Some(word) = written.strip_prefix('\\') {
                self.fallbacks.saw(word);
            }
            // A script on an empty base (`{}^{\circ}`) can't take another
            // of its kind, so a script after it needs a base of its own.
            let based = on_empty_base(&written) && script_follows(&chars[i..]);
            // A script, or a command's argument, is one token, which a name
            // can be several of. A script's gets braces, which change
            // nothing there, and an argument's when they leave it as it is
            // (see [`is_ordinary`]).
            let braced = (script && written.chars().count() > 1)
                || (arguments > 0 && is_ordinary(&written))
                || based;
            if braced {
                out.push_str(&format!("{{{written}}}"));
            } else {
                push_tex(out, &written);
            }
            script = false;
            arguments = arguments.saturating_sub(1);
        }
    }

    /// Copies the control sequence after a `\` at the start of `rest` to
    /// `out` (the `\` included), with what TeX takes with it that isn't one
    /// of its math arguments: the argument of a text command like `\text`
    /// (see [`text_command`]) or of `\verb`, and the star and option of a
    /// command that takes math arguments after them (see
    /// [`Formula::star_and_option`]).
    fn control_sequence(&mut self, rest: &[char], out: &mut String) -> ControlSequence {
        let name = name_len(rest);
        let word: String = rest[..name].iter().collect();
        let after = &rest[name..];
        let global = std::mem::take(&mut self.global);
        let word = self.definition(word, after, global);
        out.push('\\');
        out.push_str(&word);
        self.fallbacks.saw(&word);
        let copied = match word.as_str() {
            "global" => {
                self.global = true;
                0
            }
            // Prefixes, which leave a `\global` before them to the
            // definition after them.
            "long" | "protected" | "outer" => {
                self.global = global;
                0
            }
            "verb" => {
                let argument = verbatim_argument(after);
                out.extend(&after[..argument]);
                argument
            }
            word => match text_command(word) {
                Some(argument) => self.text_argument(after, argument, out),
                None => {
                    let arguments = arguments(word);
                    let copied = if arguments > 0 {
                        self.star_and_option(word, after, out)
                    } else {
                        0
                    };
                    return ControlSequence {
                        len: name + copied,
                        arguments,
                    };
                }
            },
        };
        ControlSequence {
            len: name + copied,
            arguments: 0,
        }
    }

    /// Copies the star and the option in brackets that command `word` may
    /// have at the start of `rest`, before its arguments (`\operatorname*`,
    /// `\sqrt[3]`), the option's math as [`math`] writes it, and returns
    /// how many characters of `rest` they took. Neither is one of its
    /// arguments: a name after them is the first.
    fn star_and_option(&mut self, word: &str, rest: &[char], out: &mut String) -> usize {
        // TeX looks for each past spaces and comments.
        let mut star = 0;
        let mut next = skipped(rest);
        if STARRED.contains(&word) && rest.get(next) == Some(&'*') {
            star = next + 1;
            next = star + skipped(&rest[star..]);
        }
        let option = (OPTIONS.contains(&word) && rest.get(next) == Some(&'['))
            .then(|| option_len(&rest[next + 1..]))
            .flatten();
        let Some(len) = option else {
            out.extend(&rest[..star]);
            return star;
        };
        out.extend(&rest[..=next]);
        self.nested(&rest[next + 1..next + 1 + len], out, Self::math);
        out.push(']');
        next + len + 2
    }

    /// Command `word`, which `after` follows, as it's written. The math's
    /// own definitions of [`FALLBACKS`] are noted, and one that defines a
    /// command only where none exists (`\newcommand`, `\providecommand`)
    /// is written as one that redefines the preamble's (`\renewcommand`),
    /// unless the formula, or a formula before it for the rest of the
    /// document, has defined it already. LaTeX would stop at the
    /// preamble's definition or keep it, where the math meant its own.
    fn definition(&mut self, word: String, after: &[char], global: bool) -> String {
        let renew = NEW_DEFINITIONS
            .iter()
            .find_map(|&(new, renew)| (new == word).then_some(renew));
        if renew.is_none() && !DEFINITIONS.contains(&word.as_str()) {
            return word;
        }
        let Some(name) = fallback(&defined_command(after)) else {
            return word;
        };
        let defined = self.defined.contains(&name) || self.fallbacks.global.contains(name);
        self.defined.push(name);
        if global || GLOBAL_DEFINITIONS.contains(&word.as_str()) {
            self.fallbacks.global.insert(name);
        }
        match renew {
            Some(renew) if !defined => renew.to_string(),
            _ => word,
        }
    }

    /// Copies `argument`, the text argument of a command, at the start of
    /// `rest` to `out`, with what comes before it (see [`argument_span`]),
    /// and returns how many characters that took. One character beyond
    /// ASCII, which TeX takes for the argument without braces (`\text é`),
    /// gets them: pdfLaTeX reads it as several, and `\tag` takes nothing
    /// without them.
    fn text_argument(&mut self, rest: &[char], argument: TextArgument, out: &mut String) -> usize {
        let span = argument_span(rest, argument);
        let before = &rest[..span.start];
        let text = &rest[span.clone()];
        match *text {
            [c] if !c.is_ascii() => {
                let spaces = before
                    .iter()
                    .rev()
                    .take_while(|&&c| c == ' ' || c == '\t')
                    .count();
                out.extend(&before[..before.len() - spaces]);
                out.push_str(&format!("{{{c}}}"));
            }
            _ => {
                out.extend(before);
                self.nested(text, out, Self::text);
            }
        }
        span.end
    }

    /// Writes `chars`, which are in a text argument or an option, with
    /// `write`, or copies them as they are past [`MAX_NESTING`] of those
    /// deep.
    fn nested(
        &mut self,
        chars: &[char],
        out: &mut String,
        write: fn(&mut Self, &[char], &mut String),
    ) {
        if self.nesting == MAX_NESTING {
            out.extend(chars);
            return;
        }
        self.nesting += 1;
        write(self, chars, out);
        self.nesting -= 1;
    }

    /// Copies text argument `text` to `out`, with the math inside it
    /// (`$…$` or `\(…\)`) as [`math`] writes it. Its control sequences are
    /// copied as the math's are, so the commands of [`FALLBACKS`] that it
    /// uses are noted too.
    fn text(&mut self, text: &[char], out: &mut String) {
        // How many of [`Formula::defined`] there were when each group in
        // braces still open opened.
        let mut groups = Vec::new();
        let mut i = 0;
        while let Some(&c) = text.get(i) {
            let rest = &text[i..];
            // The math's opening `$` or `\(`, and whether it's a `$`.
            let (open, dollar) = match (c, rest.get(1)) {
                ('$', _) => (1, true),
                ('\\', Some('(')) => (2, false),
                ('\\', _) => {
                    i += 1 + self.control_sequence(&rest[1..], out).len;
                    continue;
                }
                ('%', _) => {
                    let len = comment_len(rest);
                    out.extend(&rest[..len]);
                    i += len;
                    continue;
                }
                ('{', _) => {
                    groups.push(self.defined.len());
                    out.push(c);
                    i += 1;
                    continue;
                }
                // The definitions made in the group end with it.
                ('}', _) => {
                    if let Some(defined) = groups.pop() {
                        self.defined.truncate(defined);
                    }
                    out.push(c);
                    i += 1;
                    continue;
                }
                _ => {
                    out.push(c);
                    i += 1;
                    continue;
                }
            };
            let Some(len) = math_len(&rest[open..], dollar) else {
                // Math that nothing closes.
                out.extend(rest);
                break;
            };
            // It closes as it opens: `$`, or `\)` for `\(`.
            let end = open + len + open;
            out.extend(&rest[..open]);
            // TeX math is a group, which its definitions end with.
            let defined = self.defined.len();
            self.math(&rest[open..open + len], out);
            self.defined.truncate(defined);
            out.extend(&rest[open + len..end]);
            i += end;
        }
    }
}

/// Whether `tex` is a script on an empty base, like `{}^{\circ}` or `{}'`.
fn on_empty_base(tex: &str) -> bool {
    tex.len() > 2 && tex.starts_with("{}")
}

/// Whether `rest` starts with a script (`^`, `_` or a prime), past spaces
/// and variation selectors.
fn script_follows(rest: &[char]) -> bool {
    rest.iter()
        .find(|&&c| !c.is_ascii_whitespace() && !is_variation_selector(c))
        .is_some_and(|c| matches!(c, '^' | '_' | '\''))
}

/// Whether `tex`, a name that [`math`] writes, is one ordinary symbol of
/// several tokens (`\mathbb{R}`, `\text{é}`, `{}^{\circ}`): the names with
/// braces. Braces around one leave it as it is, but around a relation like
/// `\not\equiv` they'd make it ordinary, and so space it differently from
/// the symbols beside it.
fn is_ordinary(tex: &str) -> bool {
    tex.contains('{')
}

/// How many characters the name of the control sequence at the start of
/// `rest`, after its `\`, takes: the letters of a control word (`alpha`),
/// or the one character of a control symbol (`{`).
fn name_len(rest: &[char]) -> usize {
    match rest.iter().take_while(|c| c.is_ascii_alphabetic()).count() {
        0 => rest.len().min(1),
        len => len,
    }
}

/// How many arguments command `word` takes, for the commands that take
/// some and that [`math`] may write a name after: accents, alphabets,
/// boxes and fractions. `0` for any other.
fn arguments(word: &str) -> usize {
    if TWO_ARGUMENTS.contains(&word) {
        2
    } else {
        usize::from(ONE_ARGUMENT.contains(&word))
    }
}

/// Commands that take one argument (see [`arguments`]).
const ONE_ARGUMENT: &[&str] = &[
    // Accents.
    "acute",
    "bar",
    "breve",
    "check",
    "dot",
    "ddot",
    "dddot",
    "ddddot",
    "grave",
    "hat",
    "mathring",
    "tilde",
    "vec",
    "widehat",
    "widetilde",
    "widecheck",
    "utilde",
    "overline",
    "underline",
    "overbrace",
    "underbrace",
    "overrightarrow",
    "overleftarrow",
    "overleftrightarrow",
    "underrightarrow",
    "underleftarrow",
    "underleftrightarrow",
    // Alphabets and classes.
    "mathrm",
    "mathit",
    "mathbf",
    "mathsf",
    "mathtt",
    "mathcal",
    "mathscr",
    "mathbb",
    "mathfrak",
    "mathnormal",
    "boldsymbol",
    "bm",
    "pmb",
    "operatorname",
    "mathop",
    "mathbin",
    "mathrel",
    "mathord",
    "mathopen",
    "mathclose",
    "mathpunct",
    "mathinner",
    // Boxes.
    "sqrt",
    "boxed",
    "phantom",
    "hphantom",
    "vphantom",
    "smash",
    "cancel",
    "bcancel",
    "xcancel",
    "substack",
    "xrightarrow",
    "xleftarrow",
    "pmod",
    "pod",
];

/// Commands that take two arguments (see [`arguments`]).
const TWO_ARGUMENTS: &[&str] = &[
    "frac", "dfrac", "tfrac", "cfrac", "binom", "dbinom", "tbinom", "overset", "underset",
    "stackrel", "sideset", "cancelto",
];

/// The commands of [`ONE_ARGUMENT`] and [`TWO_ARGUMENTS`] that take an
/// option in brackets before their arguments (`\sqrt[3]{x}`).
const OPTIONS: &[&str] = &["sqrt", "smash", "xrightarrow", "xleftarrow", "cfrac"];

/// Commands with a star form (`\tag*{…}`, `\operatorname*{…}`). Another
/// takes a `*` for its argument.
const STARRED: &[&str] = &["tag", "ref", "operatorname"];

/// Where a command's text argument is, past what may come before it (see
/// [`text_command`]).
#[derive(Clone, Copy)]
struct TextArgument {
    /// Whether a `*` before it is the command's star form (see
    /// [`STARRED`]).
    star: bool,
    /// How many arguments in braces come before it: settings, such as a
    /// box's color (`\colorbox{red}{…}`), kept as they are.
    settings: usize,
    /// Whether options in brackets may come before it, kept as they are
    /// too (`\makebox[2em]{…}`).
    options: bool,
}

/// Where the argument of command `word` that is text, not math, is, for
/// the commands that have one: text commands (`\text{…}`), references,
/// whose argument is a key (`\label{…}`), links, whose first is a URL, and
/// boxes, whose text comes after their settings (`\colorbox{red}{…}`).
fn text_command(word: &str) -> Option<TextArgument> {
    let first = TextArgument {
        star: STARRED.contains(&word),
        settings: 0,
        options: false,
    };
    Some(match word {
        "text" | "textrm" | "textit" | "textbf" | "textsf" | "texttt" | "textup" | "textmd"
        | "textnormal" | "textsl" | "textsc" | "emph" | "textsuperscript" | "textsubscript"
        | "mbox" | "hbox" | "fbox" | "llap" | "rlap" | "clap" | "intertext" | "shortintertext"
        | "tag" | "label" | "ref" | "eqref" | "pageref" | "url" | "href" => first,
        "makebox" | "framebox" => TextArgument {
            options: true,
            ..first
        },
        "colorbox" | "raisebox" | "parbox" => TextArgument {
            settings: 1,
            options: true,
            ..first
        },
        "fcolorbox" => TextArgument {
            settings: 2,
            options: true,
            ..first
        },
        _ => return None,
    })
}

/// Where `argument`, a command's text argument, is at the start of `rest`:
/// after the spaces and comments that TeX skips, the command's star and
/// its settings (see [`TextArgument`]). It's a group in braces, or else
/// the one character or control sequence that TeX takes for it (`\text
/// é`), or nothing at the end of the math or its group.
fn argument_span(rest: &[char], argument: TextArgument) -> Range<usize> {
    let mut start = skipped(rest);
    if argument.star && rest.get(start) == Some(&'*') {
        start += 1 + skipped(&rest[start + 1..]);
    }
    let mut settings = argument.settings;
    loop {
        while argument.options && rest.get(start) == Some(&'[') {
            let Some(len) = option_len(&rest[start + 1..]) else {
                break;
            };
            start += len + 2;
            start += skipped(&rest[start..]);
        }
        let len = argument_len(&rest[start..]);
        if settings == 0 || len == 0 {
            return start..start + len;
        }
        settings -= 1;
        start += len;
        start += skipped(&rest[start..]);
    }
}

/// How long the argument at the start of `rest` is: a group in braces, or
/// else the one character or control sequence that TeX takes for it, or
/// nothing at the end of the math or its group.
fn argument_len(rest: &[char]) -> usize {
    match rest.first() {
        None | Some('}') => 0,
        Some('\\') => 1 + name_len(&rest[1..]),
        Some('{') => group_len(rest),
        Some(_) => 1,
    }
}

/// How long the option in brackets whose `[` is before `rest` is, up to
/// the `]` that closes it outside braces: `None` when none does.
fn option_len(rest: &[char]) -> Option<usize> {
    let mut depth = 0_usize;
    let mut i = 0;
    while let Some(&c) = rest.get(i) {
        match c {
            ']' if depth == 0 => return Some(i),
            // An escaped character, which neither closes the option nor
            // opens or closes a group.
            '\\' => i += 1,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    None
}

/// How long the group in braces at the start of `rest` is, up to the
/// brace that closes it, or to the end without one.
fn group_len(rest: &[char]) -> usize {
    let mut depth = 0;
    let mut i = 0;
    while let Some(&c) = rest.get(i) {
        i += 1;
        match c {
            // An escaped brace doesn't open or close the group.
            '\\' => i += 1,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    rest.len()
}

/// How many characters at the start of `rest` TeX skips before an
/// argument: spaces, but not Unicode's other ones, which it takes for the
/// argument, and comments with the line break that ends them.
fn skipped(rest: &[char]) -> usize {
    let mut i = 0;
    loop {
        match rest.get(i) {
            Some(' ' | '\t' | '\n' | '\r') => i += 1,
            Some('%') => i += comment_len(&rest[i..]),
            _ => return i,
        }
    }
}

/// How long the comment at the start of `rest` is: its `%` and the rest
/// of its line, with the line break.
fn comment_len(rest: &[char]) -> usize {
    rest.iter()
        .position(|&c| c == '\n')
        .map_or(rest.len(), |end| end + 1)
}

/// How long `\verb`'s argument at the start of `rest` is: after any spaces
/// and a `*`, a delimiter and the text up to the next one (or to the end,
/// without one).
fn verbatim_argument(rest: &[char]) -> usize {
    let mut i = rest.iter().take_while(|c| c.is_whitespace()).count();
    if rest.get(i) == Some(&'*') {
        i += 1;
    }
    let Some(&delimiter) = rest.get(i) else {
        return i;
    };
    rest[i + 1..]
        .iter()
        .position(|&c| c == delimiter)
        .map_or(rest.len(), |len| i + len + 2)
}

/// How long the math at the start of `rest` is, up to the `$` (`dollar`)
/// or `\)` that closes it outside any group: `None` when none does.
fn math_len(rest: &[char], dollar: bool) -> Option<usize> {
    let mut depth = 0_usize;
    let mut i = 0;
    while let Some(&c) = rest.get(i) {
        match c {
            '$' if dollar && depth == 0 => return Some(i),
            '\\' if !dollar && depth == 0 && rest.get(i + 1) == Some(&')') => return Some(i),
            // An escaped character, which neither closes the math nor opens
            // or closes a group.
            '\\' => i += 1,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }
    None
}

/// A run of superscript or subscript characters at the start of `chars`
/// (`²³`) as one script on an empty base (`{}^{23}`), which can follow
/// another script, and how many characters it took.
fn script_run(chars: &[char]) -> Option<(String, usize)> {
    let run = |position: char, plain: fn(char) -> Option<char>| {
        let text: String = chars.iter().map_while(|&c| plain(c)).collect();
        let len = text.chars().count();
        (len > 0).then(|| (format!("{{}}{position}{{{text}}}"), len))
    };
    run('^', superscript).or_else(|| run('_', subscript))
}

/// A run of characters at the start of `chars` that LaTeX has no math name
/// for (`中`, `é`, `⟦`), in `\text`, and how many characters it took.
/// pdfLaTeX's `inputenc` only takes them in text, and XeLaTeX and LuaLaTeX
/// set them from the text font either way.
fn text_run(chars: &[char]) -> (String, usize) {
    let text: String = chars
        .iter()
        .take_while(|&&c| !c.is_ascii() && script_run(&[c]).is_none() && name(c).is_none())
        .collect();
    let len = text.chars().count().max(1);
    (format!("\\text{{{text}}}"), len)
}

/// What a superscript character (`²`) raises.
fn superscript(c: char) -> Option<char> {
    Some(match c {
        '⁰' => '0',
        '¹' => '1',
        '²' => '2',
        '³' => '3',
        '⁴'..='⁹' => char::from_u32(c as u32 - '⁴' as u32 + '4' as u32)?,
        '⁺' => '+',
        '⁻' => '-',
        '⁼' => '=',
        '⁽' => '(',
        '⁾' => ')',
        'ⁱ' => 'i',
        'ⁿ' => 'n',
        _ => return None,
    })
}

/// What a subscript character (`₂`) lowers.
fn subscript(c: char) -> Option<char> {
    Some(match c {
        '₀'..='₉' => char::from_u32(c as u32 - '₀' as u32 + '0' as u32)?,
        '₊' => '+',
        '₋' => '-',
        '₌' => '=',
        '₍' => '(',
        '₎' => ')',
        'ₐ' => 'a',
        'ₑ' => 'e',
        'ₒ' => 'o',
        'ₓ' => 'x',
        'ₕ' => 'h',
        'ᵢ' => 'i',
        'ⱼ' => 'j',
        'ₖ' => 'k',
        'ₗ' => 'l',
        'ₘ' => 'm',
        'ₙ' => 'n',
        'ₚ' => 'p',
        'ₛ' => 's',
        'ₜ' => 't',
        _ => return None,
    })
}

/// The commands of [`FALLBACKS`] that a document's math uses.
#[derive(Default)]
pub(super) struct Fallbacks {
    used: HashSet<&'static str>,
    /// Those that a formula has defined for the rest of the document
    /// (`\gdef`), as later formulas find them defined.
    global: HashSet<&'static str>,
}

impl Fallbacks {
    /// Notes control word `word`.
    fn saw(&mut self, word: &str) {
        self.used.extend(fallback(word));
    }

    /// The preamble's definitions of the commands the math uses, for those
    /// no package has defined.
    pub fn preamble(&self) -> String {
        let mut lines = String::new();
        for (name, definition) in FALLBACKS {
            if !self.used.contains(name) {
                continue;
            }
            let line = match definition {
                Definition::Macro(body) => {
                    format!("\\ifdefined\\{name}\\else\\def\\{name}{{{body}}}\\fi")
                }
                Definition::Letter(letter) => format!(
                    "\\ifdefined\\{name}\\else\\mathchardef\\{name}=\"70{:02X} \\fi",
                    u32::from(*letter)
                ),
            };
            lines.push_str(&line);
            lines.push('\n');
        }
        lines
    }
}

/// How the preamble defines one of [`FALLBACKS`], where no package has.
/// Pandoc, reading the LaTeX back, keeps the name of a command defined
/// under `\ifdefined`, where it would put the body of one that
/// `\providecommand` defines in its place.
enum Definition {
    /// A macro with this body.
    Macro(&'static str),
    /// A math character like `\Gamma`: an upright letter of the text font
    /// (math family 0), which `\mathbf` and the other alphabets restyle.
    Letter(char),
}

/// Commands that amsmath and amssymb lack, which [`math`] writes and which
/// math written for KaTeX or MathJax uses, and how the preamble defines
/// them.
const FALLBACKS: &[(&str, Definition)] = &[
    // mathtools' colon-equals, a relation like typst's `≔`.
    ("coloneqq", Definition::Macro("\\mathrel{:=}")),
    ("eqqcolon", Definition::Macro("\\mathrel{=:}")),
    ("Coloneqq", Definition::Macro("\\mathrel{::=}")),
    // Greek letters that look Latin, upright like TeX's Greek capitals and
    // italic like its small letters.
    ("Alpha", Definition::Letter('A')),
    ("Beta", Definition::Letter('B')),
    ("Epsilon", Definition::Letter('E')),
    ("Zeta", Definition::Letter('Z')),
    ("Eta", Definition::Letter('H')),
    ("Iota", Definition::Letter('I')),
    ("Kappa", Definition::Letter('K')),
    ("Mu", Definition::Letter('M')),
    ("Nu", Definition::Letter('N')),
    ("Omicron", Definition::Letter('O')),
    ("Rho", Definition::Letter('P')),
    ("Tau", Definition::Letter('T')),
    ("Chi", Definition::Letter('X')),
    ("omicron", Definition::Macro("o")),
    // mathtools' `\shortintertext`, as amsmath's `\intertext`, which sets
    // the text the same but for the space around it.
    ("shortintertext", Definition::Macro("\\intertext")),
];

/// `word` as the name of one of [`FALLBACKS`].
fn fallback(word: &str) -> Option<&'static str> {
    FALLBACKS
        .iter()
        .find_map(|&(name, _)| (name == word).then_some(name))
}

/// Commands that define a command only where none exists, which stop with
/// an error where one does (`\newcommand`) or keep it (`\providecommand`),
/// and the commands that redefine one in their place.
const NEW_DEFINITIONS: &[(&str, &str)] = &[
    ("newcommand", "renewcommand"),
    ("providecommand", "renewcommand"),
    ("NewDocumentCommand", "RenewDocumentCommand"),
    ("ProvideDocumentCommand", "RenewDocumentCommand"),
    (
        "NewExpandableDocumentCommand",
        "RenewExpandableDocumentCommand",
    ),
    (
        "ProvideExpandableDocumentCommand",
        "RenewExpandableDocumentCommand",
    ),
    ("NewCommandCopy", "RenewCommandCopy"),
];

/// Commands that define a command whether or not one exists.
const DEFINITIONS: &[&str] = &[
    "def",
    "edef",
    "gdef",
    "xdef",
    "let",
    "renewcommand",
    "DeclareRobustCommand",
    "RenewDocumentCommand",
    "DeclareDocumentCommand",
    "RenewExpandableDocumentCommand",
    "DeclareExpandableDocumentCommand",
    "RenewCommandCopy",
    "DeclareCommandCopy",
    "mathchardef",
    "chardef",
];

/// Of [`DEFINITIONS`], those that define it past their formula, which TeX
/// math is a group of.
const GLOBAL_DEFINITIONS: &[&str] = &["gdef", "xdef"];

/// The command that a definition's first argument at the start of `rest`
/// names: `\name` in `{\name}`, `\name` or `*{\name}`.
fn defined_command(rest: &[char]) -> String {
    let mut rest = rest.iter().copied().peekable();
    let mut skip = |wanted: Option<char>| {
        while rest.next_if(|c| c.is_whitespace()).is_some() {}
        if let Some(wanted) = wanted {
            rest.next_if_eq(&wanted);
        }
    };
    skip(Some('*'));
    skip(Some('{'));
    skip(None);
    match rest.next() {
        Some('\\') => rest.take_while(char::is_ascii_alphabetic).collect(),
        _ => String::new(),
    }
}

/// amssymb's name for the character at the start of `chars` with the
/// variation selector after it, for the variants amssymb has glyphs of: a
/// negated relation with its stroke elsewhere (`≨` and U+FE00, which
/// crosses its line, is `\lvertneqq`).
fn variant(chars: &[char]) -> Option<&'static str> {
    let [c, '\u{FE00}', ..] = *chars else {
        return None;
    };
    Some(match c {
        '≨' => "\\lvertneqq",
        '≩' => "\\gvertneqq",
        '⊊' => "\\varsubsetneq",
        '⊋' => "\\varsupsetneq",
        '⫋' => "\\varsubsetneqq",
        '⫌' => "\\varsupsetneqq",
        _ => return None,
    })
}

/// The LaTeX name of math character `c`.
fn name(c: char) -> Option<String> {
    named(c).map(str::to_string).or_else(|| alphanumeric(c))
}

/// The names amsmath and amssymb (and [`FALLBACKS`]) give math characters.
/// They follow the glyph: ε is `\varepsilon` and ϵ is `\epsilon`, and a
/// character LaTeX has no glyph for gets the closest one's name (`∊` is
/// `\in`).
fn named(c: char) -> Option<&'static str> {
    let tex = match c {
        // Greek.
        'α' => "\\alpha",
        'β' | 'ϐ' => "\\beta",
        'γ' => "\\gamma",
        'δ' => "\\delta",
        'ε' => "\\varepsilon",
        'ϵ' => "\\epsilon",
        '϶' => "\\backepsilon",
        'ζ' => "\\zeta",
        'η' => "\\eta",
        'θ' => "\\theta",
        'ϑ' => "\\vartheta",
        'ι' => "\\iota",
        'κ' => "\\kappa",
        'ϰ' => "\\varkappa",
        'λ' => "\\lambda",
        // The micro sign too, which macOS keyboards type for μ.
        'μ' | 'µ' => "\\mu",
        'ν' => "\\nu",
        'ξ' => "\\xi",
        'ο' => "\\omicron",
        'π' => "\\pi",
        'ϖ' => "\\varpi",
        'ρ' => "\\rho",
        'ϱ' => "\\varrho",
        'σ' => "\\sigma",
        'ς' => "\\varsigma",
        'τ' => "\\tau",
        'υ' => "\\upsilon",
        'φ' => "\\varphi",
        'ϕ' => "\\phi",
        'χ' => "\\chi",
        'ψ' => "\\psi",
        'ω' => "\\omega",
        'ϝ' => "\\digamma",
        'Α' => "\\Alpha",
        'Β' => "\\Beta",
        'Γ' => "\\Gamma",
        // The increment sign too, which macOS keyboards type for Δ.
        'Δ' | '∆' => "\\Delta",
        'Ε' => "\\Epsilon",
        'Ζ' => "\\Zeta",
        'Η' => "\\Eta",
        'Θ' => "\\Theta",
        'Ι' => "\\Iota",
        'Κ' => "\\Kappa",
        'Λ' => "\\Lambda",
        'Μ' => "\\Mu",
        'Ν' => "\\Nu",
        'Ξ' => "\\Xi",
        'Ο' => "\\Omicron",
        'Π' => "\\Pi",
        'Ρ' => "\\Rho",
        'Σ' => "\\Sigma",
        'Τ' => "\\Tau",
        'Υ' => "\\Upsilon",
        'Φ' => "\\Phi",
        'Χ' => "\\Chi",
        'Ψ' => "\\Psi",
        // The ohm sign too.
        'Ω' | '\u{2126}' => "\\Omega",
        // Letterlike symbols, and the letters Unicode's mathematical
        // alphanumerics leave to them.
        'ℂ' => "\\mathbb{C}",
        'ℍ' => "\\mathbb{H}",
        'ℕ' => "\\mathbb{N}",
        'ℙ' => "\\mathbb{P}",
        'ℚ' => "\\mathbb{Q}",
        'ℝ' => "\\mathbb{R}",
        'ℤ' => "\\mathbb{Z}",
        '𝕜' => "\\Bbbk",
        'ℬ' => "\\mathcal{B}",
        'ℰ' => "\\mathcal{E}",
        'ℱ' => "\\mathcal{F}",
        'ℋ' => "\\mathcal{H}",
        'ℐ' => "\\mathcal{I}",
        'ℒ' => "\\mathcal{L}",
        'ℳ' => "\\mathcal{M}",
        'ℛ' => "\\mathcal{R}",
        'ℭ' => "\\mathfrak{C}",
        'ℌ' => "\\mathfrak{H}",
        'ℨ' => "\\mathfrak{Z}",
        'ℜ' => "\\Re",
        'ℑ' => "\\Im",
        'ℎ' => "h",
        'ℏ' | 'ħ' => "\\hbar",
        'ℓ' => "\\ell",
        '℘' => "\\wp",
        'ℵ' => "\\aleph",
        'ℶ' => "\\beth",
        'ℷ' => "\\gimel",
        'ℸ' => "\\daleth",
        'Ⅎ' => "\\Finv",
        '⅁' => "\\Game",
        '℧' => "\\mho",
        'ı' => "\\imath",
        'ȷ' => "\\jmath",
        'ð' => "\\eth",
        '∂' => "\\partial",
        '∇' => "\\nabla",
        '∞' => "\\infty",
        // Operators.
        '−' => "-",
        '∕' => "/",
        '±' => "\\pm",
        '∓' => "\\mp",
        '×' => "\\times",
        '÷' => "\\div",
        '⋅' | '·' => "\\cdot",
        '∘' => "\\circ",
        '∙' | '•' => "\\bullet",
        '∗' => "\\ast",
        '⋆' => "\\star",
        '⊕' => "\\oplus",
        '⊖' => "\\ominus",
        '⊗' => "\\otimes",
        '⊘' => "\\oslash",
        '⊙' => "\\odot",
        '⊚' => "\\circledcirc",
        '⊛' => "\\circledast",
        '⊝' => "\\circleddash",
        '⊞' => "\\boxplus",
        '⊟' => "\\boxminus",
        '⊠' => "\\boxtimes",
        '⊡' => "\\boxdot",
        '⊎' => "\\uplus",
        '⊓' => "\\sqcap",
        '⊔' => "\\sqcup",
        '∔' => "\\dotplus",
        '⊻' => "\\veebar",
        '⊼' => "\\barwedge",
        '⋉' => "\\ltimes",
        '⋊' => "\\rtimes",
        '⋋' => "\\leftthreetimes",
        '⋌' => "\\rightthreetimes",
        '⋇' => "\\divideontimes",
        '⋏' => "\\curlywedge",
        '⋎' => "\\curlyvee",
        '⊺' => "\\intercal",
        '⋒' => "\\Cap",
        '⋓' => "\\Cup",
        '≀' => "\\wr",
        '⋄' => "\\diamond",
        '†' => "\\dagger",
        '‡' => "\\ddagger",
        '∖' => "\\setminus",
        '√' => "\\surd",
        '∑' => "\\sum",
        '∏' => "\\prod",
        '∐' => "\\coprod",
        '∫' => "\\int",
        '∬' => "\\iint",
        '∭' => "\\iiint",
        '⨌' => "\\iiiint",
        '∮' => "\\oint",
        '⋃' => "\\bigcup",
        '⋂' => "\\bigcap",
        '⋀' => "\\bigwedge",
        '⋁' => "\\bigvee",
        '⨁' => "\\bigoplus",
        '⨂' => "\\bigotimes",
        '⨀' => "\\bigodot",
        '⨄' => "\\biguplus",
        '⨆' => "\\bigsqcup",
        // Logic and sets.
        '∀' => "\\forall",
        '∃' => "\\exists",
        '∄' => "\\nexists",
        '¬' => "\\neg",
        '∧' => "\\land",
        '∨' => "\\lor",
        '∈' | '∊' => "\\in",
        '∉' => "\\notin",
        '∋' | '∍' => "\\ni",
        '∌' => "\\not\\ni",
        '⊂' => "\\subset",
        '⊃' => "\\supset",
        '⊆' => "\\subseteq",
        '⊇' => "\\supseteq",
        '⫅' => "\\subseteqq",
        '⫆' => "\\supseteqq",
        '⊄' => "\\not\\subset",
        '⊅' => "\\not\\supset",
        '⊈' => "\\nsubseteq",
        '⊉' => "\\nsupseteq",
        '⊊' => "\\subsetneq",
        '⊋' => "\\supsetneq",
        '⫋' => "\\subsetneqq",
        '⫌' => "\\supsetneqq",
        '⋐' => "\\Subset",
        '⋑' => "\\Supset",
        '⊏' => "\\sqsubset",
        '⊐' => "\\sqsupset",
        '⊑' => "\\sqsubseteq",
        '⊒' => "\\sqsupseteq",
        '⋢' => "\\not\\sqsubseteq",
        '⋣' => "\\not\\sqsupseteq",
        '∁' => "\\complement",
        '∪' => "\\cup",
        '∩' => "\\cap",
        '∅' => "\\emptyset",
        '⊤' => "\\top",
        '⊥' => "\\bot",
        '⊢' => "\\vdash",
        '⊣' => "\\dashv",
        '⊨' | '⊧' => "\\models",
        '⊩' => "\\Vdash",
        '⊪' => "\\Vvdash",
        '⊬' => "\\nvdash",
        '⊭' => "\\nvDash",
        '⊮' => "\\nVdash",
        '⊯' => "\\nVDash",
        '⊸' => "\\multimap",
        '∴' => "\\therefore",
        '∵' => "\\because",
        // Relations, and their negations: an amssymb name, or `\not`.
        '≠' => "\\neq",
        '≤' => "\\leq",
        '≥' => "\\geq",
        '≦' => "\\leqq",
        '≧' => "\\geqq",
        '≰' => "\\nleq",
        '≱' => "\\ngeq",
        '≮' => "\\nless",
        '≯' => "\\ngtr",
        '≨' => "\\lneqq",
        '≩' => "\\gneqq",
        '⪇' => "\\lneq",
        '⪈' => "\\gneq",
        '⋦' => "\\lnsim",
        '⋧' => "\\gnsim",
        '⪉' => "\\lnapprox",
        '⪊' => "\\gnapprox",
        '≶' => "\\lessgtr",
        '≷' => "\\gtrless",
        '≸' => "\\not\\lessgtr",
        '≹' => "\\not\\gtrless",
        '⋚' => "\\lesseqgtr",
        '⋛' => "\\gtreqless",
        '⪋' => "\\lesseqqgtr",
        '⪌' => "\\gtreqqless",
        '⋖' => "\\lessdot",
        '⋗' => "\\gtrdot",
        '⩽' => "\\leqslant",
        '⩾' => "\\geqslant",
        '⪕' => "\\eqslantless",
        '⪖' => "\\eqslantgtr",
        '≪' => "\\ll",
        '≫' => "\\gg",
        '⋘' => "\\lll",
        '⋙' => "\\ggg",
        '≈' => "\\approx",
        '≉' => "\\not\\approx",
        '≊' => "\\approxeq",
        '⪅' => "\\lessapprox",
        '⪆' => "\\gtrapprox",
        '≡' => "\\equiv",
        '≢' => "\\not\\equiv",
        '≅' => "\\cong",
        '≇' => "\\ncong",
        '∼' => "\\sim",
        '≁' => "\\nsim",
        '∽' => "\\backsim",
        '⋍' => "\\backsimeq",
        '≃' => "\\simeq",
        '≄' => "\\not\\simeq",
        '≂' => "\\eqsim",
        '≲' => "\\lesssim",
        '≳' => "\\gtrsim",
        '≴' => "\\not\\lesssim",
        '≵' => "\\not\\gtrsim",
        '≍' => "\\asymp",
        '≭' => "\\not\\asymp",
        '≐' => "\\doteq",
        '≑' => "\\doteqdot",
        '≒' => "\\fallingdotseq",
        '≓' => "\\risingdotseq",
        '≖' => "\\eqcirc",
        '≗' => "\\circeq",
        '≜' => "\\triangleq",
        '≏' => "\\bumpeq",
        '≎' => "\\Bumpeq",
        '∝' => "\\propto",
        '≺' => "\\prec",
        '≻' => "\\succ",
        '⪯' => "\\preceq",
        '⪰' => "\\succeq",
        '≼' => "\\preccurlyeq",
        '≽' => "\\succcurlyeq",
        '≾' => "\\precsim",
        '≿' => "\\succsim",
        '⋞' => "\\curlyeqprec",
        '⋟' => "\\curlyeqsucc",
        '⪷' => "\\precapprox",
        '⪸' => "\\succapprox",
        '⊀' => "\\nprec",
        '⊁' => "\\nsucc",
        '⋠' => "\\npreceq",
        '⋡' => "\\nsucceq",
        '⪵' => "\\precneqq",
        '⪶' => "\\succneqq",
        '⋨' => "\\precnsim",
        '⋩' => "\\succnsim",
        '⪹' => "\\precnapprox",
        '⪺' => "\\succnapprox",
        '∣' => "\\mid",
        '∤' => "\\nmid",
        '∥' => "\\parallel",
        '∦' => "\\nparallel",
        '⟂' => "\\perp",
        '⊲' => "\\vartriangleleft",
        '⊳' => "\\vartriangleright",
        '⊴' => "\\trianglelefteq",
        '⊵' => "\\trianglerighteq",
        '⋪' => "\\ntriangleleft",
        '⋫' => "\\ntriangleright",
        '⋬' => "\\ntrianglelefteq",
        '⋭' => "\\ntrianglerighteq",
        '⋈' => "\\bowtie",
        '≬' => "\\between",
        '⋔' => "\\pitchfork",
        '⌢' | '⌒' => "\\frown",
        '⌣' => "\\smile",
        // TeX's `:` is a relation, like the ratio sign.
        '∶' => ":",
        '∷' => "::",
        '≔' => "\\coloneqq",
        '≕' => "\\eqqcolon",
        '⩴' => "\\Coloneqq",
        // Arrows.
        // The black arrows too, whose glyphs LaTeX lacks.
        '→' | '➡' => "\\to",
        '←' | '⬅' => "\\leftarrow",
        '↔' => "\\leftrightarrow",
        '⇒' => "\\Rightarrow",
        '⇐' => "\\Leftarrow",
        '⇔' => "\\Leftrightarrow",
        '↛' => "\\nrightarrow",
        '↚' => "\\nleftarrow",
        '↮' => "\\nleftrightarrow",
        '⇏' => "\\nRightarrow",
        '⇍' => "\\nLeftarrow",
        '⇎' => "\\nLeftrightarrow",
        '⟶' => "\\longrightarrow",
        '⟵' => "\\longleftarrow",
        '⟷' => "\\longleftrightarrow",
        '⟹' => "\\Longrightarrow",
        '⟸' => "\\Longleftarrow",
        '⟺' => "\\Longleftrightarrow",
        '↦' => "\\mapsto",
        '⟼' => "\\longmapsto",
        '↑' | '⬆' => "\\uparrow",
        '↓' | '⬇' => "\\downarrow",
        '↕' => "\\updownarrow",
        '⇑' => "\\Uparrow",
        '⇓' => "\\Downarrow",
        '⇕' => "\\Updownarrow",
        '↗' => "\\nearrow",
        '↘' => "\\searrow",
        '↙' => "\\swarrow",
        '↖' => "\\nwarrow",
        '↪' => "\\hookrightarrow",
        '↩' => "\\hookleftarrow",
        '↠' => "\\twoheadrightarrow",
        '↞' => "\\twoheadleftarrow",
        '↣' => "\\rightarrowtail",
        '↢' => "\\leftarrowtail",
        '⇝' | '↝' => "\\rightsquigarrow",
        '↭' => "\\leftrightsquigarrow",
        '⇌' => "\\rightleftharpoons",
        '⇋' => "\\leftrightharpoons",
        '⇀' => "\\rightharpoonup",
        '⇁' => "\\rightharpoondown",
        '↼' => "\\leftharpoonup",
        '↽' => "\\leftharpoondown",
        '↾' => "\\upharpoonright",
        '↿' => "\\upharpoonleft",
        '⇂' => "\\downharpoonright",
        '⇃' => "\\downharpoonleft",
        '⇄' => "\\rightleftarrows",
        '⇆' => "\\leftrightarrows",
        '⇉' => "\\rightrightarrows",
        '⇇' => "\\leftleftarrows",
        '⇈' => "\\upuparrows",
        '⇊' => "\\downdownarrows",
        '↺' => "\\circlearrowleft",
        '↻' => "\\circlearrowright",
        '↶' => "\\curvearrowleft",
        '↷' => "\\curvearrowright",
        '⇢' => "\\dashrightarrow",
        '⇠' => "\\dashleftarrow",
        '⇚' => "\\Lleftarrow",
        '⇛' => "\\Rrightarrow",
        '↰' => "\\Lsh",
        '↱' => "\\Rsh",
        '↫' => "\\looparrowleft",
        '↬' => "\\looparrowright",
        // Dots: typst's `…` stays on the baseline, where amsmath's `\dots`
        // would centre it before an operator.
        '…' => "\\ldots",
        '⋯' => "\\cdots",
        '⋮' => "\\vdots",
        '⋱' => "\\ddots",
        // The double exclamation mark, as two.
        '‼' => "!!",
        // Superscripts in TeX, on an empty base so that they can follow a
        // script (`x^2°`), which a second superscript can't.
        '′' => "{}'",
        '″' => "{}''",
        '‴' => "{}'''",
        '°' => "{}^{\\circ}",
        // Shapes.
        '∠' => "\\angle",
        '∡' => "\\measuredangle",
        '∢' => "\\sphericalangle",
        '△' => "\\triangle",
        '▽' => "\\triangledown",
        '▲' => "\\blacktriangle",
        '▼' => "\\blacktriangledown",
        '▷' => "\\triangleright",
        '◁' => "\\triangleleft",
        '▶' => "\\blacktriangleright",
        '◀' => "\\blacktriangleleft",
        '□' | '◻' | '◽' | '⬜' => "\\square",
        '■' | '∎' | '◼' | '◾' | '⬛' | '▪' => "\\blacksquare",
        '◊' => "\\lozenge",
        '⧫' => "\\blacklozenge",
        '◇' => "\\Diamond",
        '○' => "\\bigcirc",
        '★' => "\\bigstar",
        '✓' | '✔' => "\\checkmark",
        '✠' => "\\maltese",
        '♠' | '♤' => "\\spadesuit",
        '♡' | '♥' => "\\heartsuit",
        '♢' | '♦' => "\\diamondsuit",
        '♣' | '♧' => "\\clubsuit",
        '♭' => "\\flat",
        '♮' => "\\natural",
        '♯' => "\\sharp",
        // Delimiters.
        '⟨' => "\\langle",
        '⟩' => "\\rangle",
        '⟪' => "\\langle\\!\\langle",
        '⟫' => "\\rangle\\!\\rangle",
        '⌈' => "\\lceil",
        '⌉' => "\\rceil",
        '⌊' => "\\lfloor",
        '⌋' => "\\rfloor",
        '⌜' => "\\ulcorner",
        '⌝' => "\\urcorner",
        '⌞' => "\\llcorner",
        '⌟' => "\\lrcorner",
        '⟮' => "\\lgroup",
        '⟯' => "\\rgroup",
        '‖' => "\\|",
        // Spaces, by their width, and the zero-width characters.
        '\u{00A0}' => "~",
        '\u{2002}' => "\\enspace",
        '\u{2001}' | '\u{2003}' | '\u{3000}' => "\\quad",
        '\u{2004}' => "\\;",
        '\u{2005}' | '\u{205F}' => "\\:",
        '\u{2006}' | '\u{2009}' | '\u{200A}' => "\\,",
        '\u{2007}' => "\\phantom{0}",
        '\u{2008}' => "\\phantom{.}",
        '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' => "{}",
        _ => return None,
    };
    Some(tex)
}

/// A letter or digit of Unicode's mathematical alphanumerics (`𝐀`, `𝔄`,
/// `𝕂`, `𝟎`) in the LaTeX alphabet of its style, for the styles LaTeX has.
fn alphanumeric(c: char) -> Option<String> {
    // Each style's `A`, which its capitals and then (`small`) its small
    // letters follow, and its alphabet: `None` is math's own italic.
    const LETTERS: &[(char, Option<&str>, bool)] = &[
        ('𝐀', Some("mathbf"), true),
        ('𝐴', None, true),
        ('𝑨', Some("boldsymbol"), true),
        ('𝒜', Some("mathcal"), false),
        ('𝔄', Some("mathfrak"), true),
        ('𝔸', Some("mathbb"), false),
        ('𝖠', Some("mathsf"), true),
        ('𝙰', Some("mathtt"), true),
    ];
    // Each style's `0`, and its alphabet.
    const DIGITS: &[(char, &str)] = &[('𝟎', "mathbf"), ('𝟢', "mathsf"), ('𝟶', "mathtt")];
    let offset =
        |first: char, count: u32| (c as u32).checked_sub(first as u32).filter(|&i| i < count);
    let styled = |alphabet: Option<&str>, plain: u32| {
        let plain = char::from_u32(plain)?;
        Some(match alphabet {
            Some(alphabet) => format!("\\{alphabet}{{{plain}}}"),
            None => plain.to_string(),
        })
    };
    for &(first, alphabet, small) in LETTERS {
        if let Some(i) = offset(first, if small { 52 } else { 26 }) {
            let plain = if i < 26 {
                'A' as u32 + i
            } else {
                'a' as u32 + i - 26
            };
            return styled(alphabet, plain);
        }
    }
    DIGITS
        .iter()
        .find_map(|&(first, alphabet)| styled(Some(alphabet), '0' as u32 + offset(first, 10)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latex(tex: &str) -> String {
        math(tex, &mut Fallbacks::default())
    }

    #[test]
    fn math_characters_get_their_names() {
        assert_eq!(latex("α + β → γ"), "\\alpha + \\beta \\to \\gamma");
        assert_eq!(latex("∀ x ∈ ℝ"), "\\forall x \\in \\mathbb{R}");
        assert_eq!(latex("a ≔ b ≇ c"), "a \\coloneqq b \\ncong c");
        assert_eq!(latex("x_1 … x_n"), "x_1 \\ldots x_n");
        // The keyboard's micro and increment signs are μ and Δ.
        assert_eq!(latex("µ ∆"), "\\mu \\Delta");
        // Commands and text in the math stay as they are.
        assert_eq!(
            latex("\\alpha \\{ \\\\ \\text{α → β}"),
            "\\alpha \\{ \\\\ \\text{α → β}"
        );
        assert_eq!(
            latex("\\text{a {b} \\} c} → \\tag*{α}"),
            "\\text{a {b} \\} c} \\to \\tag*{α}"
        );
        // Characters LaTeX has no name for are text, which pdfLaTeX takes.
        assert_eq!(
            latex("中文 ⟦ é ⟧"),
            "\\text{中文} \\text{⟦} \\text{é} \\text{⟧}"
        );
        assert_eq!(latex("x^é"), "x^{\\text{é}}");
        // Spaces get their widths, and zero-width characters none.
        assert_eq!(latex("a\u{2009}b\u{2003}c\u{200B}d"), "a\\,b\\quad c{}d");
    }

    #[test]
    fn names_neither_run_into_letters_nor_split_scripts() {
        // A letter after a name would be part of it.
        assert_eq!(latex("∀x∈ℝ"), "\\forall x\\in\\mathbb{R}");
        assert_eq!(latex("\\cdot𝑥 ∀中"), "\\cdot x \\forall\\text{中}");
        // A script takes one token, which a name of several isn't.
        assert_eq!(latex("x^α + y_ℝ"), "x^{\\alpha} + y_{\\mathbb{R}}");
        assert_eq!(latex("x^ ∘"), "x^ {\\circ}");
        // Degrees, primes and script characters go on an empty base, which
        // can follow another script.
        assert_eq!(latex("x^{2}°"), "x^{2}{}^{\\circ}");
        assert_eq!(latex("x^{2}′"), "x^{2}{}'");
        assert_eq!(latex("x₁₂ + y⁻¹"), "x{}_{12} + y{}^{-1}");
    }

    #[test]
    fn styled_letters_get_latex_alphabets() {
        assert_eq!(
            latex("𝐅 𝑥 𝒜 𝔄 𝔞 𝕂 𝖠 𝙰"),
            "\\mathbf{F} x \\mathcal{A} \\mathfrak{A} \\mathfrak{a} \\mathbb{K} \\mathsf{A} \\mathtt{A}"
        );
        assert_eq!(latex("𝟎 𝟣 ℎ"), "\\mathbf{0} \\mathsf{1} h");
        // Styles LaTeX lacks are text.
        assert_eq!(latex("𝒶 𝕒"), "\\text{𝒶} \\text{𝕒}");
        // Capitals that look Latin follow `\mathbf` as TeX's Greek does.
        assert_eq!(latex("\\mathbf{Α} Μ ο"), "\\mathbf{\\Alpha} \\Mu \\omicron");
    }

    #[test]
    fn commands_latex_lacks_are_defined_where_the_math_uses_them() {
        let mut fallbacks = Fallbacks::default();
        math("a ≔ b + Α", &mut fallbacks);
        math("c \\eqqcolon d", &mut fallbacks);
        assert_eq!(
            fallbacks.preamble(),
            "\\ifdefined\\coloneqq\\else\\def\\coloneqq{\\mathrel{:=}}\\fi\n\
             \\ifdefined\\eqqcolon\\else\\def\\eqqcolon{\\mathrel{=:}}\\fi\n\
             \\ifdefined\\Alpha\\else\\mathchardef\\Alpha=\"7041 \\fi\n"
        );
        let mut fallbacks = Fallbacks::default();
        math("a = b", &mut fallbacks);
        assert_eq!(fallbacks.preamble(), "");
    }

    /// Regression for #58: math that defines one of the commands itself
    /// (for its formula, which TeX math is a group of) keeps its own, which
    /// the preamble's would make an error: it redefines the preamble's.
    #[test]
    fn the_maths_own_definitions_hold_in_its_formula() {
        for (definition, redefinition) in [
            (
                "\\newcommand{\\coloneqq}{:=}",
                "\\renewcommand{\\coloneqq}{:=}",
            ),
            (
                "\\newcommand*\\coloneqq{:=}",
                "\\renewcommand*\\coloneqq{:=}",
            ),
            (
                "\\newcommand * { \\Alpha }{A}",
                "\\renewcommand * { \\Alpha }{A}",
            ),
            (
                "\\NewCommandCopy{\\Mu}{\\mu}",
                "\\RenewCommandCopy{\\Mu}{\\mu}",
            ),
        ] {
            assert_eq!(latex(definition), redefinition);
        }
        // Redefining needs the preamble's definition, and other commands
        // have none.
        for kept in ["\\renewcommand{\\coloneqq}{=}", "\\newcommand{\\R}{x}"] {
            assert_eq!(latex(kept), kept);
        }
    }

    /// Regression for #58: `\providecommand` defines a command only where
    /// none exists, so the preamble's definition would win over the
    /// math's own as well.
    #[test]
    fn the_maths_own_provided_commands_hold_in_its_formula() {
        for (definition, redefinition) in [
            (
                "\\providecommand{\\coloneqq}{\\mathrel{\\equiv}}",
                "\\renewcommand{\\coloneqq}{\\mathrel{\\equiv}}",
            ),
            (
                "\\providecommand*{\\Alpha}{A}",
                "\\renewcommand*{\\Alpha}{A}",
            ),
            (
                "\\ProvideDocumentCommand{\\eqqcolon}{}{=:}",
                "\\RenewDocumentCommand{\\eqqcolon}{}{=:}",
            ),
            (
                "\\ProvideExpandableDocumentCommand{\\Nu}{}{N}",
                "\\RenewExpandableDocumentCommand{\\Nu}{}{N}",
            ),
        ] {
            assert_eq!(latex(definition), redefinition);
        }
    }

    /// Regression for #58: a definition the document made before (in the
    /// formula, or for the rest of the document) is the one that
    /// `\providecommand` keeps and that `\newcommand` stops at, as LaTeX
    /// without the preamble's would.
    #[test]
    fn earlier_definitions_of_the_commands_are_kept() {
        for kept in [
            "\\def\\coloneqq{\\equiv}\\providecommand{\\coloneqq}{\\sim}",
            "\\let\\Alpha=\\beta \\newcommand{\\Alpha}{Z}",
        ] {
            assert_eq!(latex(kept), kept);
        }
        assert_eq!(
            latex("\\providecommand{\\coloneqq}{\\equiv}\\providecommand{\\coloneqq}{\\sim}"),
            "\\renewcommand{\\coloneqq}{\\equiv}\\providecommand{\\coloneqq}{\\sim}"
        );
        let mut fallbacks = Fallbacks::default();
        for (formula, written) in [
            ("\\gdef\\coloneqq{\\equiv}", "\\gdef\\coloneqq{\\equiv}"),
            (
                "\\global\\long\\def\\Alpha{Q}",
                "\\global\\long\\def\\Alpha{Q}",
            ),
            (
                "\\providecommand{\\coloneqq}{\\sim} \\providecommand{\\Alpha}{Z}",
                "\\providecommand{\\coloneqq}{\\sim} \\providecommand{\\Alpha}{Z}",
            ),
        ] {
            assert_eq!(math(formula, &mut fallbacks), written);
        }
        // One in another formula, or in the math inside a text argument,
        // ended with it.
        let mut fallbacks = Fallbacks::default();
        math("\\def\\coloneqq{\\equiv}", &mut fallbacks);
        assert_eq!(
            math(
                "\\text{$\\def\\coloneqq{x}$} \\providecommand{\\coloneqq}{\\sim}",
                &mut fallbacks
            ),
            "\\text{$\\def\\coloneqq{x}$} \\renewcommand{\\coloneqq}{\\sim}"
        );
        // The math inside a text argument sees the formula's.
        assert_eq!(
            latex("\\def\\coloneqq{x} \\text{$\\providecommand{\\coloneqq}{\\sim}$}"),
            "\\def\\coloneqq{x} \\text{$\\providecommand{\\coloneqq}{\\sim}$}"
        );
    }

    /// A definition made in a group (in braces, or a text argument) ends
    /// with it, as TeX ends it, unless it's made for the rest of the
    /// document: after the group, the preamble's definition is back, and
    /// the math's own redefines it.
    #[test]
    fn definitions_end_with_their_group() {
        for (formula, written) in [
            (
                "{\\def\\coloneqq{x}}\\newcommand{\\coloneqq}{y}",
                "{\\def\\coloneqq{x}}\\renewcommand{\\coloneqq}{y}",
            ),
            (
                "\\text{\\def\\coloneqq{x}}\\providecommand{\\coloneqq}{y}",
                "\\text{\\def\\coloneqq{x}}\\renewcommand{\\coloneqq}{y}",
            ),
            (
                "\\text{{\\def\\coloneqq{x}} $\\newcommand{\\coloneqq}{y}$}",
                "\\text{{\\def\\coloneqq{x}} $\\renewcommand{\\coloneqq}{y}$}",
            ),
        ] {
            assert_eq!(latex(formula), written);
        }
        for kept in [
            // Inside the group, the definition before it holds.
            "\\def\\coloneqq{x}{\\providecommand{\\coloneqq}{y}}",
            "{\\global\\def\\coloneqq{x}}\\providecommand{\\coloneqq}{y}",
            "{\\gdef\\coloneqq{x}}\\newcommand{\\coloneqq}{y}",
        ] {
            assert_eq!(latex(kept), kept);
        }
    }

    /// A heading's short form, which LaTeX writes to the table of contents
    /// and reads back, protects each control word of math that defines a
    /// command, control symbols aside, and leaves other math as it is.
    #[test]
    fn moving_math_protects_definitions() {
        assert!(matches!(
            moving("a \\coloneqq b"),
            Cow::Borrowed("a \\coloneqq b")
        ));
        assert_eq!(
            moving("\\renewcommand{\\coloneqq}{\\mathrel{\\equiv}} a \\coloneqq b \\\\ \\{"),
            "\\protect\\renewcommand{\\protect\\coloneqq}{\\protect\\mathrel{\\protect\\equiv}} \
             a \\protect\\coloneqq b \\\\ \\{"
        );
        assert_eq!(
            moving("\\global\\let\\Alpha=A"),
            "\\protect\\global\\protect\\let\\protect\\Alpha=A"
        );
    }

    /// Math that ends in a comment, which would take what follows it on
    /// its line: not an escaped `%`, one in `\verb`, or one a line break
    /// ends.
    #[test]
    fn comments_at_the_end_of_math_are_found() {
        for (math, ends) in [
            ("a % note", true),
            ("a \\\\% note", true),
            ("\\text{a % b}", true),
            ("a % note\nb", false),
            ("50\\%", false),
            ("\\verb|%| x", false),
            ("\\verb*+%+", false),
            ("a\\", false),
        ] {
            assert_eq!(ends_in_comment(math), ends, "{math}");
        }
    }

    /// Regression for #58: pandoc, reading the LaTeX back, expands a macro
    /// that `\providecommand` defines, where it keeps one that `\def`
    /// defines under `\ifdefined`.
    #[test]
    fn fallback_macros_are_defined_only_where_undefined() {
        let mut fallbacks = Fallbacks::default();
        math("a ≔ b", &mut fallbacks);
        assert_eq!(
            fallbacks.preamble(),
            "\\ifdefined\\coloneqq\\else\\def\\coloneqq{\\mathrel{:=}}\\fi\n"
        );
    }

    /// Regression for #58: the math inside a text argument uses the
    /// commands too, and gets its characters' names.
    #[test]
    fn math_inside_text_is_math() {
        for formula in [
            "a = b \\quad \\text{where $c \\coloneqq d$}",
            "\\begin{align} a &= b \\intertext{where $c \\coloneqq d$} e &= f \\end{align}",
            "x \\textbf{a \\emph{b {\\(c \\coloneqq d\\)}}}",
        ] {
            let mut fallbacks = Fallbacks::default();
            assert_eq!(math(formula, &mut fallbacks), formula);
            assert!(fallbacks.preamble().contains("\\coloneqq"), "{formula}");
        }
        let mut fallbacks = Fallbacks::default();
        assert_eq!(
            math(
                "a \\intertext{for \\(c ≔ d\\) and $\\text{$α$}$, not \\$α\\$} b",
                &mut fallbacks
            ),
            "a \\intertext{for \\(c \\coloneqq d\\) and $\\text{$\\alpha$}$, not \\$α\\$} b"
        );
        assert!(fallbacks.preamble().contains("\\coloneqq"));
        // mathtools' `\shortintertext`, which amsmath lacks.
        let mut fallbacks = Fallbacks::default();
        math("a \\shortintertext{so} b", &mut fallbacks);
        assert!(
            fallbacks
                .preamble()
                .contains("\\def\\shortintertext{\\intertext}"),
            "{}",
            fallbacks.preamble()
        );
    }

    /// Regression for #58: text arguments and options nest no deeper than
    /// TeX's groups before the rest is kept as it is, on the 2 MiB stack
    /// conversions run on, and the math inside them isn't copied for each.
    /// (An option ends at the first `]` outside braces, so options nest
    /// in braces.)
    #[test]
    fn deeply_nested_text_stays_within_the_stack() {
        let math = [("\\text{$", "$}"), ("\\sqrt[{", "}]{x}")];
        for (open, close) in [("\\text{", "}")].into_iter().chain(math) {
            let tex = format!("{}α{}", open.repeat(20_000), close.repeat(20_000));
            let written = std::thread::Builder::new()
                .stack_size(2 << 20)
                .spawn(move || latex(&tex))
                .unwrap()
                .join()
                .unwrap();
            assert!(written.starts_with(&open.repeat(20_000)), "{open}");
            assert!(written.contains('α') && !written.contains("\\alpha"));
        }
        for (open, close) in math {
            let shallow = format!("{}α{}", open.repeat(3), close.repeat(3));
            assert_eq!(latex(&shallow), shallow.replace('α', "\\alpha"));
        }
    }

    /// A command's star and its option in brackets (`\operatorname*`,
    /// `\sqrt[3]`) are none of its arguments, so a name after them is one,
    /// with braces. The option is math too.
    #[test]
    fn stars_and_options_come_before_the_arguments() {
        assert_eq!(latex("\\sqrt[3]ℝ"), "\\sqrt[3]{\\mathbb{R}}");
        assert_eq!(
            latex("\\sqrt [α] ℝ + \\sqrt{ℝ}"),
            "\\sqrt [\\alpha] {\\mathbb{R}} + \\sqrt{\\mathbb{R}}"
        );
        assert_eq!(
            latex("\\operatorname* é x"),
            "\\operatorname* {\\text{é}} x"
        );
        assert_eq!(
            latex("\\xrightarrow[α]ℝ \\cfrac[l]1ℝ"),
            "\\xrightarrow[\\alpha]{\\mathbb{R}} \\cfrac[l]1{\\mathbb{R}}"
        );
        // A bracket in braces doesn't close the option.
        assert_eq!(latex("\\sqrt[{]}]ℝ"), "\\sqrt[{]}]{\\mathbb{R}}");
        // A command without an option takes the bracket for its argument.
        assert_eq!(latex("\\hat[ℝ"), "\\hat[\\mathbb{R}");
    }

    /// Text-mode commands keep their text (`\fbox`, `\textsuperscript`),
    /// after their settings where they have some (`\colorbox{red}{…}`,
    /// `\raisebox{1ex}[2ex]{…}`), which are kept too, and so does a link's
    /// URL.
    #[test]
    fn text_mode_commands_keep_their_text() {
        for kept in [
            "\\fbox{α → β}",
            "\\textsuperscript{α}\\textsubscript{β}",
            "\\llap{α}\\rlap{β}\\clap{γ}",
            "\\colorbox{red}{α}",
            "\\fcolorbox{red}{blue}{α}",
            "\\raisebox{1ex}[2ex][0ex]{α}",
            "\\parbox[t][3cm][s]{5cm}{α}",
            "\\makebox[2cm][l]{α}",
            "\\framebox[2cm]{α}",
            "\\href{https://e.org/α}{x}",
        ] {
            assert_eq!(latex(kept), kept);
        }
        // The math after them is math again.
        assert_eq!(latex("\\colorbox{red}{α} α"), "\\colorbox{red}{α} \\alpha");
        assert_eq!(latex("\\href{u}{α}"), "\\href{u}{\\alpha}");
        assert_eq!(latex("\\colorbox{red} é"), "\\colorbox{red}{é}");
    }

    /// Regression for #65: an argument without braces (`\text é`) is the
    /// next character or command that TeX skips spaces and comments to,
    /// and `\verb`'s is everything up to its delimiter; both are text, kept
    /// as they are. One character beyond ASCII gets braces, which pdfLaTeX
    /// needs to read all of it, and `\tag` to read it at all.
    #[test]
    fn unbraced_and_verbatim_text_is_kept() {
        for kept in [
            "\\text a",
            "\\textbf\\alpha",
            "\\text\\%",
            "\\verb|α|",
            "\\verb*+α β+ → x",
        ] {
            let named = kept.replace('→', "\\to");
            assert_eq!(latex(kept), named);
        }
        assert_eq!(latex("\\text é"), "\\text{é}");
        assert_eq!(latex("\\mbox é + x"), "\\mbox{é} + x");
        assert_eq!(latex("\\text é ≤ é"), "\\text{é} \\leq \\text{é}");
        assert_eq!(latex("\\mbox 中文"), "\\mbox{中}\\text{文}");
        assert_eq!(latex("x = 1 \\tag é"), "x = 1 \\tag{é}");
        assert_eq!(latex("\\tag * é"), "\\tag *{é}");
        assert_eq!(latex("\\text%note\n é"), "\\text%note\n{é}");
        // Only `\tag` has a star form: `\text` takes the `*`.
        assert_eq!(latex("\\text*é"), "\\text*\\text{é}");
        // TeX takes a space beyond ASCII for the argument.
        assert_eq!(latex("\\text\u{A0}é"), "\\text{\u{A0}}\\text{é}");
    }

    /// Regression for #65: a name of several tokens (`\text{é}`,
    /// `\mathbb{R}`) gets braces where a command takes it for its argument,
    /// which would be its first token, and only there.
    #[test]
    fn names_are_whole_arguments() {
        assert_eq!(
            latex("\\hat é + \\mathrm é"),
            "\\hat {\\text{é}} + \\mathrm {\\text{é}}"
        );
        assert_eq!(
            latex("\\vec ℝ \\bar 𝐱"),
            "\\vec {\\mathbb{R}} \\bar {\\mathbf{x}}"
        );
        assert_eq!(
            latex("\\frac 1 ℝ + \\frac{ℝ}中"),
            "\\frac 1 {\\mathbb{R}} + \\frac{\\mathbb{R}}{\\text{中}}"
        );
        assert_eq!(latex("\\operatorname é x"), "\\operatorname {\\text{é}} x");
        assert_eq!(
            latex("\\frac\\alpha ℝ + \\frac\\text{a}é"),
            "\\frac\\alpha {\\mathbb{R}} + \\frac\\text{a}{\\text{é}}"
        );
        // A command that takes no argument, or one it has, is left alone,
        // and so are names that aren't ordinary symbols.
        assert_eq!(
            latex("x \\in ℝ, \\hat a ℝ"),
            "x \\in \\mathbb{R}, \\hat a \\mathbb{R}"
        );
        assert_eq!(
            latex("\\quad ≢ \\mathrel ≢"),
            "\\quad \\not\\equiv \\mathrel \\not\\equiv"
        );
        // A comment's characters are its own.
        assert_eq!(latex("a % \\text{ é\nb"), "a % \\text{ é\nb");
    }

    /// Regression for #65: a character written as a script on an empty
    /// base (`°`, `²`, `′`) is a base of its own when a script follows it,
    /// which would otherwise be a second superscript on `{}`.
    #[test]
    fn script_characters_are_bases_of_their_own() {
        assert_eq!(latex("x°^2"), "x{{}^{\\circ}}^2");
        assert_eq!(latex("a²^3 + b³ ^{2}"), "a{{}^{2}}^3 + b{{}^{3}} ^{2}");
        assert_eq!(latex("x°_1 + y′^2"), "x{{}^{\\circ}}_1 + y{{}'}^2");
        assert_eq!(latex("x°'"), "x{{}^{\\circ}}'");
        assert_eq!(latex("p₁^2 + q⁻^2"), "p{{}_{1}}^2 + q{{}^{-}}^2");
        // A variation selector between them changes nothing.
        assert_eq!(latex("x°\u{FE0E}^2"), "x{{}^{\\circ}}^2");
        // Without a script after them, they need no braces.
        assert_eq!(latex("x°²"), "x{}^{\\circ}{}^{2}");
    }

    /// Regression for #65: amssymb's negated relations, and characters
    /// with a variation selector after them, which picks a glyph that
    /// LaTeX has no choice of.
    #[test]
    fn negated_relations_and_variation_selectors_get_names() {
        assert_eq!(
            latex("≨ ≩ ⪇ ⪈ ⋦ ⋧ ⪉ ⪊ ⪷ ⪸ ⪹ ⪺ ⪵ ⪶ ⋨ ⋩ ⋠ ⋡ ≭"),
            "\\lneqq \\gneqq \\lneq \\gneq \\lnsim \\gnsim \\lnapprox \\gnapprox \
             \\precapprox \\succapprox \\precnapprox \\succnapprox \\precneqq \
             \\succneqq \\precnsim \\succnsim \\npreceq \\nsucceq \\not\\asymp"
        );
        assert_eq!(
            latex("⪕ ⪖ ≂ ≴ ≵ ≸ ≹ ⋢ ⋣ ≄"),
            "\\eqslantless \\eqslantgtr \\eqsim \\not\\lesssim \\not\\gtrsim \
             \\not\\lessgtr \\not\\gtrless \\not\\sqsubseteq \\not\\sqsupseteq \\not\\simeq"
        );
        assert_eq!(
            latex("✔ ⬜ ⬛ ♥ ‼ ➡"),
            "\\checkmark \\square \\blacksquare \\heartsuit !! \\to"
        );
        assert_eq!(
            latex("a ↔\u{FE0E} b ∪\u{FE00}x"),
            "a \\leftrightarrow b \\cup x"
        );
        // amssymb has some of the glyphs it picks.
        assert_eq!(
            latex("≨\u{FE00} ⊋\u{FE00} ≨"),
            "\\lvertneqq \\varsupsetneq \\lneqq"
        );
        // In text, the selector stays with its character.
        assert_eq!(latex("中\u{FE00}"), "\\text{中\u{FE00}}");
        // As a script, it's an empty one.
        assert_eq!(latex("x^\u{FE0E} + y_\u{FE0F}"), "x^{{}} + y_{{}}");
    }
}

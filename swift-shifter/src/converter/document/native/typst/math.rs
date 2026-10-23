//! TeX math → typst math, via mitex and its vendored scope (`mitex/`).

/// Symbol names mitex 0.2.4 emits that typst has since renamed or removed
/// (checked against typst 0.15: each new name renders the same glyph).
const RENAMES: &[(&str, &str)] = &[
    ("angle.l", "chevron.l"),
    ("angle.r", "chevron.r"),
    ("arrow.l.dash", "arrow.l.dashed"),
    ("arrow.r.dash", "arrow.r.dashed"),
    ("ast.circle", "ast.o"),
    ("bracket.l.double", "bracket.l.stroked"),
    ("bracket.r.double", "bracket.r.stroked"),
    ("circle.nested", "compose.o"),
    ("dash.circle", "dash.o"),
    ("diff", "partial"),
    ("dot.circle", "dot.o"),
    ("dot.circle.big", "dot.o.big"),
    ("minus.circle", "minus.o"),
    ("ohm.inv", "Omega.inv"),
    ("planck.reduce", "planck"),
    ("plus.circle", "plus.o"),
    ("plus.circle.big", "plus.o.big"),
    ("sect", "inter"),
    ("sect.big", "inter.big"),
    ("sect.double", "inter.double"),
    ("sect.sq", "inter.sq"),
    ("times.circle", "times.o"),
    ("times.circle.big", "times.o.big"),
];

/// Commands mitex 0.2.4 doesn't know. It converts with its built-in command
/// spec whatever spec it's given, so each is defined as a macro instead:
/// (command, its definition). Where typst needs a function mitex never
/// writes, the macro wraps its argument in one mitex knows, behind a
/// private-use marker that [`MARKED`] turns into the right call. A
/// definition may use the macros listed before it.
const MACROS: &[(&str, &str)] = &[
    (
        r"\mathscr",
        "\\newcommand{\\mathscr}[1]{\\mathcal{\u{E000}#1}}",
    ),
    // `mathrlap` and `mathllap` are in our scope (`mitex/`).
    (
        r"\mathrlap",
        "\\newcommand{\\mathrlap}[1]{\\mathclap{\u{E001}#1}}",
    ),
    (
        r"\mathllap",
        "\\newcommand{\\mathllap}[1]{\\mathclap{\u{E002}#1}}",
    ),
    // TeX's `\rlap` and the like hold text.
    (r"\rlap", r"\newcommand{\rlap}[1]{\mathrlap{\text{#1}}}"),
    (r"\llap", r"\newcommand{\llap}[1]{\mathllap{\text{#1}}}"),
    (r"\clap", r"\newcommand{\clap}[1]{\mathclap{\text{#1}}}"),
];

/// The marked calls [`MACROS`] leave in mitex's output, and what they become.
const MARKED: &[(&str, &str)] = &[
    ("cal(\u{E000} ", "scr("),
    ("mathclap(\u{E001} ", "mathrlap("),
    ("mathclap(\u{E002} ", "mathllap("),
];

/// Typst math code for a TeX formula, to be evaluated with mitex's scope
/// (`eval(…, mode: "math", scope: mitex-scope)`).
pub fn tex_to_typst(tex: &str) -> Result<String, String> {
    let mut code = rename_symbols(&with_macros(tex)?);
    for (marked, call) in MARKED {
        code = code.replace(marked, call);
    }
    // A marker left over (the formula redefines `\mathclap`, say) would
    // show as a stray glyph, and the call it marks would be wrong.
    if code.contains(['\u{E000}', '\u{E001}', '\u{E002}']) {
        return Err("error: the formula redefines a command its conversion uses".into());
    }
    Ok(code)
}

/// mitex's conversion of `tex`, defining each of the [`MACROS`] only once
/// mitex says it doesn't know the command, so a formula's own definition
/// (`\newcommand`, `\providecommand`) of it wins wherever mitex reads it.
fn with_macros(tex: &str) -> Result<String, String> {
    let mut defined = vec![false; MACROS.len()];
    loop {
        let defines: String = MACROS
            .iter()
            .zip(&defined)
            .filter_map(|((_, define), &defined)| defined.then_some(*define))
            .collect();
        let err = match mitex::convert_math(&format!("{defines}{tex}"), None) {
            Ok(converted) => return Ok(converted),
            Err(err) => err,
        };
        let unknown = err.strip_prefix("error: unknown command: ");
        let missing = MACROS
            .iter()
            .position(|(command, _)| Some(*command) == unknown);
        match missing {
            Some(i) if !defined[i] => defined[i] = true,
            _ => return Err(err),
        }
    }
}

/// Replace whole dotted identifiers (`plus.circle`, never the `plus.circle`
/// inside `plus.circle.big`) from [`RENAMES`]. Text is copied through
/// untouched: string literals (`"…"`, e.g. from `\operatorname{…}`) and the
/// content blocks of function calls (`#textmath[…]` from `\text{…}`,
/// `#strong[…]` from `\textbf{…}`). A bare `[` is a math delimiter and its
/// contents are still renamed.
fn rename_symbols(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut chars = code.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        if c == '#' {
            out.push('#');
            while let Some(&(_, next)) = chars.peek() {
                if !(next.is_ascii_alphanumeric() || matches!(next, '.' | '-' | '_')) {
                    break;
                }
                out.push(next);
                chars.next();
            }
            // `#[…]` and `#func[…]` are both content blocks.
            if chars.peek().is_some_and(|&(_, next)| next == '[') {
                let mut depth = 0usize;
                let mut escaped = false;
                for (_, c) in chars.by_ref() {
                    out.push(c);
                    match c {
                        '\\' if !escaped => {
                            escaped = true;
                            continue;
                        }
                        '[' if !escaped => depth += 1,
                        ']' if !escaped => depth -= 1,
                        _ => {}
                    }
                    escaped = false;
                    if depth == 0 {
                        break;
                    }
                }
            }
            continue;
        }
        if c == '"' {
            out.push('"');
            let mut escaped = false;
            for (_, c) in chars.by_ref() {
                out.push(c);
                match c {
                    '\\' if !escaped => escaped = true,
                    '"' if !escaped => break,
                    _ => escaped = false,
                }
            }
            continue;
        }
        let joined_to_previous = out
            .chars()
            .last()
            .is_some_and(|p| p.is_ascii_alphanumeric() || p == '.' || p == '_');
        if !c.is_ascii_alphabetic() || joined_to_previous {
            out.push(c);
            continue;
        }
        // An identifier: letters, digits and dots, without a trailing dot.
        let mut end = start + c.len_utf8();
        while let Some(&(i, next)) = chars.peek() {
            if next.is_ascii_alphanumeric() || next == '.' {
                end = i + next.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        let ident = &code[start..end];
        let trimmed = ident.trim_end_matches('.');
        match RENAMES.iter().find(|(old, _)| *old == trimmed) {
            Some((_, new)) => {
                out.push_str(new);
                out.push_str(&ident[trimmed.len()..]);
            }
            None => out.push_str(ident),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mathscr_becomes_scr() {
        let code = |tex: &str| tex_to_typst(tex).unwrap().replace(' ', "");
        assert_eq!(code(r"\mathscr{F}"), "scr(F)");
        assert_eq!(code(r"\mathscr F + \mathcal{F}"), "scr(F)+cal(F)");
        assert_eq!(code(r"x_\mathscr{F}"), "x_(scr(F))");
        assert_eq!(code(r"\frac{\mathscr{AB}}{2}"), "frac(scr(AB),2)");
        assert!(!code(r"\mathscr{\mathscr{F}}").contains('\u{E000}'));
        // A formula's own definition wins.
        for define in [r"\newcommand", r"\providecommand"] {
            assert_eq!(
                code(&format!(
                    r"{define}{{\mathscr}}[1]{{\mathbf{{#1}}}} \mathscr{{F}}"
                )),
                code(r"\mathbf{F}"),
                "{define}"
            );
        }
    }

    #[test]
    fn laps_become_our_lap_functions() {
        let code = |tex: &str| tex_to_typst(tex).unwrap().replace(' ', "");
        assert_eq!(code(r"a\mathrlap{x}b"), "amathrlap(x)b");
        assert_eq!(code(r"\mathllap{x+1}"), "mathllap(x+1)");
        assert_eq!(code(r"\mathclap{x}"), "mathclap(x)");
        assert_eq!(code(r"\rlap{y}"), "mathrlap(#textmath[y];)");
        assert_eq!(code(r"\llap{y}"), "mathllap(#textmath[y];)");
        assert_eq!(
            code(r"\clap{y} \mathscr{F}"),
            "mathclap(#textmath[y];)scr(F)"
        );
        assert_eq!(code(r"\mathrlap{\mathllap{x}}"), "mathrlap(mathllap(x))");
        assert!(code(r"\rlap{$\mathllap{x}$}").contains("$mathllap(x)$"));
        // A formula's own definition wins, beside the macros it still needs.
        assert_eq!(
            code(r"\newcommand{\rlap}[1]{\mathbf{#1}} \rlap{y}+\mathllap{x}"),
            code(r"\mathbf{y}") + "+mathllap(x)"
        );
        for name in [
            "{\\mathscr}",
            "*{\\mathscr}",
            " {\\mathscr}",
            "* { \\mathscr }",
        ] {
            assert_eq!(
                code(&format!(
                    r"\providecommand{name}[1]{{\mathbf{{#1}}}} \mathscr{{F}}+\mathrlap{{x}}"
                )),
                code(r"\mathbf{F}") + "+mathrlap(x)",
                "{name}"
            );
        }
        assert_eq!(
            code("\\newcommand % note\n{\\rlap}[1]{\\mathbf{#1}} \\rlap{y}"),
            code(r"\mathbf{y}")
        );
        assert_eq!(
            code(r"\newcommand{\rlapx}[1]{#1} \rlapx{y}+\rlap{y}"),
            "y+mathrlap(#textmath[y];)"
        );
        // A definition in a comment defines nothing.
        assert_eq!(
            code("% \\newcommand{\\mathscr}[1]{#1}\n\\mathscr{F}").trim(),
            "scr(F)"
        );
        assert_eq!(
            code("% \\providecommand{\\rlap}[1]{#1}\n\\rlap{y}").trim(),
            "mathrlap(#textmath[y];)"
        );
        // A definition inside another's body defines nothing yet.
        assert_eq!(
            code(r"\newcommand{\foo}{\newcommand{\mathscr}[1]{#1}} \mathscr{F}"),
            "scr(F)"
        );
        // ... and doesn't cost the formula its other, real definitions.
        assert_eq!(
            code(
                r"\providecommand{\rlap}[1]{\mathbf{#1}} \newcommand{\foo}{\newcommand{\mathscr}[1]{#1}} \rlap{y}+\mathscr{F}"
            ),
            code(r"\mathbf{y}") + "+scr(F)"
        );
        // A definition in a group counts too.
        assert_eq!(
            code(
                r"\newcommand{\unused}{\newcommand{\mathscr}[1]{#1}} {\providecommand{\rlap}[1]{\mathbf{#1}} \rlap{y}}+\mathscr{F}"
            ),
            code(r"{\mathbf{y}}") + "+scr(F)"
        );
        // With `\mathclap` redefined the macros can't mark their calls.
        for lap in [r"\mathllap", r"\mathrlap"] {
            assert!(
                tex_to_typst(&format!(
                    r"\providecommand{{\mathclap}}[1]{{\mathbf{{#1}}}} {lap}{{x}}"
                ))
                .is_err(),
                "{lap}"
            );
        }
        // `\rlap` uses the formula's `\mathrlap`.
        assert_eq!(
            code(r"\newcommand{\mathrlap}[1]{\mathbf{#1}} \rlap{y}"),
            code(r"\mathbf{\text{y}}")
        );
    }

    #[test]
    fn renames_whole_identifiers_only() {
        assert_eq!(rename_symbols("plus.circle.big x"), "plus.o.big x");
        assert_eq!(
            rename_symbols("lr(angle.l u angle.r)"),
            "lr(chevron.l u chevron.r)"
        );
        assert_eq!(
            rename_symbols("diff x + difference"),
            "partial x + difference"
        );
        assert_eq!(rename_symbols("a2sect"), "a2sect");
        assert_eq!(rename_symbols("sect."), "inter.");
    }

    #[test]
    fn leaves_string_literals_alone() {
        assert_eq!(rename_symbols(r#""diff" diff"#), r#""diff" partial"#);
        assert_eq!(
            rename_symbols(r#"op("a \" sect") sect"#),
            r#"op("a \" sect") inter"#
        );
    }

    #[test]
    fn leaves_content_blocks_alone() {
        assert_eq!(
            rename_symbols("#textmath[diff] diff"),
            "#textmath[diff] partial"
        );
        assert_eq!(
            rename_symbols(r"#strong[a [sect\]] b] sect"),
            r"#strong[a [sect\]] b] inter"
        );
        // A bare bracket is a math delimiter.
        assert_eq!(rename_symbols("lr([ diff x ])"), "lr([ partial x ])");
        for tex in [r"\text{diff sect}", r"\textbf{diff}", r"\textit{sect}"] {
            let code = tex_to_typst(tex).unwrap();
            assert!(
                !code.contains("partial") && !code.contains("inter"),
                "{tex}: {code}"
            );
        }
        assert!(tex_to_typst(r"\partial x").unwrap().contains("partial"));
    }
}

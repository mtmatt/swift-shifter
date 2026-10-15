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

/// Typst math code for a TeX formula, to be evaluated with mitex's scope
/// (`eval(…, mode: "math", scope: mitex-scope)`).
pub fn tex_to_typst(tex: &str) -> Result<String, String> {
    let converted = mitex::convert_math(tex, None)?;
    Ok(rename_symbols(&converted))
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

//! PDF output through the typst compiler, linked in as a library: no typst
//! binary, no LaTeX, and one font setup that we control.

mod fonts;
mod math;
mod world;

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;

use typst::WorldExt;
use typst::diag::SourceDiagnostic;
use typst_layout::PagedDocument;

#[cfg(test)]
pub use fonts::fonts_scanned;
pub use fonts::{cjk_family, default_paper, has_cjk_font, text_fonts, warm_up};
pub use math::tex_to_typst;
use world::ShifterWorld;

/// Virtual directory for files Swift Shifter provides to every compile
/// (the mitex math scope) and for files registered per compile (images).
pub const VIRTUAL_DIR: &str = "/__shifter__";

/// One document to typeset.
pub struct Project {
    /// The main source's text.
    pub main: String,
    /// The main file's name, for user-written files (so it can't shadow a
    /// sibling `main.typ`, and errors name the right file). Generated
    /// sources use `main.typ`.
    pub main_name: Option<String>,
    /// The directory relative paths in the source resolve against.
    pub root: PathBuf,
    /// Extra files by virtual path (`/__shifter__/…`), e.g. images that live
    /// outside `root`.
    pub files: HashMap<String, PathBuf>,
    /// Whether `@preview/…` package imports may be downloaded (user-written
    /// `.typ` input); generated documents never import packages.
    pub allow_packages: bool,
}

#[derive(Debug)]
pub struct CompileError {
    /// The first error, readable on its own.
    pub message: String,
    /// Byte ranges in the main source involved in the errors (the errors'
    /// own spans and their call traces), so a caller that generated the
    /// source can find which part of it failed.
    pub main_ranges: Vec<Range<usize>>,
}

/// typst's standard library, the one every compile uses. Its math scope
/// holds typst's symbols (`bar.v`, `bracket.l`, …).
pub fn library() -> &'static typst::Library {
    &world::LIBRARY
}

/// Typeset `project` to PDF bytes. Blocking: run it off the async runtime.
pub fn compile_pdf(project: Project) -> Result<Vec<u8>, CompileError> {
    let world = ShifterWorld::new(project);
    let compiled = typst::compile::<PagedDocument>(&world);
    let document = compiled
        .output
        .map_err(|errors| describe(&world, &errors))?;
    typst_pdf::pdf(&document, &typst_pdf::PdfOptions::default())
        .map_err(|errors| describe(&world, &errors))
}

fn describe(world: &ShifterWorld, errors: &[SourceDiagnostic]) -> CompileError {
    let main = world.main_id();
    let mut main_ranges = Vec::new();
    for error in errors {
        let spans = std::iter::once(error.span).chain(error.trace.iter().map(|t| t.span.into()));
        for span in spans {
            if span.id() == Some(main)
                && let Some(range) = world.range(span)
            {
                main_ranges.push(range);
            }
        }
    }
    let message = match errors.first() {
        Some(first) => {
            let location = world.line_col(first.span);
            match location {
                Some((path, line, col)) => format!("{path}:{line}:{col}: {}", first.message),
                None => first.message.to_string(),
            }
        }
        None => "typst failed without an error message".to_string(),
    };
    CompileError {
        message,
        main_ranges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(main: &str) -> Project {
        Project {
            main: main.to_string(),
            main_name: None,
            root: std::env::temp_dir(),
            files: HashMap::new(),
            allow_packages: false,
        }
    }

    #[test]
    fn compiles_a_document_to_pdf() {
        let pdf = compile_pdf(project("= Hello\n\nWorld")).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        let doc = lopdf::Document::load_mem(&pdf).unwrap();
        assert_eq!(doc.get_pages().len(), 1);
    }

    #[test]
    fn errors_point_into_the_main_source() {
        let source = "Fine text.\n\n#nonexistent-function()\n";
        let err = compile_pdf(project(source)).unwrap_err();
        assert!(
            err.message.contains("nonexistent-function"),
            "{}",
            err.message
        );
        assert!(err.message.contains(":3:"), "line number: {}", err.message);
        let start = source.find("nonexistent").unwrap();
        assert!(
            err.main_ranges.iter().any(|r| r.contains(&start)),
            "{:?}",
            err.main_ranges
        );
    }

    #[test]
    fn mitex_scope_is_available_offline() {
        let tex = r"\frac{a}{b} + \sqrt{x^2} \xrightarrow{f} \begin{pmatrix} a & b \\ c & d \end{pmatrix}";
        let converted = tex_to_typst(tex).unwrap();
        let source = format!(
            "#import \"{VIRTUAL_DIR}/mitex/specs/mod.typ\": mitex-scope\n\
             #math.equation(block: true, eval({converted:?}, mode: \"math\", scope: mitex-scope))\n"
        );
        compile_pdf(project(&source)).unwrap();
    }

    /// Common TeX, each formula compiled on its own through mitex and the
    /// vendored scope. A failure here means the scope needs a patch.
    #[test]
    fn mitex_handles_common_tex() {
        let formulas = [
            r"\alpha \beta \gamma \Gamma \delta \epsilon \varepsilon \theta \lambda \mu \pi \sigma \phi \varphi \omega \Omega",
            r"a \leq b \geq c \neq d \approx e \equiv f \sim g \propto h \pm i \mp j \times k \div l \cdot m",
            r"\infty \partial \nabla \forall \exists \emptyset \in \notin \subset \subseteq \cup \cap \setminus",
            r"\to \rightarrow \leftarrow \Rightarrow \Leftrightarrow \mapsto \implies \iff \uparrow",
            r"\frac{a}{b} \dfrac{a}{b} \tfrac{a}{b} \binom{n}{k} \sqrt{x} \sqrt[3]{x}",
            r"\sum_{i=1}^{n} i \prod_{k} k \int_a^b f \iint g \oint h \lim_{x \to 0} \sup \inf \max \min",
            r"\hat{a} \bar{b} \tilde{c} \vec{v} \dot{x} \ddot{y} \overline{xy} \underline{z} \widehat{abc}",
            r"\mathbb{R} \mathbf{v} \mathrm{d} \mathit{x} \mathcal{L} \mathscr{F} \mathfrak{g} \mathsf{s} \mathtt{t} \boldsymbol{\alpha}",
            r"\text{if } x > 0 \operatorname{tr}(A) \sin x \cos y \tan z \log n \ln e \exp t \det A",
            r"\left( \frac{a}{b} \right) \left[ x \right] \left\{ y \right\} \left| z \right| \langle u \rangle",
            r"\begin{matrix} a & b \\ c & d \end{matrix}",
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix} \begin{bmatrix} 1 \\ 2 \end{bmatrix} \begin{vmatrix} a & b \\ c & d \end{vmatrix}",
            r"\begin{Bmatrix}a&b\\c&d\end{Bmatrix}",
            r"\begin{Vmatrix}a&b\\c&d\end{Vmatrix}",
            r"\mathrlap{x}",
            r"\mathllap{x}",
            r"\mathclap{x}",
            r"\rlap{y}",
            r"\llap{y}",
            r"\clap{y}",
            r"\sideset{_a^b}{_c^d}\sum \sideset{}{'}\prod_{i,j}",
            r"f(x) = \begin{cases} 1 & x > 0 \\ 0 & \text{otherwise} \end{cases}",
            r"\begin{aligned} a &= b + c \\ d &= e \end{aligned}",
            r"\begin{array}{cc} 1 & 2 \\ 3 & 4 \end{array}",
            r"x \xrightarrow{f} y \xleftarrow{g} z",
            r"\underbrace{a + b}_{n} \overbrace{c + d}^{m} \stackrel{def}{=}",
            r"a \bmod b \pmod{n} \gcd(a, b)",
            r"\ldots \cdots \vdots \ddots \quad \qquad \, \; \! \ ",
            r"x^{2} y_{i} z_{i}^{2} e^{i\pi} a'' f^{(n)}",
            r"\not\in \not= \nleq \ngeq",
            r"\displaystyle \sum_{n=1}^\infty \frac{1}{n^2} = \frac{\pi^2}{6}",
            r"\mathbb{E}[X] = \int_{-\infty}^{\infty} x f(x) \, dx",
            r"\hbar \ell \Re \Im \aleph \wp",
            r"\circ \bullet \star \dagger \ddagger \oplus \otimes \odot",
            r"\{ a, b \} \| v \| \lfloor x \rfloor \lceil y \rceil",
            // Symbols typst renamed since mitex 0.2.4 (see math::RENAMES).
            r"\partial \langle x \rangle \oplus \ominus \otimes \odot \circledast \circleddash \bigoplus \bigotimes \bigodot",
            r"\cap \bigcap \Cap \sqcap \hbar \mho \dashleftarrow \dashrightarrow \llbracket \rrbracket \circledcirc",
        ];
        let failures: Vec<String> = formulas
            .iter()
            .filter_map(|tex| {
                let converted = match tex_to_typst(tex) {
                    Ok(c) => c,
                    Err(e) => return Some(format!("{tex}\n  convert: {e}")),
                };
                let source = format!(
                    "#import \"{VIRTUAL_DIR}/mitex/specs/mod.typ\": mitex-scope\n\
                     #math.equation(block: true, eval({converted:?}, mode: \"math\", scope: mitex-scope))\n"
                );
                compile_pdf(project(&source))
                    .err()
                    .map(|e| format!("{tex}\n  -> {converted}\n  {}", e.message))
            })
            .collect();
        assert!(
            failures.is_empty(),
            "{} failing:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    /// `\mathrlap`, `\mathllap` and `\mathclap` take no width and set their
    /// content after, before and across the point they're at, on one line:
    /// checked by typst itself, from where markers around them are laid out.
    #[test]
    fn laps_take_no_width_and_overlap_their_side() {
        let source = format!(
            r#"#import "{VIRTUAL_DIR}/mitex/specs/mod.typ": mitex-scope
#let x(name, side) = label(name + "-" + side)
#for name in ("mathrlap", "mathllap", "mathclap") [
  a#metadata(none)#x(name, "before")#(mitex-scope.at(name))[#metadata(none)#x(name, "start")$x + y + z$#metadata(none)#x(name, "end")]#metadata(none)#x(name, "after")b
  #context {{
    let at(side) = locate(x(name, side)).position()
    let (before, start, end, after) = ("before", "start", "end", "after").map(at)
    assert.eq(before, after, message: name + " takes width")
    assert.eq(start.y, end.y, message: name + " breaks its line")
    assert(end.x - start.x > 10pt, message: name + " is empty")
    let middle = (start.x + end.x) / 2
    let (lead, trail) = (before.x - start.x, end.x - before.x)
    if name == "mathrlap" {{ assert.eq(start.x, before.x, message: name) }}
    if name == "mathllap" {{ assert.eq(end.x, before.x, message: name) }}
    if name == "mathclap" {{ assert(calc.abs((lead - trail).pt()) < 0.01, message: name) }}
  }}
]
"#
        );
        if let Err(e) = compile_pdf(project(&source)) {
            panic!("{}", e.message);
        }
    }

    /// `\sideset{_a^b}{_c^d}` sets `a` and `b` at the left corners of what
    /// follows, `c` and `d` at its right ones: checked by typst, from where
    /// markers standing in for the scripts and the operator's ends are laid
    /// out.
    #[test]
    fn sideset_puts_scripts_at_the_corners() {
        let converted = tex_to_typst(r"\sideset{_\alpha^\beta}{_\gamma^\delta}\sum").unwrap();
        let primed = tex_to_typst(r"\sideset{}{'}\sum").unwrap();
        let primed_sub = tex_to_typst(r"\sideset{}{'_a}\sum").unwrap();
        let source = format!(
            r#"#import "{VIRTUAL_DIR}/mitex/specs/mod.typ": mitex-scope
#let m(name) = [#metadata(none)#label(name)]
#let script(name) = [#m(name)x]
#let scope = mitex-scope + (
  alpha: script("bl"), beta: script("tl"), gamma: script("br"), delta: script("tr"),
  sum: [#m("start")#sym.sum#m("end")],
)
#math.equation(block: true, eval({converted:?}, mode: "math", scope: scope))
#context {{
  let at(name) = locate(label(name)).position()
  let (bl, tl, br, tr, start, end) = ("bl", "tl", "br", "tr", "start", "end").map(at)
  assert(tl.x < start.x and bl.x < start.x, message: "left scripts aren't left")
  assert(tr.x >= end.x and br.x >= end.x, message: "right scripts aren't right")
  assert(tl.y < bl.y, message: "left scripts are upside down")
  assert(tr.y < br.y, message: "right scripts are upside down")
}}
#let primed = eval({primed:?}, mode: "math", scope: mitex-scope)
#assert(primed.body.at("tr", default: none) != none, message: "the prime is lost")
#let primed-sub = eval({primed_sub:?}, mode: "math", scope: mitex-scope).body
#assert(primed-sub.at("tr", default: none) != none, message: "the prime beside `_a` is lost")
#assert(primed-sub.at("br", default: none) != none, message: "`_a` beside the prime is lost")
"#
        );
        if let Err(e) = compile_pdf(project(&source)) {
            panic!("{converted}\n{}", e.message);
        }
    }

    #[test]
    fn registered_files_are_readable() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("pixel.png");
        image::RgbImage::new(4, 4).save(&img).unwrap();
        let mut p = project(&format!("#image(\"{VIRTUAL_DIR}/img/0.png\")"));
        p.files.insert(format!("{VIRTUAL_DIR}/img/0.png"), img);
        compile_pdf(p).unwrap();
    }
}

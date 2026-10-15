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

pub use fonts::{default_paper, has_cjk_font, text_fonts, warm_up};
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
            r"\mathbb{R} \mathbf{v} \mathrm{d} \mathit{x} \mathcal{L} \mathfrak{g} \mathsf{s} \mathtt{t} \boldsymbol{\alpha}",
            r"\text{if } x > 0 \operatorname{tr}(A) \sin x \cos y \tan z \log n \ln e \exp t \det A",
            r"\left( \frac{a}{b} \right) \left[ x \right] \left\{ y \right\} \left| z \right| \langle u \rangle",
            r"\begin{matrix} a & b \\ c & d \end{matrix}",
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix} \begin{bmatrix} 1 \\ 2 \end{bmatrix} \begin{vmatrix} a & b \\ c & d \end{vmatrix}",
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

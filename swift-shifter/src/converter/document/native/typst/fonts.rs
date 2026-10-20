//! Fonts for typesetting: typst's embedded fonts plus everything installed,
//! scanned once per process, and the text font list for a document.

use std::sync::LazyLock;

use typst_kit::fonts::FontStore;

/// Scanning system fonts takes over a second, so it happens once, on first
/// use (or ahead of time via [`warm_up`]).
static STORE: LazyLock<FontStore> = LazyLock::new(|| {
    let mut store = FontStore::new();
    store.extend(typst_kit::fonts::embedded());
    store.extend(typst_kit::fonts::system());
    store
});

pub(super) fn store() -> &'static FontStore {
    &STORE
}

/// Start scanning fonts in the background so the first PDF conversion
/// doesn't pay for it.
pub fn warm_up() {
    std::thread::spawn(|| {
        LazyLock::force(&STORE);
    });
}

/// Whether this process has scanned fonts yet.
#[cfg(test)]
pub fn fonts_scanned() -> bool {
    LazyLock::get(&STORE).is_some()
}

/// The paper size for the user's region: US Letter in the US, Canada,
/// Mexico and the Philippines, A4 elsewhere.
pub fn default_paper() -> &'static str {
    let region = sys_locale::get_locale().and_then(|locale| {
        locale
            .rsplit(['-', '_'])
            .next()
            .map(str::to_ascii_uppercase)
    });
    match region.as_deref() {
        Some("US" | "CA" | "MX" | "PH") => "us-letter",
        _ => "a4",
    }
}

/// Latin text font, embedded in the typst assets so it's always present.
const LATIN: &str = "Libertinus Serif";

/// One script's CJK families, most preferred first, and the one the
/// operating system installs for it.
#[derive(Clone, Copy)]
struct CjkScript {
    /// Serif faces lead to match Libertinus Serif. Covers macOS, Windows
    /// and common Linux packages.
    families: &'static [&'static str],
    /// What to name when fonts haven't been scanned: macOS and Windows ship
    /// it, and Noto CJK is the usual Linux package.
    stock: &'static str,
}

/// The stock family on macOS, on Windows, and elsewhere.
const fn stock(macos: &'static str, windows: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        macos
    } else if cfg!(windows) {
        windows
    } else {
        other
    }
}

const SIMPLIFIED: CjkScript = CjkScript {
    families: &[
        "Noto Serif CJK SC",
        "Source Han Serif SC",
        "Songti SC",
        "STSong",
        "SimSun",
        "AR PL UMing CN",
        "Noto Sans CJK SC",
        "Source Han Sans SC",
        "PingFang SC",
        "Hiragino Sans GB",
        "Microsoft YaHei",
        "WenQuanYi Zen Hei",
        "WenQuanYi Micro Hei",
        "Droid Sans Fallback",
    ],
    stock: stock("Songti SC", "SimSun", "Noto Serif CJK SC"),
};
const TRADITIONAL: CjkScript = CjkScript {
    families: &[
        "Noto Serif CJK TC",
        "Source Han Serif TC",
        "Songti TC",
        "LiSong Pro",
        "PMingLiU",
        "MingLiU",
        "AR PL UMing TW",
        "Noto Sans CJK TC",
        "PingFang TC",
        "Microsoft JhengHei",
    ],
    stock: stock("Songti TC", "Microsoft JhengHei", "Noto Serif CJK TC"),
};
const JAPANESE: CjkScript = CjkScript {
    families: &[
        "Noto Serif CJK JP",
        "Source Han Serif JP",
        "Hiragino Mincho ProN",
        "Yu Mincho",
        "MS Mincho",
        "IPAexMincho",
        "Noto Sans CJK JP",
        "Hiragino Sans",
        "Yu Gothic",
        "Meiryo",
    ],
    stock: stock("Hiragino Mincho ProN", "Yu Gothic", "Noto Serif CJK JP"),
};
const KOREAN: CjkScript = CjkScript {
    families: &[
        "Noto Serif CJK KR",
        "Source Han Serif KR",
        "AppleMyungjo",
        "Batang",
        "Noto Sans CJK KR",
        "Apple SD Gothic Neo",
        "Malgun Gothic",
        "NanumGothic",
    ],
    stock: stock("AppleMyungjo", "Malgun Gothic", "Noto Serif CJK KR"),
};

/// The CJK scripts for a document in `lang`: its own first, so typst picks
/// matching glyph shapes, then the rest as fallbacks.
fn scripts(lang: Option<&str>) -> [CjkScript; 4] {
    let lang = lang.unwrap_or("").to_ascii_lowercase();
    if lang.starts_with("ja") {
        [JAPANESE, SIMPLIFIED, TRADITIONAL, KOREAN]
    } else if lang.starts_with("ko") {
        [KOREAN, SIMPLIFIED, TRADITIONAL, JAPANESE]
    } else if ["zh-hant", "zh-tw", "zh-hk", "zh-mo"]
        .iter()
        .any(|p| lang.starts_with(p))
    {
        [TRADITIONAL, SIMPLIFIED, JAPANESE, KOREAN]
    } else {
        [SIMPLIFIED, TRADITIONAL, JAPANESE, KOREAN]
    }
}

/// Every CJK family of `scripts`, in order.
fn families(scripts: [CjkScript; 4]) -> impl Iterator<Item = &'static str> {
    scripts
        .into_iter()
        .flat_map(|script| script.families.iter().copied())
}

/// Whether `store` has `family`, matched without case as typst does.
fn installed(store: &FontStore, family: &str) -> bool {
    store
        .book()
        .select_family(&family.to_lowercase())
        .next()
        .is_some()
}

/// The `text(font: …)` list for a document in `lang`: the Latin font, then
/// the installed CJK families in [`scripts`] order.
pub fn text_fonts(lang: Option<&str>) -> Vec<String> {
    let store = store();
    std::iter::once(LATIN)
        .chain(families(scripts(lang)).filter(|family| installed(store, family)))
        .map(str::to_string)
        .collect()
}

/// The CJK family to name for a document in `lang` where only one name is
/// wanted, as in a LaTeX preamble. Naming a font isn't worth the
/// second-long font scan, so the family [`text_fonts`] would put first is
/// used only when fonts have been scanned already; otherwise it's the
/// stock family for the document's script. Either way it's a guess about
/// the computer converting, and a `.tex` compiled elsewhere may need it
/// changed.
pub fn cjk_family(lang: Option<&str>) -> &'static str {
    let scripts = scripts(lang);
    LazyLock::get(&STORE)
        .and_then(|store| families(scripts).find(|family| installed(store, family)))
        .unwrap_or(scripts[0].stock)
}

/// Whether a list from [`text_fonts`] has any CJK family. Without one,
/// typst would draw CJK text as empty boxes.
pub fn has_cjk_font(fonts: &[String]) -> bool {
    fonts.iter().any(|f| f != LATIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_font_always_leads() {
        let fonts = text_fonts(Some("en"));
        assert_eq!(fonts[0], "Libertinus Serif");
    }

    #[test]
    fn document_script_comes_first_among_cjk_fonts() {
        let fonts = text_fonts(Some("zh-Hant"));
        if !has_cjk_font(&fonts) {
            // No CJK fonts on this machine; nothing to order.
            return;
        }
        let first_cjk = &fonts[1];
        let script_of = |f: &str| {
            if TRADITIONAL.families.contains(&f) {
                "tc"
            } else if SIMPLIFIED.families.contains(&f) {
                "sc"
            } else {
                "other"
            }
        };
        let any_tc_installed = fonts
            .iter()
            .any(|f| TRADITIONAL.families.contains(&f.as_str()));
        if any_tc_installed {
            assert_eq!(script_of(first_cjk), "tc", "{fonts:?}");
        }
    }

    #[test]
    fn cjk_family_matches_the_pdf_path_once_fonts_are_scanned() {
        LazyLock::force(&STORE);
        for lang in [None, Some("zh-Hant"), Some("ja"), Some("ko")] {
            let fonts = text_fonts(lang);
            let first_cjk = fonts.get(1).map_or(scripts(lang)[0].stock, String::as_str);
            assert_eq!(cjk_family(lang), first_cjk, "{lang:?}");
        }
    }

    #[test]
    fn stock_families_are_listed_for_their_script() {
        for script in [SIMPLIFIED, TRADITIONAL, JAPANESE, KOREAN] {
            assert!(script.families.contains(&script.stock), "{}", script.stock);
        }
    }
}

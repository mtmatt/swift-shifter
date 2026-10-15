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

/// CJK families most preferred first, per script. Serif faces lead to match
/// Libertinus Serif. Covers macOS, Windows and common Linux packages.
const SIMPLIFIED: &[&str] = &[
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
];
const TRADITIONAL: &[&str] = &[
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
];
const JAPANESE: &[&str] = &[
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
];
const KOREAN: &[&str] = &[
    "Noto Serif CJK KR",
    "Source Han Serif KR",
    "AppleMyungjo",
    "Batang",
    "Noto Sans CJK KR",
    "Apple SD Gothic Neo",
    "Malgun Gothic",
    "NanumGothic",
];

/// The `text(font: …)` list for a document in `lang`: the Latin font, then
/// the installed CJK families, the document's own script first so typst
/// picks matching glyph shapes, then the rest as fallbacks.
pub fn text_fonts(lang: Option<&str>) -> Vec<String> {
    let lang = lang.unwrap_or("").to_ascii_lowercase();
    let scripts: [&[&str]; 4] = if lang.starts_with("ja") {
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
    };
    let installed = |family: &&str| {
        store()
            .book()
            .select_family(&family.to_lowercase())
            .next()
            .is_some()
    };
    std::iter::once(LATIN)
        .chain(
            scripts
                .iter()
                .flat_map(|list| list.iter().copied())
                .filter(installed),
        )
        .map(str::to_string)
        .collect()
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
            if TRADITIONAL.contains(&f) {
                "tc"
            } else if SIMPLIFIED.contains(&f) {
                "sc"
            } else {
                "other"
            }
        };
        let any_tc_installed = fonts.iter().any(|f| TRADITIONAL.contains(&f.as_str()));
        if any_tc_installed {
            assert_eq!(script_of(first_cjk), "tc", "{fonts:?}");
        }
    }
}

//! How alike two PDFs' text is, as `pdftotext` gives it: 1 for the same
//! words in the same order, down to 0 for nothing in common.

/// 1 − the word-level edit distance between `a` and `b` (see [`words`]),
/// over the longer one's words; 1 for two with no words.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (words(a), words(b));
    let longer = a.len().max(b.len());
    if longer == 0 {
        return 1.0;
    }
    1.0 - edit_distance(&a, &b) as f64 / longer as f64
}

/// `text`'s words: ligatures are their letters, and each CJK character is
/// a word of its own, as the text between them has no spaces. Hyphens
/// are left out, so that a word broken across lines with one is joined,
/// and reads the same as where the line didn't break it: which hyphen at
/// a line's end is the word's own (`two-` `column`) and which a break's
/// (`con-` `version`) isn't known.
pub fn words(text: &str) -> Vec<String> {
    let joined = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace("-\n", "")
        .replace('-', "");
    let mut words = Vec::new();
    for chunk in joined.split_whitespace() {
        let mut word = String::new();
        for c in chunk.chars() {
            if is_cjk(c) {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                words.push(c.to_string());
            } else {
                match ligature(c) {
                    Some(letters) => word.push_str(letters),
                    None => word.push(c),
                }
            }
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
}

/// The letters `c` is a ligature of, for those PDF text keeps.
fn ligature(c: char) -> Option<&'static str> {
    Some(match c {
        'ﬀ' => "ff",
        'ﬁ' => "fi",
        'ﬂ' => "fl",
        'ﬃ' => "ffi",
        'ﬄ' => "ffl",
        _ => return None,
    })
}

/// Han, kana, Hangul and CJK punctuation.
fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3000}'..='\u{303f}'
        | '\u{3040}'..='\u{30ff}'
        | '\u{3400}'..='\u{4dbf}'
        | '\u{4e00}'..='\u{9fff}'
        | '\u{ac00}'..='\u{d7af}'
        | '\u{f900}'..='\u{faff}'
        | '\u{ff00}'..='\u{ffef}'
        | '\u{20000}'..='\u{2fa1f}')
}

/// Levenshtein distance between `a` and `b`, a word a step, in two rows.
fn edit_distance(a: &[String], b: &[String]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let substituted = diagonal + usize::from(x != y);
            diagonal = row[j + 1];
            row[j + 1] = substituted.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

#[test]
fn words_are_as_read() {
    assert_eq!(
        words("con-\nversion  of\nthe ﬁrst"),
        ["conversion", "of", "the", "first"]
    );
    // A hyphenated word reads the same, broken across lines or not.
    assert_eq!(words("two-column"), ["twocolumn"]);
    assert_eq!(words("two-\ncolumn"), ["twocolumn"]);
    assert!(words(" - ").is_empty());
    assert_eq!(words("中文 text混排"), ["中", "文", "text", "混", "排"]);
    assert!(words(" \n ").is_empty());
}

#[test]
fn similarity_is_one_less_the_share_of_words_edited() {
    assert_eq!(similarity("a b c d", "a b c d"), 1.0);
    assert_eq!(similarity("", ""), 1.0);
    assert_eq!(similarity("a b", ""), 0.0);
    assert_eq!(similarity("a b", "c d"), 0.0);
    // One word of four replaced, and one dropped.
    assert_eq!(similarity("a b c d", "a x c d"), 0.75);
    assert_eq!(similarity("a b c d", "a c d"), 0.75);
    // Whitespace and line breaks don't count, nor where a line breaks a
    // word, its own hyphen's or a break's.
    assert_eq!(similarity("a b\nc", "a  b c"), 1.0);
    assert_eq!(similarity("two-\ncolumn", "two-column"), 1.0);
    assert_eq!(similarity("con-\nversion", "conversion"), 1.0);
    // Whatever the line breaks are.
    assert_eq!(similarity("con-\r\nversion", "conversion"), 1.0);
    assert_eq!(similarity("con-\rversion", "conversion"), 1.0);
}

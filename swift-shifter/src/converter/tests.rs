#[cfg(test)]
mod tests {
    use regex::Regex;

    #[test]
    fn test_regex() {
        let html = "
<a name=9></a>9<br/>
10<br/>
fn main() {<br/>
    let dat = read_to_string(\"input.dat\").expect(\"[Error] Fail to<br/>
11<br/>
read dat file.\");<br/>
12<br/>
    let dat: Vec&lt;f64&gt; = dat<br/>
13<br/>
        .split_whitespace()<br/>
14<br/>
        .into_iter()<br/>
15<br/>
        .map(|x| x.parse().unwrap())<br/>
16<br/>
";

        // Step 1: Remove isolated line numbers that break the flow.
        // A line number is a line with just optional whitespace, an optional anchor, and digits,
        // followed by `<br/>` and a newline.
        let re_num = Regex::new(r"(?m)^\s*(?:<a name=\d+></a>)?\s*\d+\s*<br/>\s*\r?\n?").unwrap();
        let html_no_nums = re_num.replace_all(html, "").to_string();

        // Step 2: Merge lines that wrap sentences.
        let re_merge =
            Regex::new(r"([a-zA-Z,\-]|&#160;)\s*<br/>\s*\r?\n?\s*(?:&#160;)*([a-z])").unwrap();
        let result = re_merge.replace_all(&html_no_nums, "${1} ${2}").to_string();

        println!("{}", result);

        assert!(result.contains("Fail to read dat file."));
        assert!(result.contains("let dat: Vec&lt;f64&gt; = dat<br/>\n.split_whitespace()"));
        assert!(!result.contains("11"));
        assert!(!result.contains("12"));
    }
}

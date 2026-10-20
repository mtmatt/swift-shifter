//! HTML → XHTML. EPUB content must be well-formed XML, so chapters rendered
//! by the HTML writer (raw HTML from the source included) are parsed with
//! html5ever and written back out here: void elements self-closed, every tag
//! balanced, MathML and SVG in their namespaces. The LaTeX path rewrites the
//! URLs of the raw HTML its writer reads the same way (see [`rewrite_urls`]),
//! but keeps every element.
//!
//! This guarantees well-formed XML, not valid EPUB: obsolete markup in raw
//! HTML (`<font>`, `<center>`, `<a name>`) passes through as written.

use scraper::{ElementRef, Html, Node};

const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";
const MATHML_NS: &str = "http://www.w3.org/1998/Math/MathML";
const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// HTML elements that never have content or an end tag.
pub(crate) const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// Elements left out of EPUB content (see [`serialize`]): scripts and
/// styles (EPUB readers may not run or honour them), head-only elements
/// (`<base>` would redirect every relative URL in the chapter), and elements
/// that load resources we don't package (embeds, remote stylesheets,
/// `<picture>` sources).
const DROPPED: &[&str] = &[
    "script", "style", "noscript", "iframe", "object", "embed", "video", "audio", "template",
    "base", "meta", "link", "source", "title",
];

/// Attributes left out: `srcset` names images we don't package.
const DROPPED_ATTRS: &[&str] = &["srcset"];

/// How URLs in the content map into the EPUB, or to the images copied
/// beside LaTeX output.
pub(crate) struct Hooks<'a> {
    /// The packaged path for an image `src`, or `None` to replace the image
    /// with its alt text (remote or missing images).
    pub image: &'a mut dyn FnMut(&str) -> Option<String>,
    /// The `href` to write for a link, or `None` to keep only the link's
    /// text (targets that don't exist inside the book).
    pub link: &'a dyn Fn(&str) -> Option<String>,
}

/// What the manifest needs to declare about a chapter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Features {
    pub mathml: bool,
    pub svg: bool,
}

/// Parse an HTML fragment (a chapter body).
pub(crate) fn parse(fragment: &str) -> Html {
    Html::parse_fragment(fragment)
}

/// The `id`s defined in a parsed fragment, in document order.
pub(crate) fn ids(html: &Html) -> Vec<String> {
    html.root_element()
        .descendants()
        .filter_map(ElementRef::wrap)
        .filter_map(|el| el.value().id().map(str::to_string))
        .collect()
}

/// The fragment's content as XHTML for EPUB, without the elements EPUB
/// content leaves out ([`DROPPED`]), and the features it uses.
pub(crate) fn serialize(html: &Html, hooks: &mut Hooks) -> (String, Features) {
    let mut writer = Serializer {
        out: String::new(),
        features: Features::default(),
        hooks,
        dropped: DROPPED,
    };
    writer.children(html.root_element(), XHTML_NS);
    (writer.out, writer.features)
}

/// The HTML `fragment` with its image and link URLs rewritten by `hooks`,
/// every element kept, for HTML that's read again: the raw HTML the LaTeX
/// writer reads, which shows what the HTML reader keeps (the fallback text
/// of a `<video>`, say).
pub(crate) fn rewrite_urls(fragment: &str, hooks: &mut Hooks) -> String {
    let mut writer = Serializer {
        out: String::new(),
        features: Features::default(),
        hooks,
        dropped: &[],
    };
    writer.children(parse(fragment).root_element(), XHTML_NS);
    writer.out
}

struct Serializer<'h, 'a> {
    out: String,
    features: Features,
    hooks: &'h mut Hooks<'a>,
    /// The elements left out, with their content.
    dropped: &'static [&'static str],
}

impl Serializer<'_, '_> {
    fn children(&mut self, el: ElementRef, parent_ns: &str) {
        for child in el.children() {
            match child.value() {
                Node::Text(text) => escape_text_into(&mut self.out, text),
                Node::Element(_) => {
                    if let Some(child) = ElementRef::wrap(child) {
                        self.element(child, parent_ns);
                    }
                }
                _ => {}
            }
        }
    }

    fn element(&mut self, el: ElementRef, parent_ns: &str) {
        let element = el.value();
        let name = element.name();
        let ns: &str = &element.name.ns;
        if ns == XHTML_NS && self.dropped.contains(&name) {
            return;
        }
        // `<o:p>` from Word and the like: no XML name we could write, so
        // keep only the content.
        if !is_xml_name(name) {
            self.children(el, parent_ns);
            return;
        }

        // Images and links resolve first: either may turn the element into
        // plain text.
        let url_attr = match (ns, name) {
            (XHTML_NS, "img") => Some("src"),
            (SVG_NS, "image") => Some("href"),
            (XHTML_NS, "a") => Some("href"),
            _ => None,
        };
        let mut url = None;
        if let Some(attr) = url_attr
            && let Some(value) = element
                .attrs
                .iter()
                .find(|(q, _)| &*q.local == attr)
                .map(|(_, v)| v.to_string())
        {
            if name == "a" {
                match (self.hooks.link)(&value) {
                    Some(href) => url = Some(href),
                    None => {
                        self.children(el, parent_ns);
                        return;
                    }
                }
            } else {
                match (self.hooks.image)(&value) {
                    Some(src) => url = Some(src),
                    None => {
                        let alt = element.attr("alt").unwrap_or("image");
                        escape_text_into(&mut self.out, &format!("[{alt}]"));
                        return;
                    }
                }
            }
        }

        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut uses_xlink = false;
        for (qual, value) in &element.attrs {
            let local: &str = &qual.local;
            let attr_name = match &*qual.ns {
                "" => local.to_string(),
                XLINK_NS => {
                    uses_xlink = true;
                    format!("xlink:{local}")
                }
                XML_NS => format!("xml:{local}"),
                // Namespace declarations are written below, as needed.
                _ => continue,
            };
            let scripting = local.starts_with("on");
            if scripting || DROPPED_ATTRS.contains(&local) || !is_xml_attr_name(&attr_name) {
                continue;
            }
            let value = match &url {
                Some(url) if Some(local) == url_attr => url.clone(),
                _ => value.to_string(),
            };
            attrs.push((attr_name, value));
        }

        self.out.push('<');
        self.out.push_str(name);
        if ns != parent_ns {
            match ns {
                MATHML_NS => self.features.mathml = true,
                SVG_NS => self.features.svg = true,
                _ => {}
            }
            self.out.push_str(&format!(" xmlns=\"{ns}\""));
        }
        if uses_xlink {
            self.out.push_str(&format!(" xmlns:xlink=\"{XLINK_NS}\""));
        }
        for (attr, value) in &attrs {
            self.out
                .push_str(&format!(" {attr}=\"{}\"", escape_attr(value)));
        }
        if ns == XHTML_NS && VOID.contains(&name) {
            self.out.push_str("/>");
            return;
        }
        self.out.push('>');
        self.children(el, ns);
        self.out.push_str(&format!("</{name}>"));
    }
}

/// A name XML can write without a namespace prefix.
fn is_xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// `epub:type`, `xml:` and `xlink:` attributes, and plain XML names other
/// than namespace declarations; anything else (`@click`, `x"y`) can't be
/// written as an XML attribute.
fn is_xml_attr_name(name: &str) -> bool {
    if matches!(name, "epub:type" | "xml:lang" | "xml:space") || name.starts_with("xlink:") {
        return true;
    }
    is_xml_name(name) && !name.starts_with("xmlns")
}

/// Escape XML text, dropping characters XML 1.0 can't contain (most
/// control characters).
pub(crate) fn escape_text_into(out: &mut String, text: &str) {
    escape_into(out, text, false);
}

pub(crate) fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text, false);
    out
}

/// [`escape_text`], plus `"` for use inside a double-quoted attribute.
pub(crate) fn escape_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(&mut out, text, true);
    out
}

fn escape_into(out: &mut String, text: &str, quotes: bool) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if quotes => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_xhtml(html: &str) -> (String, Features) {
        let mut image = |src: &str| (!src.starts_with("http")).then(|| format!("images/{src}"));
        let link = |href: &str| {
            (href.starts_with('#') || href.starts_with("https:"))
                .then(|| href.replace("#a", "ch2.xhtml#a"))
        };
        let mut hooks = Hooks {
            image: &mut image,
            link: &link,
        };
        serialize(&parse(html), &mut hooks)
    }

    fn assert_well_formed(xhtml: &str) {
        let wrapped = format!(
            "<body xmlns=\"{XHTML_NS}\" xmlns:epub=\"http://www.idpf.org/2007/ops\">{xhtml}</body>"
        );
        roxmltree::Document::parse(&wrapped).unwrap_or_else(|e| panic!("{e}: {xhtml}"));
    }

    #[test]
    fn balances_and_self_closes() {
        let (out, _) = to_xhtml("<p>a<br>b<sup>2<hr><input type=checkbox disabled checked>");
        assert_eq!(
            out,
            "<p>a<br/>b<sup>2</sup></p><hr/><input checked=\"\" disabled=\"\" type=\"checkbox\"/>"
        );
        assert_well_formed(&out);
    }

    #[test]
    fn math_and_svg_get_namespaces() {
        let (out, features) = to_xhtml(
            "<p><math display=\"block\"><mi>x</mi></math></p>\
             <svg viewBox=\"0 0 1 1\"><image xlink:href=\"a.png\"/></svg>",
        );
        assert!(
            out.contains(&format!(
                "<math xmlns=\"{MATHML_NS}\" display=\"block\"><mi>x</mi></math>"
            )),
            "{out}"
        );
        assert!(
            out.contains(&format!(
                "<svg xmlns=\"{SVG_NS}\" viewBox=\"0 0 1 1\"><image xmlns:xlink=\"{XLINK_NS}\" xlink:href=\"images/a.png\"></image></svg>"
            )),
            "{out}"
        );
        assert_eq!(
            features,
            Features {
                mathml: true,
                svg: true
            }
        );
        assert_well_formed(&out);
    }

    #[test]
    fn rewrites_images_and_links() {
        let (out, _) = to_xhtml(
            "<p><img src=\"x.png\" alt=\"X\" srcset=\"x2.png 2x\"> \
             <img src=\"https://e.org/y.png\" alt=\"Y &amp; Z\"> \
             <a href=\"#a1\">go</a> <a href=\"other.md\">local</a> \
             <a href=\"javascript:alert(1)\">js</a></p>",
        );
        assert_eq!(
            out,
            "<p><img alt=\"X\" src=\"images/x.png\"/> [Y &amp; Z] \
             <a href=\"ch2.xhtml#a1\">go</a> local js</p>"
        );
    }

    #[test]
    fn drops_scripts_bad_names_and_control_characters() {
        let (out, _) = to_xhtml(
            "<div @click=\"x\" onclick=\"y()\" data-ok=\"1\" epub:type=\"noteref\" xmlns=\"urn:x\">\
             a\u{1}b<script>alert(1)</script><o:p>word</o:p></div>\
             <base href=\"https://e.org/\"><meta http-equiv=\"refresh\" content=\"0\">\
             <link rel=\"stylesheet\" href=\"https://e.org/s.css\">\
             <picture><source srcset=\"a.webp\"><img src=\"a.png\" alt=\"A\"></picture>",
        );
        assert_eq!(
            out,
            "<div data-ok=\"1\" epub:type=\"noteref\">abword</div>\
             <picture><img alt=\"A\" src=\"images/a.png\"/></picture>"
        );
        assert_well_formed(&out);
    }

    #[test]
    fn collects_ids() {
        let html = parse("<h1 id=\"a\">A</h1><p><span id=\"b\">x</span></p>");
        assert_eq!(ids(&html), ["a", "b"]);
    }
}

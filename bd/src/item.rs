//! Turn one definition into the fields we index and display.
use crate::text;
use scraper::node::Element;
use scraper::{Html, Node, Selector};

pub const SUBTITLE_MAX_CHARS: usize = 300;
pub const FULLTEXT_MAX_CHARS: usize = 4000;
/// Only the start of an entry is parsed for the result line (entries reach 450 KB).
const DISPLAY_PARSE_MAX_BYTES: usize = 32 * 1024;
/// The Text View shows the whole entry, up to this much source.
const TEXT_PARSE_MAX_BYTES: usize = 512 * 1024;
const BLOCK_TAGS: [&str; 9] = ["div", "p", "br", "li", "tr", "h1", "h2", "h3", "d:entry"];
const PARAGRAPH_CLASSES: [&str; 8] = [
    "hg",
    "se1",
    "subEntry",
    "subEntryBlock",
    "etym",
    "x_xoLblBlk",
    "feature",
    "feature-caption",
];
const LINE_CLASSES: [&str; 5] = ["posg", "se2", "se3", "feature-title", "list"];

fn text_of<'a>(it: impl Iterator<Item = &'a str>) -> String {
    it.collect()
}

fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn base16(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02X}")).collect()
}

pub struct Sel {
    def: Selector,
    forms: Selector,
    ipa_solitary: Selector,
    ipa: Selector,
}

impl Sel {
    pub fn new() -> Self {
        let p = |s: &str| Selector::parse(s).unwrap();
        Sel {
            def: p(r#"[d\:def="1"]"#),
            forms: p(".infg"),
            ipa_solitary: p(r#"[d\:prn="IPA solitary"]"#),
            ipa: p(r#"[d\:prn="IPA"]"#),
        }
    }
}

/// What the index stores per entry. The subtitle and pronunciation are *not*
/// stored (they are derived from the definition for the few hits shown).
#[derive(Clone, Debug, Default)]
pub struct DocRec {
    pub title: String,
    pub forms: String,
    /// file name of the cached html (`<hex>.html`)
    pub name: String,
    /// position of this definition's gzip member in `defs.pack`
    pub off: u64,
    pub len: u64,
}

/// A document ready to be added to the index builder.
pub struct Prepared {
    pub rec: DocRec,
    /// unique, sorted search terms of the (truncated) full text
    pub full_terms: Vec<String>,
}

pub fn prepare(sel: &Sel, word: &str, name: &str, definition: &str) -> Prepared {
    let doc = Html::parse_fragment(definition);
    let fulltext = trunc(&text_of(doc.root_element().text()), FULLTEXT_MAX_CHARS);
    let forms = doc
        .select(&sel.forms)
        .next()
        .map(|e| text_of(e.text()))
        .unwrap_or_default();
    let mut full_terms: Vec<String> = text::tokens(&fulltext)
        .into_iter()
        .filter(|t| t.len() <= 40)
        .collect();
    full_terms.sort_unstable();
    full_terms.dedup();
    Prepared {
        rec: DocRec {
            title: word.to_string(),
            forms,
            name: name.to_string(),
            off: 0,
            len: 0,
        },
        full_terms,
    }
}

/// The result line shown under the title, plus the IPA pronunciation (if any).
pub fn display(sel: &Sel, definition: &str) -> (String, String) {
    let mut end = definition.len().min(DISPLAY_PARSE_MAX_BYTES);
    while !definition.is_char_boundary(end) {
        end -= 1;
    }
    let definition = &definition[..end];
    let doc = Html::parse_fragment(definition);
    let mut snippet = text_of(doc.root_element().text());
    if let Some(e) = doc.select(&sel.def).next() {
        snippet = text_of(e.text());
    }
    let snippet = trunc(&snippet, SUBTITLE_MAX_CHARS);
    let forms = doc
        .select(&sel.forms)
        .next()
        .map(|e| text_of(e.text()))
        .unwrap_or_default();
    let mut subtitle = trunc(&format!("{forms}{snippet}"), SUBTITLE_MAX_CHARS);
    let mut pron = String::new();
    let ipa = doc
        .select(&sel.ipa_solitary)
        .next()
        .or_else(|| doc.select(&sel.ipa).next());
    if let Some(e) = ipa {
        pron = text_of(e.text())
            .split(',')
            .next()
            .unwrap_or("")
            .to_string();
        subtitle = format!("[⌘: 🗣] {subtitle}");
    }
    (subtitle, pron)
}

/// Plain text of a definition page for Alfred's Text View, one sense per line.
pub fn plain_text(page: &str) -> String {
    let mut end = page.len().min(TEXT_PARSE_MAX_BYTES);
    while !page.is_char_boundary(end) {
        end -= 1;
    }
    let doc = Html::parse_fragment(&page[..end]);
    let mut raw = String::new();
    for node in doc.root_element().descendants() {
        match node.value() {
            Node::Text(t)
                if !node
                    .parent()
                    .and_then(|p| p.value().as_element())
                    .is_some_and(is_script) =>
            {
                // a whitespace-only node is source formatting; a newline inside text separates words
                let blank = t.trim().is_empty();
                raw.extend(t.chars().filter(|&c| !(blank && c == '\n')).map(|c| {
                    if c == '\n' {
                        ' '
                    } else {
                        c
                    }
                }));
            }
            Node::Element(e) => raw.push_str(break_before(e)),
            _ => {}
        }
    }
    let mut out = String::new();
    let mut gap = false;
    for line in raw.split('\n') {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            gap = !out.is_empty();
        } else {
            if gap {
                out.push('\n');
                gap = false;
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn is_script(e: &Element) -> bool {
    matches!(e.name(), "style" | "script")
}

/// What the entry's markup implies before an element: a blank line before headwords and sections,
/// a line break before senses, a space between adjacent pronunciations.
fn break_before(e: &Element) -> &'static str {
    let has = |names: &[&str]| e.classes().any(|c| names.contains(&c));
    if has(&PARAGRAPH_CLASSES) {
        "\n\n"
    } else if BLOCK_TAGS.contains(&e.name())
        || has(&LINE_CLASSES)
        || (has(&["msDict"]) && !has(&["t_first"]))
    {
        "\n"
    } else if has(&["ph"]) {
        " "
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_puts_each_sense_on_its_own_line() {
        let page = "<body><d:entry d:title=\"happy\"><span class=\"hg\"><span class=\"hw\">hap<span class=\"hsb\">\n</span>py </span></span>\n<span class=\"sg\"><span class=\"se1\"><span class=\"pos\">adjective </span>\n<span class=\"se2\"><span class=\"sn\"> 1 </span><span class=\"df\">feeling   pleasure</span></span><span class=\"se2\"><span class=\"sn\"> 2 </span><span class=\"df\">fortunate</span></span></span></span></d:entry></body>";
        assert_eq!(
            plain_text(page),
            "happy\n\nadjective\n1 feeling pleasure\n2 fortunate\n"
        );
    }

    #[test]
    fn plain_text_keeps_a_sense_number_with_its_first_definition() {
        let page = "<span class=\"hg\">happy<span class=\"ph t_respell\">ˈhapē</span><span class=\"ph t_IPA\">ˈhæpi</span></span><span class=\"se2\"><span class=\"sn\">1 </span><span class=\"msDict t_first\">pleased</span><span class=\"msDict t_subsense\">• glad</span></span><span class=\"x_xoLblBlk tg_etym\">ORIGIN</span><span class=\"etym\">Middle English</span>";
        assert_eq!(
            plain_text(page),
            "happy ˈhapē ˈhæpi\n1 pleased\n• glad\n\nORIGIN\n\nMiddle English\n"
        );
    }

    #[test]
    fn plain_text_puts_list_items_and_captions_on_their_own_lines() {
        let page = "<span class=\"feature t_wordLinks\"><span class=\"feature-title\">WORD LINKS </span><span class=\"feature-body\"><span class=\"list\"><span class=\"item\"><span class=\"xr\">tribo-</span>: <br/>related prefix</span></span><span class=\"list\"><span class=\"item\">tribology: <br/>study of friction</span></span></span><span class=\"feature-caption\">Word Links sections</span></span>";
        assert_eq!(
            plain_text(page),
            "WORD LINKS\ntribo-:\nrelated prefix\ntribology:\nstudy of friction\n\nWord Links sections\n"
        );
    }

    #[test]
    fn plain_text_treats_a_newline_between_words_as_a_space() {
        assert_eq!(
            plain_text("<p>line one\nline two</p>"),
            "line one line two\n"
        );
        assert_eq!(plain_text("<span>a</span>\n<span>b</span>"), "ab\n");
    }

    #[test]
    fn plain_text_drops_script_and_style_text() {
        assert_eq!(
            plain_text("<style>.x{}</style><p>hi</p><script>var a;</script>"),
            "hi\n"
        );
    }

    #[test]
    fn plain_text_ignores_markup_only_input_and_cuts_huge_pages_safely() {
        assert_eq!(plain_text("<div><br></div>"), "");
        let big = format!("<p>a</p><p>{}</p>", "é".repeat(TEXT_PARSE_MAX_BYTES));
        assert!(plain_text(&big).starts_with("a\n"));
    }

    #[test]
    fn display_reads_only_the_start_of_huge_entries() {
        let sel = Sel::new();
        // multi-byte text straddling the cut must not panic
        let filler = "é".repeat(DISPLAY_PARSE_MAX_BYTES);
        let (sub, _) = display(&sel, &format!(r#"<span d:def="1">early</span>{filler}"#));
        assert!(sub.starts_with("early"));
        // a definition beyond the cut is not parsed
        let (sub, _) = display(&sel, &format!(r#"{filler}<span d:def="1">late</span>"#));
        assert!(!sub.contains("late"));
    }
}

//! Turn one definition into the fields we index and display.
use crate::text;
use scraper::{Html, Selector};

pub const SUBTITLE_MAX_CHARS: usize = 300;
pub const FULLTEXT_MAX_CHARS: usize = 4000;

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
    let forms = doc.select(&sel.forms).next().map(|e| text_of(e.text())).unwrap_or_default();
    let mut full_terms: Vec<String> = text::tokens(&fulltext).into_iter().filter(|t| t.len() <= 40).collect();
    full_terms.sort_unstable();
    full_terms.dedup();
    Prepared { rec: DocRec { title: word.to_string(), forms, name: name.to_string(), off: 0, len: 0 }, full_terms }
}

/// The result line shown under the title, plus the IPA pronunciation (if any).
pub fn display(sel: &Sel, definition: &str) -> (String, String) {
    let doc = Html::parse_fragment(definition);
    let mut snippet = text_of(doc.root_element().text());
    if let Some(e) = doc.select(&sel.def).next() {
        snippet = text_of(e.text());
    }
    let snippet = trunc(&snippet, SUBTITLE_MAX_CHARS);
    let forms = doc.select(&sel.forms).next().map(|e| text_of(e.text())).unwrap_or_default();
    let mut subtitle = trunc(&format!("{forms}{snippet}"), SUBTITLE_MAX_CHARS);
    let mut pron = String::new();
    let ipa = doc.select(&sel.ipa_solitary).next().or_else(|| doc.select(&sel.ipa).next());
    if let Some(e) = ipa {
        pron = text_of(e.text()).split(',').next().unwrap_or("").to_string();
        subtitle = format!("[⌘: 🗣] {subtitle}");
    }
    (subtitle, pron)
}

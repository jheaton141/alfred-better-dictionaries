//! Build the search document for one definition (same shape as the Python
//! importer produced, so existing workflows/lookup scripts keep working).
use scraper::{Html, Selector};
use serde_json::{json, Value};

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

/// `off`/`len` are filled in by the caller once the pack position is known.
pub fn make_item(sel: &Sel, word: &str, filename: &str, definition: &str, html_path: &str) -> Value {
    let doc = Html::parse_fragment(definition);
    let fulltext_all = text_of(doc.root_element().text());
    let mut snippet = fulltext_all.clone();
    if let Some(e) = doc.select(&sel.def).next() {
        snippet = text_of(e.text());
    }
    let snippet = trunc(&snippet, SUBTITLE_MAX_CHARS);
    let fulltext = trunc(&fulltext_all, FULLTEXT_MAX_CHARS);
    let forms = doc.select(&sel.forms).next().map(|e| text_of(e.text())).unwrap_or_default();

    let mut item = json!({
        "arg": html_path,
        "title": word,
        "id": filename.split('.').next().unwrap(),
        "forms": forms,
        "subtitle": trunc(&format!("{forms}{snippet}"), SUBTITLE_MAX_CHARS),
        "fulltext": fulltext,
        "quicklookurl": html_path,
    });

    let ipa = doc.select(&sel.ipa_solitary).next().or_else(|| doc.select(&sel.ipa).next());
    if let Some(e) = ipa {
        let t = text_of(e.text());
        let pron = t.split(',').next().unwrap_or("").to_string();
        let sub = item["subtitle"].as_str().unwrap().to_string();
        item["subtitle"] = Value::String(format!("[⌘: 🗣] {sub}"));
        item["mods"] = json!({"cmd": {"arg": pron, "subtitle": format!("🗣 {pron}")}});
    }
    item
}

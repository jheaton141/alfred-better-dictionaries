//! Text normalisation shared by the indexer and the query engine.
use unicode_normalization::UnicodeNormalization;

/// Lower-cased alphanumeric words with diacritics removed ("Café-au-lait" ->
/// ["cafe", "au", "lait"]). Indexing and querying must use the same function.
pub fn tokens(s: &str) -> Vec<String> {
    // titles are stored as in the dictionary's XML: &amp; must not index as "amp"
    let unescaped;
    let s = if s.contains('&') {
        unescaped = s
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&#39;", "'");
        unescaped.as_str()
    } else {
        s
    };
    let mut out = vec![];
    let mut cur = String::new();
    for c in s.nfd() {
        if ('\u{0300}'..='\u{036F}').contains(&c) {
            continue; // combining mark
        }
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folds() {
        assert_eq!(
            tokens("Café-au-lait, N°5"),
            vec!["cafe", "au", "lait", "n", "5"]
        );
        assert_eq!(tokens("  "), Vec::<String>::new());
        assert_eq!(tokens("Zoë &amp; co"), vec!["zoe", "co"]);
    }
}

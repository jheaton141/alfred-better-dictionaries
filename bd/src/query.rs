//! Query engine over a memory-mapped [`Index`].
//!
//! Ranking, best first (ties: shorter title, then dictionary order):
//!   100 title is exactly the query
//!    90 the query is one of the entry's inflected forms
//!    80 title starts with the query
//!    70 every query word is a word of the title (last one may be a prefix)
//!    60 every query word is a word of title or forms
//!    50 same, allowing typos
//!    40 every query word occurs in the definition text
//!    20+ some of the query words occur (only when nothing better exists)
use crate::index::{fold_key, Index, Terms};
use crate::text::tokens;
use std::collections::HashMap;

pub const T_EXACT: u64 = 100;
pub const T_FORM: u64 = 90;
pub const T_PREFIX: u64 = 80;
pub const T_TITLE_WORDS: u64 = 70;
pub const T_HEAD_WORDS: u64 = 60;
pub const T_TYPO: u64 = 50;
pub const T_FULL: u64 = 40;
pub const T_RELAXED: u64 = 20;

const PREFIX_SCAN_CAP: usize = 4000;
const TERM_EXPANSION_CAP: usize = 300;
const CANDIDATE_CAP: usize = 5000;
/// Query words beyond this are ignored (a pasted paragraph must not take seconds).
const MAX_QUERY_WORDS: usize = 12;

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub doc: u32,
    /// Higher is better; comparable across dictionaries.
    pub key: u64,
}

pub fn tier_of(key: u64) -> u64 {
    key >> 48
}

fn mk_key(tier: u64, title_chars: usize, doc: u32) -> u64 {
    (tier << 48) | ((0xFFFF - title_chars.min(0xFFFF)) as u64) << 32 | (u32::MAX - doc) as u64
}

fn put(best: &mut HashMap<u32, u64>, doc: u32, key: u64) {
    let e = best.entry(doc).or_insert(0);
    if key > *e {
        *e = key;
    }
}

fn all_match(qt: &[String], toks: &[String]) -> bool {
    let last = qt.len() - 1;
    qt.iter().enumerate().all(|(j, q)| {
        if j == last {
            toks.iter().any(|t| t.starts_with(q.as_str()))
        } else {
            toks.iter().any(|t| t == q)
        }
    })
}

fn contains_seq(hay: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Tier a document earns for the query (whatever route found it).
fn classify(ix: &Index, doc: u32, qt: &[String], qn: &str) -> u64 {
    let d = ix.doc(doc);
    let tt = tokens(d.title);
    let tn = tt.join(" ");
    let tier = if tn == qn {
        T_EXACT
    } else {
        let ft = tokens(d.forms);
        if contains_seq(&ft, qt) {
            T_FORM
        } else if tn.starts_with(qn) {
            T_PREFIX
        } else if all_match(qt, &tt) {
            T_TITLE_WORDS
        } else {
            T_HEAD_WORDS
        }
    };
    mk_key(tier, tn.chars().count(), doc)
}

/// Sorted, de-duplicated docs for one query word as `(doc, in_title)`.
fn head_docs(t: &Terms, tok: &str, prefix: bool) -> Vec<(u32, bool)> {
    let (lo, hi) = if prefix {
        let (lo, hi) = t.prefix_range(tok.as_bytes());
        (lo, hi.min(lo + TERM_EXPANSION_CAP))
    } else {
        match t.exact(tok.as_bytes()) {
            Some(i) => (i, i + 1),
            None => (0, 0),
        }
    };
    collect_head(t, lo..hi)
}

fn collect_head(t: &Terms, terms: impl Iterator<Item = usize>) -> Vec<(u32, bool)> {
    let mut v: Vec<(u32, bool)> = vec![];
    for i in terms {
        v.extend(t.postings(i).into_iter().map(|x| (x >> 1, x & 1 == 1)));
    }
    v.sort_unstable();
    let mut out: Vec<(u32, bool)> = Vec::with_capacity(v.len());
    for (d, f) in v {
        match out.last_mut() {
            Some(l) if l.0 == d => l.1 |= f,
            _ => out.push((d, f)),
        }
    }
    out
}

fn full_docs(t: &Terms, tok: &str, prefix: bool) -> Vec<u32> {
    let (lo, hi) = if prefix && tok.chars().count() >= 3 {
        let (lo, hi) = t.prefix_range(tok.as_bytes());
        (lo, hi.min(lo + TERM_EXPANSION_CAP / 3))
    } else {
        match t.exact(tok.as_bytes()) {
            Some(i) => (i, i + 1),
            None => (0, 0),
        }
    };
    let mut v: Vec<u32> = vec![];
    for i in lo..hi {
        v.extend(t.postings(i));
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// AND of per-word `(doc, in_title)` lists; in_title is the AND of the flags.
fn intersect_head(mut lists: Vec<Vec<(u32, bool)>>) -> Vec<(u32, bool)> {
    lists.sort_by_key(|l| l.len());
    let mut it = lists.into_iter();
    let mut acc = it.next().unwrap_or_default();
    for l in it {
        acc = acc
            .into_iter()
            .filter_map(|(d, f)| {
                l.binary_search_by_key(&d, |x| x.0)
                    .ok()
                    .map(|k| (d, f && l[k].1))
            })
            .collect();
        if acc.is_empty() {
            break;
        }
    }
    acc
}

/// Bounded Levenshtein distance (None if more than `k`). Words up to 64 units.
fn lev_le<T: PartialEq + Copy>(a: &[T], b: &[T], k: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > k || a.len() > 64 || b.len() > 64 {
        return None;
    }
    let mut prev = [0usize; 65];
    let mut cur = [0usize; 65];
    for (j, p) in prev.iter_mut().enumerate().take(b.len() + 1) {
        *p = j;
    }
    for i in 1..=a.len() {
        cur[0] = i;
        let mut rowmin = i;
        for j in 1..=b.len() {
            let cost = (a[i - 1] != b[j - 1]) as usize;
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            rowmin = rowmin.min(cur[j]);
        }
        if rowmin > k {
            return None;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    (prev[b.len()] <= k).then_some(prev[b.len()])
}

fn typos_allowed(tok: &str) -> usize {
    match tok.chars().count() {
        0..=4 => 0,
        5..=8 => 1,
        _ => 2,
    }
}

/// Terms within the typo budget of `tok`. For the last word, also terms whose
/// *beginning* is within budget (the user is still typing).
fn fuzzy_terms(t: &Terms, tok: &str, prefix: bool) -> Vec<usize> {
    let k = typos_allowed(tok);
    if k == 0 {
        return vec![];
    }
    let q: Vec<char> = tok.chars().collect();
    let ascii = tok.is_ascii();
    let mut out = vec![];
    for i in 0..t.n {
        let term = t.term(i);
        if term.first() != tok.as_bytes().first() {
            continue; // first letter must be right
        }
        let ok = if ascii && term.is_ascii() {
            let qb = tok.as_bytes();
            lev_le(qb, term, k).is_some()
                || (prefix && term.len() > qb.len() && lev_le(qb, &term[..qb.len()], k).is_some())
        } else {
            let Ok(s) = std::str::from_utf8(term) else {
                continue;
            };
            let tc: Vec<char> = s.chars().collect();
            lev_le(&q, &tc, k).is_some()
                || (prefix && tc.len() > q.len() && lev_le(&q, &tc[..q.len()], k).is_some())
        };
        if ok {
            out.push(i);
            if out.len() >= TERM_EXPANSION_CAP {
                break;
            }
        }
    }
    out
}

fn query_tokens(q: &str) -> Vec<String> {
    let mut qt = tokens(q);
    qt.truncate(MAX_QUERY_WORDS);
    qt
}

pub fn search(ix: &Index, q: &str, limit: usize) -> Vec<Hit> {
    let qt = query_tokens(q);
    if qt.is_empty() || ix.n == 0 {
        return vec![];
    }
    let qn = qt.join(" ");
    let last = qt.len() - 1;
    let mut best: HashMap<u32, u64> = HashMap::new();
    let head = ix.head();

    // A. whole-title prefix: a binary search in the title-sorted order.
    let start = ix.title_lower_bound(&qn);
    let mut scanned = 0;
    let mut i = start;
    while i < ix.n && scanned < PREFIX_SCAN_CAP {
        let doc = ix.sorted_doc(i);
        if !fold_key(ix.doc(doc).title).starts_with(&qn) {
            break;
        }
        put(&mut best, doc, classify(ix, doc, &qt, &qn));
        i += 1;
        scanned += 1;
    }

    // B. every query word is a word of the title/forms.
    let lists: Vec<Vec<(u32, bool)>> = qt
        .iter()
        .enumerate()
        .map(|(j, t)| head_docs(&head, t, j == last))
        .collect();
    if lists.iter().all(|l| !l.is_empty()) {
        let mut cand = intersect_head(lists);
        cand.sort_by_key(|&(_, in_title)| !in_title); // stable: title matches first
        for (doc, _) in cand.into_iter().take(CANDIDATE_CAP) {
            if !best.contains_key(&doc) {
                put(&mut best, doc, classify(ix, doc, &qt, &qn));
            }
        }
    }

    let good = |best: &HashMap<u32, u64>| {
        best.values()
            .filter(|&&k| tier_of(k) >= T_HEAD_WORDS)
            .count()
    };

    // C. typo tolerance.
    if good(&best) < limit {
        let mut lists: Vec<Vec<(u32, bool)>> = vec![];
        for (j, t) in qt.iter().enumerate() {
            let mut terms: Vec<usize> = vec![];
            let (lo, hi) = if j == last {
                head.prefix_range(t.as_bytes())
            } else {
                head.exact(t.as_bytes()).map_or((0, 0), |i| (i, i + 1))
            };
            terms.extend(lo..hi.min(lo + TERM_EXPANSION_CAP));
            terms.extend(fuzzy_terms(&head, t, j == last));
            terms.sort_unstable();
            terms.dedup();
            lists.push(collect_head(&head, terms.into_iter()));
        }
        if lists.iter().all(|l| !l.is_empty()) {
            let mut cand = intersect_head(lists);
            cand.sort_by_key(|&(_, in_title)| !in_title);
            for (doc, _) in cand.into_iter().take(CANDIDATE_CAP) {
                if !best.contains_key(&doc) {
                    let tn = fold_key(ix.doc(doc).title);
                    put(&mut best, doc, mk_key(T_TYPO, tn.chars().count(), doc));
                }
            }
        }
    }

    // D. words in the definition text.
    if best.len() < limit {
        let full = ix.full();
        let mut lists: Vec<Vec<u32>> = vec![];
        for (j, t) in qt.iter().enumerate() {
            let l = full_docs(&full, t, j == last);
            // very common words are not indexed; ignore short unknown words
            if l.is_empty() && t.len() <= 3 && qt.len() > 1 {
                continue;
            }
            lists.push(l);
        }
        if !lists.is_empty() && lists.iter().all(|l| !l.is_empty()) {
            lists.sort_by_key(|l| l.len());
            let mut it = lists.into_iter();
            let mut acc = it.next().unwrap();
            for l in it {
                acc.retain(|d| l.binary_search(d).is_ok());
            }
            for doc in acc.into_iter().take(CANDIDATE_CAP) {
                if !best.contains_key(&doc) {
                    let tn = fold_key(ix.doc(doc).title);
                    put(&mut best, doc, mk_key(T_FULL, tn.chars().count(), doc));
                }
            }
        }
    }

    // E. nothing at all: documents containing some of the words.
    if best.is_empty() && qt.len() >= 2 {
        let full = ix.full();
        let mut counts: HashMap<u32, u64> = HashMap::new();
        for (j, t) in qt.iter().enumerate() {
            let mut docs: Vec<u32> = head_docs(&head, t, j == last)
                .into_iter()
                .map(|x| x.0)
                .collect();
            docs.extend(full_docs(&full, t, j == last));
            docs.sort_unstable();
            docs.dedup();
            for d in docs.into_iter().take(CANDIDATE_CAP) {
                *counts.entry(d).or_insert(0) += 1;
            }
        }
        for (doc, c) in counts {
            let tn = fold_key(ix.doc(doc).title);
            put(
                &mut best,
                doc,
                mk_key(T_RELAXED + c, tn.chars().count(), doc),
            );
        }
    }

    let mut hits: Vec<Hit> = best
        .into_iter()
        .map(|(doc, key)| Hit { doc, key })
        .collect();
    // front matter and other untitled entries are not lookup results
    hits.retain(|h| !ix.doc(h.doc).title.trim().is_empty());
    hits.sort_unstable_by_key(|a| std::cmp::Reverse(a.key));
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Builder;
    use crate::item::{DocRec, Prepared};

    fn build(docs: &[(&str, &str, &str)]) -> (tempdir::Dir, Index) {
        let dir = tempdir::Dir::new();
        let mut b = Builder::new();
        for (i, (title, forms, body)) in docs.iter().enumerate() {
            let mut ft = tokens(body);
            ft.sort();
            ft.dedup();
            b.add(&Prepared {
                rec: DocRec {
                    title: title.to_string(),
                    forms: forms.to_string(),
                    name: format!("n{i}"),
                    off: i as u64 * 10,
                    len: 10,
                },
                full_terms: ft,
            });
        }
        let path = format!("{}/index.bin", dir.path);
        b.finish(&path).unwrap();
        let ix = Index::open(&path).unwrap();
        (dir, ix)
    }

    mod tempdir {
        pub struct Dir {
            pub path: String,
        }
        impl Dir {
            pub fn new() -> Dir {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static N: AtomicUsize = AtomicUsize::new(0);
                let path = format!(
                    "{}/bd-test-{}-{}",
                    std::env::temp_dir().display(),
                    std::process::id(),
                    N.fetch_add(1, Ordering::SeqCst)
                );
                std::fs::create_dir_all(&path).unwrap();
                Dir { path }
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
    }

    fn titles(ix: &Index, q: &str) -> Vec<String> {
        search(ix, q, 9)
            .iter()
            .map(|h| ix.doc(h.doc).title.to_string())
            .collect()
    }

    fn sample() -> (tempdir::Dir, Index) {
        build(&[
            ("run", "ran, running, runs", "move fast on foot"),
            ("runner", "runners", "a person who runs"),
            ("ranch", "ranches", "a large farm for cattle"),
            ("run-up", "", "the period before an event"),
            ("ice cream", "", "frozen dessert made from cream"),
            ("ice", "", "frozen water"),
            ("cream", "", "thick yellow white liquid from milk"),
            ("Café", "cafes", "a small restaurant serving coffee"),
            ("extraordinary", "", "very unusual or remarkable"),
            (
                "zebra",
                "",
                "an african animal with black and white stripes",
            ),
        ])
    }

    #[test]
    fn exact_beats_prefix() {
        let (_d, ix) = sample();
        let t = titles(&ix, "run");
        assert_eq!(t[0], "run");
        assert!(t.contains(&"runner".to_string()) && t.contains(&"run-up".to_string()));
        assert!(
            t.iter().position(|x| x == "runner").unwrap()
                < t.iter().position(|x| x == "run-up").unwrap()
        );
    }

    #[test]
    fn inflected_form_finds_headword() {
        let (_d, ix) = sample();
        assert_eq!(titles(&ix, "ran")[0], "run");
        assert_eq!(titles(&ix, "ranches")[0], "ranch");
    }

    #[test]
    fn prefix_and_multiword() {
        let (_d, ix) = sample();
        assert_eq!(titles(&ix, "ice cr")[0], "ice cream");
        assert_eq!(titles(&ix, "cream ice")[0], "ice cream");
        assert_eq!(titles(&ix, "ic")[0], "ice");
    }

    #[test]
    fn diacritics_and_case() {
        let (_d, ix) = sample();
        assert_eq!(titles(&ix, "cafe")[0], "Café");
        assert_eq!(titles(&ix, "CAFÉ")[0], "Café");
    }

    #[test]
    fn typos() {
        let (_d, ix) = sample();
        assert_eq!(titles(&ix, "extrordinary")[0], "extraordinary");
        assert_eq!(titles(&ix, "zebrra")[0], "zebra");
    }

    #[test]
    fn definition_text() {
        let (_d, ix) = sample();
        assert_eq!(titles(&ix, "african stripes")[0], "zebra");
        assert_eq!(titles(&ix, "frozen dessert")[0], "ice cream");
    }

    #[test]
    fn nothing() {
        let (_d, ix) = sample();
        assert!(titles(&ix, "qqqqq").is_empty());
        assert!(titles(&ix, "   ").is_empty());
    }

    #[test]
    fn untitled_entries_are_not_results() {
        let (_d, ix) = build(&[("", "", "to be or not to be preface"), ("be", "", "verb")]);
        assert_eq!(titles(&ix, "to be or not to be"), Vec::<String>::new());
        assert_eq!(titles(&ix, "be"), vec!["be"]);
    }

    #[test]
    fn long_queries_use_only_the_first_words() {
        let (_d, ix) = build(&[("be", "", "verb")]);
        let twelve = ["be"; MAX_QUERY_WORDS].join(" ");
        assert_eq!(titles(&ix, &twelve), vec!["be"]);
        let long = format!("{twelve} zzzzz yyyyy");
        assert_eq!(query_tokens(&long).len(), MAX_QUERY_WORDS);
        assert_eq!(titles(&ix, &long), vec!["be"]);
    }

    #[test]
    fn lev() {
        assert_eq!(lev_le(b"kitten", b"sitting", 3), Some(3));
        assert_eq!(lev_le(b"kitten", b"sitting", 2), None);
        assert_eq!(lev_le(b"abc", b"abc", 0), Some(0));
    }
}

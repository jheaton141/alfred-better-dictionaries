//! On-disk search index (`<dict>/index.bin`), memory-mapped at query time.
//!
//! Layout (version 2; all integers little-endian):
//!   header  : magic[8] | n_docs u32 | pad u32 | 5 x (offset u64, len u64)
//!   section 0: doc offsets   u64 x (n+1)       into section 1
//!   section 1: doc records   varint-prefixed strings + varints
//!   section 2: title order   u32 x n           doc ids sorted by folded title
//!   section 3: head terms    words of titles + inflected forms
//!   section 4: full terms    words of the definition text (common words dropped)
//!
//! A term section is: n u32 | term_off u32 x (n+1) | post_off u64 x (n+1) |
//! term bytes | postings. Terms are sorted (byte order), postings are
//! varint delta-coded.  Head postings store `doc << 1 | in_title`.
use crate::item::Prepared;
use crate::text::tokens;
use anyhow::{bail, Context, Result};
use memmap2::Mmap;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;

const MAGIC: &[u8; 8] = b"BDIDX002";
const HEADER_LEN: usize = 8 + 4 + 4 + 5 * 16;

pub fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub fn read_varint(b: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    while *pos < b.len() {
        let c = b[*pos];
        *pos += 1;
        v |= ((c & 0x7f) as u64) << shift;
        if c & 0x80 == 0 || shift >= 56 {
            break;
        }
        shift += 7;
    }
    v
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    write_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn read_str<'a>(b: &'a [u8], pos: &mut usize) -> &'a str {
    let n = read_varint(b, pos) as usize;
    let start = (*pos).min(b.len());
    let end = start.saturating_add(n).min(b.len());
    *pos = end;
    b.get(start..end)
        .and_then(|x| std::str::from_utf8(x).ok())
        .unwrap_or("")
}

/// `N` little-endian bytes at byte offset `pos`; zeros when out of range.
fn le_bytes<const N: usize>(b: &[u8], pos: usize) -> [u8; N] {
    let mut out = [0u8; N];
    if let Some(s) = pos.checked_add(N).and_then(|e| b.get(pos..e)) {
        out.copy_from_slice(s);
    }
    out
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(le_bytes(b, i.saturating_mul(4)))
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(le_bytes(b, i.saturating_mul(8)))
}

/// Folded form of a title, used for ordering and prefix search.
pub fn fold_key(s: &str) -> String {
    tokens(s).join(" ")
}

// ------------------------------------------------------------------ builder

#[derive(Default)]
pub struct Builder {
    doc_offsets: Vec<u64>,
    doc_blob: Vec<u8>,
    keys: Vec<String>,
    head: HashMap<String, Vec<u32>>,
    full: HashMap<String, Vec<u32>>,
}

impl Builder {
    pub fn new() -> Self {
        Builder::default()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn add(&mut self, p: &Prepared) {
        let doc = self.keys.len() as u32;
        let r = &p.rec;
        self.doc_offsets.push(self.doc_blob.len() as u64);
        for s in [&r.title, &r.forms, &r.name] {
            write_str(&mut self.doc_blob, s);
        }
        write_varint(&mut self.doc_blob, r.off);
        write_varint(&mut self.doc_blob, r.len);

        let title_toks = tokens(&r.title);
        self.keys.push(title_toks.join(" "));

        let mut per_doc: BTreeMap<String, bool> = BTreeMap::new();
        for t in tokens(&r.forms) {
            if t.len() <= 40 {
                per_doc.entry(t).or_insert(false);
            }
        }
        for t in title_toks {
            if t.len() <= 40 {
                per_doc.insert(t, true);
            }
        }
        for (t, in_title) in per_doc {
            self.head
                .entry(t)
                .or_default()
                .push(doc << 1 | in_title as u32);
        }
        for t in &p.full_terms {
            self.full.entry(t.clone()).or_default().push(doc);
        }
    }

    fn term_section(map: HashMap<String, Vec<u32>>, max_df: usize) -> Vec<u8> {
        let mut terms: Vec<(String, Vec<u32>)> =
            map.into_iter().filter(|(_, v)| v.len() <= max_df).collect();
        terms.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let n = terms.len();
        let (mut term_off, mut post_off) = (
            Vec::with_capacity((n + 1) * 4),
            Vec::with_capacity((n + 1) * 8),
        );
        let (mut tb, mut pb) = (Vec::new(), Vec::new());
        for (t, p) in &terms {
            term_off.extend((tb.len() as u32).to_le_bytes());
            post_off.extend((pb.len() as u64).to_le_bytes());
            tb.extend_from_slice(t.as_bytes());
            let mut prev = 0u64;
            for &v in p {
                write_varint(&mut pb, v as u64 - prev);
                prev = v as u64;
            }
        }
        term_off.extend((tb.len() as u32).to_le_bytes());
        post_off.extend((pb.len() as u64).to_le_bytes());
        let mut out = Vec::with_capacity(4 + term_off.len() + post_off.len() + tb.len() + pb.len());
        out.extend((n as u32).to_le_bytes());
        out.extend(term_off);
        out.extend(post_off);
        out.extend(tb);
        out.extend(pb);
        out
    }

    /// Write the index atomically to `path`.
    pub fn finish(self, path: &str) -> Result<()> {
        let n = self.keys.len();
        let mut doc_offsets: Vec<u8> = Vec::with_capacity((n + 1) * 8);
        for o in &self.doc_offsets {
            doc_offsets.extend(o.to_le_bytes());
        }
        doc_offsets.extend((self.doc_blob.len() as u64).to_le_bytes());

        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_unstable_by(|&a, &b| {
            self.keys[a as usize]
                .cmp(&self.keys[b as usize])
                .then(a.cmp(&b))
        });
        let mut sorted = Vec::with_capacity(n * 4);
        for d in order {
            sorted.extend(d.to_le_bytes());
        }

        // Words in a large share of all definitions ("the", "of", ...) carry no
        // ranking signal and have the biggest posting lists: leave them out.
        let max_df = if n >= 2000 { n / 8 } else { usize::MAX };
        let head = Self::term_section(self.head, usize::MAX);
        let full = Self::term_section(self.full, max_df);

        let sections: [&[u8]; 5] = [&doc_offsets, &self.doc_blob, &sorted, &head, &full];
        let mut header = Vec::with_capacity(HEADER_LEN);
        header.extend_from_slice(MAGIC);
        header.extend((n as u32).to_le_bytes());
        header.extend(0u32.to_le_bytes());
        let mut off = HEADER_LEN as u64;
        for s in sections {
            header.extend(off.to_le_bytes());
            header.extend((s.len() as u64).to_le_bytes());
            off += s.len() as u64;
        }
        let tmp = format!("{path}.tmp");
        {
            let mut f = std::io::BufWriter::new(
                std::fs::File::create(&tmp).with_context(|| format!("creating {tmp}"))?,
            );
            f.write_all(&header)?;
            for s in sections {
                f.write_all(s)?;
            }
            f.flush()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

// ------------------------------------------------------------------- reader

pub struct Terms<'a> {
    pub n: usize,
    term_off: &'a [u8],
    post_off: &'a [u8],
    tb: &'a [u8],
    pb: &'a [u8],
}

impl<'a> Terms<'a> {
    fn parse(sec: &'a [u8]) -> Result<Terms<'a>> {
        if sec.len() < 4 {
            bail!("index: truncated term section");
        }
        let n = u32_at(sec, 0) as usize;
        let to = 4;
        let po = to + (n + 1) * 4;
        let tbs = po + (n + 1) * 8;
        if sec.len() < tbs {
            bail!("index: truncated term section");
        }
        let term_off = &sec[to..po];
        let tb_len = u32_at(term_off, n) as usize;
        let pbs = tbs + tb_len;
        if sec.len() < pbs {
            bail!("index: truncated term section");
        }
        Ok(Terms {
            n,
            term_off,
            post_off: &sec[po..tbs],
            tb: &sec[tbs..pbs],
            pb: &sec[pbs..],
        })
    }

    /// A section with no terms (stands in only for a section `Index::open` already validated).
    fn empty() -> Terms<'static> {
        Terms {
            n: 0,
            term_off: &[],
            post_off: &[],
            tb: &[],
            pb: &[],
        }
    }

    /// Check that every offset table is monotonic and inside the term and posting bytes.
    fn validate(&self) -> Result<()> {
        let (mut prev_t, mut prev_p) = (0u32, 0u64);
        for i in 0..=self.n {
            let (t, p) = (u32_at(self.term_off, i), u64_at(self.post_off, i));
            if t < prev_t || t as usize > self.tb.len() || p < prev_p || p as usize > self.pb.len()
            {
                bail!("index: corrupt term offsets");
            }
            prev_t = t;
            prev_p = p;
        }
        Ok(())
    }

    pub fn term(&self, i: usize) -> &'a [u8] {
        &self.tb[u32_at(self.term_off, i) as usize..u32_at(self.term_off, i + 1) as usize]
    }

    fn post(&self, i: usize) -> &'a [u8] {
        &self.pb[u64_at(self.post_off, i) as usize..u64_at(self.post_off, i + 1) as usize]
    }

    pub fn lower_bound(&self, key: &[u8]) -> usize {
        let (mut lo, mut hi) = (0, self.n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.term(mid) < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Index range of every term starting with `p` (the exact term, if any, first).
    pub fn prefix_range(&self, p: &[u8]) -> (usize, usize) {
        let lo = self.lower_bound(p);
        let (mut a, mut b) = (lo, self.n);
        while a < b {
            let mid = (a + b) / 2;
            if self.term(mid).starts_with(p) {
                a = mid + 1;
            } else {
                b = mid;
            }
        }
        (lo, a)
    }

    pub fn exact(&self, t: &[u8]) -> Option<usize> {
        let i = self.lower_bound(t);
        (i < self.n && self.term(i) == t).then_some(i)
    }

    /// Posting values (raw, delta-decoded). Head postings are `doc << 1 | in_title`.
    pub fn postings(&self, i: usize) -> Vec<u32> {
        let b = self.post(i);
        let (mut pos, mut v) = (0, 0u64);
        let mut out = Vec::with_capacity(b.len() / 2 + 1);
        while pos < b.len() {
            v += read_varint(b, &mut pos);
            out.push(v as u32);
        }
        out
    }
}

pub struct DocView<'a> {
    pub title: &'a str,
    pub forms: &'a str,
    pub name: &'a str,
    pub off: u64,
    pub len: u64,
}

pub struct Index {
    mmap: Mmap,
    pub n: usize,
    sec: [(usize, usize); 5],
}

impl Index {
    pub fn open(path: &str) -> Result<Index> {
        let f = std::fs::File::open(path).with_context(|| format!("opening {path}"))?;
        let mmap = unsafe { Mmap::map(&f)? };
        if mmap.len() < HEADER_LEN || &mmap[..8] != MAGIC {
            bail!("{path} is not a BetterDict index (or is from a newer version)");
        }
        let n = u32_at(&mmap, 2) as usize;
        let mut sec = [(0, 0); 5];
        for (i, s) in sec.iter_mut().enumerate() {
            let base = 16 + i * 16;
            let off = u64::from_le_bytes(le_bytes(&mmap, base)) as usize;
            let len = u64::from_le_bytes(le_bytes(&mmap, base + 8)) as usize;
            if off.checked_add(len).is_none_or(|e| e > mmap.len()) {
                bail!("{path} is truncated");
            }
            *s = (off, len);
        }
        let ix = Index { mmap, n, sec };
        let (offs, blob) = (ix.section(0), ix.section(1));
        if offs.len() != (n + 1) * 8 || u64_at(offs, n) != blob.len() as u64 {
            bail!("{path} has a corrupt document table");
        }
        if ix.section(2).len() != n * 4 {
            bail!("{path} has a corrupt title order");
        }
        for i in [3, 4] {
            Terms::parse(ix.section(i))
                .and_then(|t| t.validate())
                .with_context(|| format!("{path}: bad term section"))?;
        }
        Ok(ix)
    }

    fn section(&self, i: usize) -> &[u8] {
        let (o, l) = self.sec[i];
        &self.mmap[o..o + l]
    }

    pub fn doc(&self, i: u32) -> DocView<'_> {
        let offs = self.section(0);
        let blob = self.section(1);
        let mut pos = u64_at(offs, i as usize) as usize;
        let title = read_str(blob, &mut pos);
        let forms = read_str(blob, &mut pos);
        let name = read_str(blob, &mut pos);
        let off = read_varint(blob, &mut pos);
        let len = read_varint(blob, &mut pos);
        DocView {
            title,
            forms,
            name,
            off,
            len,
        }
    }

    /// Doc id at position `i` of the title-sorted order.
    pub fn sorted_doc(&self, i: usize) -> u32 {
        u32_at(self.section(2), i)
    }

    pub fn head(&self) -> Terms<'_> {
        Terms::parse(self.section(3)).unwrap_or_else(|_| Terms::empty())
    }

    pub fn full(&self) -> Terms<'_> {
        Terms::parse(self.section(4)).unwrap_or_else(|_| Terms::empty())
    }

    /// First position in title order whose folded title is >= `key`.
    pub fn title_lower_bound(&self, key: &str) -> usize {
        let (mut lo, mut hi) = (0, self.n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            if fold_key(self.doc(self.sorted_doc(mid)).title).as_str() < key {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{DocRec, Prepared};

    fn tmp(name: &str) -> String {
        format!(
            "{}/bd-index-{}-{name}.bin",
            std::env::temp_dir().display(),
            std::process::id()
        )
    }

    fn build(n: usize, path: &str) {
        let mut b = Builder::new();
        for i in 0..n {
            b.add(&Prepared {
                rec: DocRec {
                    title: format!("word{i}"),
                    forms: String::new(),
                    name: "n".into(),
                    off: i as u64,
                    len: 1,
                },
                full_terms: vec![format!("def{i}")],
            });
        }
        b.finish(path).unwrap();
    }

    /// Build a small index, let `patch` edit its bytes, and return what `Index::open` says.
    fn open_patched(name: &str, patch: impl Fn(&mut Vec<u8>)) -> Result<Index> {
        let path = tmp(name);
        build(50, &path);
        let mut bytes = std::fs::read(&path).unwrap();
        patch(&mut bytes);
        std::fs::write(&path, &bytes).unwrap();
        let r = Index::open(&path);
        let _ = std::fs::remove_file(&path);
        r
    }

    fn sec(bytes: &[u8], i: usize) -> (usize, usize) {
        let b = 16 + i * 16;
        (
            u64::from_le_bytes(le_bytes(bytes, b)) as usize,
            u64::from_le_bytes(le_bytes(bytes, b + 8)) as usize,
        )
    }

    #[test]
    fn open_accepts_a_good_index() {
        let ix = open_patched("good", |_| {}).unwrap();
        assert_eq!(ix.n, 50);
        assert_eq!(ix.head().n, 50);
        assert!(ix.full().exact(b"def7").is_some());
    }

    #[test]
    fn open_rejects_corrupt_term_sections() {
        // term_off[1] of the head section points far past the term bytes
        assert!(open_patched("termoff", |b| {
            let at = sec(b, 3).0 + 4 + 4;
            b[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        })
        .is_err());
        // post_off[2] of the full section decreases below post_off[1]
        assert!(open_patched("postoff", |b| {
            let (o, _) = sec(b, 4);
            let n = u32::from_le_bytes(le_bytes(b, o)) as usize;
            let at = o + 4 + (n + 1) * 4 + 2 * 8;
            b[at..at + 8].copy_from_slice(&0u64.to_le_bytes());
            let at1 = at - 8;
            b[at1..at1 + 8].copy_from_slice(&5u64.to_le_bytes());
        })
        .is_err());
        // the head section is cut short
        assert!(open_patched("short", |b| {
            let at = 16 + 3 * 16 + 8;
            b[at..at + 8].copy_from_slice(&10u64.to_le_bytes());
        })
        .is_err());
        // the term count claims more terms than the section holds
        assert!(open_patched("count", |b| {
            let at = sec(b, 4).0;
            b[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        })
        .is_err());
    }

    #[test]
    fn open_rejects_corrupt_doc_tables() {
        assert!(open_patched("docs", |b| {
            let at = 16 + 8;
            b[at..at + 8].copy_from_slice(&16u64.to_le_bytes());
        })
        .is_err());
        assert!(open_patched("order", |b| {
            let at = 16 + 2 * 16 + 8;
            b[at..at + 8].copy_from_slice(&8u64.to_le_bytes());
        })
        .is_err());
    }

    #[test]
    #[ignore]
    fn time_open_100k_terms() {
        let path = tmp("time");
        build(100_000, &path);
        let t = std::time::Instant::now();
        for _ in 0..100 {
            let ix = Index::open(&path).unwrap();
            std::hint::black_box(&ix);
        }
        eprintln!("open: {:?} per call", t.elapsed() / 100);
        let _ = std::fs::remove_file(&path);
    }
}

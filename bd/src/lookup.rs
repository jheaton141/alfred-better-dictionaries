//! Alfred script-filter output for `lookup`, `search` and combined search.
use crate::index::{DocView, Index};
use crate::item::{self, Sel};
use crate::query::{self, Hit};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};

const LIMIT: usize = 9;

/// True if the dictionary has a readable index of the current format.
pub fn index_ok(data_dir: &str, id: &str) -> bool {
    Index::open(&index_path(data_dir, id)).is_ok()
}

pub fn index_path(data_dir: &str, id: &str) -> String {
    format!("{data_dir}/{id}/index.bin")
}

fn cache_path(data_dir: &str, id: &str, name: &str) -> String {
    format!("{data_dir}/cache/{id}/{name}")
}

/// The definition's HTML, from its gzip member in the pack.
fn read_definition(data_dir: &str, id: &str, d: &DocView) -> Result<Vec<u8>> {
    let mut f = std::fs::File::open(format!("{data_dir}/{id}/defs.pack")).context("defs.pack")?;
    if d.off.saturating_add(d.len) > f.metadata()?.len() {
        anyhow::bail!("definition lies outside defs.pack");
    }
    f.seek(SeekFrom::Start(d.off))?;
    let mut buf = vec![0u8; d.len as usize];
    f.read_exact(&mut buf)?;
    let mut html = Vec::new();
    flate2::read::GzDecoder::new(&buf[..]).read_to_end(&mut html)?;
    if !names_title(&html, d.title) {
        anyhow::bail!("defs.pack does not match the index (rebuilt during this lookup?)");
    }
    Ok(html)
}

/// True if the definition's opening tag carries `d:title="<title>"` (its own entry, not another's).
fn names_title(html: &[u8], title: &str) -> bool {
    let needle = format!("d:title=\"{title}\"");
    let head = &html[..html.len().min(4096)];
    head.windows(needle.len()).any(|w| w == needle.as_bytes())
}

const PAGE_HEAD: &[u8] = b"<!DOCTYPE html><html lang=\"en\"><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"/></head><body>";
const PAGE_TAIL: &[u8] = b"</body></html>";

/// Alfred (quicklook, the extra pane, "open") needs a real .html file, so
/// write the few hits about to be shown, once.
fn materialize(data_dir: &str, id: &str, d: &DocView, html: &[u8]) -> Result<String> {
    let path = cache_path(data_dir, id, d.name);
    let size = (PAGE_HEAD.len() + html.len() + PAGE_TAIL.len()) as u64;
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == size) {
        // a use counts as a touch, so pruning drops only entries unused for a week
        if let Ok(f) = std::fs::File::options().write(true).open(&path) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        return Ok(path);
    }
    // missing, or left by an older definition: (re)write it
    std::fs::create_dir_all(format!("{data_dir}/cache/{id}"))?;
    let tmp = format!("{path}.{}.tmp", std::process::id());
    {
        let mut o = std::fs::File::create(&tmp)?;
        o.write_all(PAGE_HEAD)?;
        o.write_all(html)?;
        o.write_all(PAGE_TAIL)?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

const TEXT_VIEW_SUBTITLE: &str = "View as selectable text";

fn item_json(sel: &Sel, data_dir: &str, id: &str, d: &DocView, label: Option<&str>) -> Value {
    let Ok(html) = read_definition(data_dir, id, d) else {
        return json!({"title": d.title, "subtitle": "Definition unavailable (try again)", "valid": false});
    };
    let path =
        materialize(data_dir, id, d, &html).unwrap_or_else(|_| cache_path(data_dir, id, d.name));
    let (sub, pron) = item::display(sel, &String::from_utf8_lossy(&html));
    let subtitle = match label {
        Some(l) => format!("[{l}] {sub}"),
        None => sub,
    };
    let mut item =
        json!({"title": d.title, "subtitle": subtitle, "arg": path, "quicklookurl": path});
    let mut mods = json!({"alt": {"subtitle": TEXT_VIEW_SUBTITLE}});
    if !pron.is_empty() {
        mods["cmd"] = json!({"arg": pron, "subtitle": format!("🗣 {pron}")});
    }
    item["mods"] = mods;
    item
}

/// `bd text-view <page.html>`: the plain text of a cached definition page.
pub fn text_view(data_dir: &str, page: &str) -> Result<String> {
    let cache = Path::new(data_dir).join("cache").canonicalize();
    let page = Path::new(page).canonicalize();
    let (Ok(cache), Ok(page)) = (cache, page) else {
        return Ok("This definition is no longer cached: run the lookup again.".into());
    };
    if !page.starts_with(&cache) || page.extension().is_none_or(|e| e != "html") {
        anyhow::bail!("{} is not a cached definition", page.display());
    }
    Ok(item::plain_text(&String::from_utf8_lossy(&std::fs::read(
        &page,
    )?)))
}

fn message(title: &str, subtitle: &str) -> Value {
    json!({"items": [{"title": title, "subtitle": subtitle, "valid": false}]})
}

const DAMAGED: (&str, &str) = (
    "A search index is damaged",
    "It is being rebuilt: run lookup again in a moment",
);

/// Run `f`, turning a panic (a corrupt index) into `None`.
fn guarded<T>(f: impl FnOnce() -> T) -> Option<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok()
}

/// Delete a damaged index so the next lookup rebuilds it from the pack.
fn discard_index(data_dir: &str, id: &str) {
    let _ = std::fs::remove_file(index_path(data_dir, id));
}

/// `bd search <dict_id> <query>`
pub fn search_dict(data_dir: &str, id: &str, q: &str) -> Result<Value> {
    let ix = match Index::open(&index_path(data_dir, id)) {
        Ok(ix) => ix,
        Err(_) => {
            if !Path::new(&format!("{data_dir}/{id}/defs.pack")).exists()
                && !Path::new(&format!("{data_dir}/{id}/html")).is_dir()
            {
                return Ok(message(
                    "This dictionary's files are missing",
                    "Re-import it with .dict-import",
                ));
            }
            return Ok(message(
                "This dictionary needs a one-time upgrade",
                "Run: lookup — it will upgrade in the background",
            ));
        }
    };
    let sel = Sel::new();
    let Some(hits) = guarded(|| query::search(&ix, q, LIMIT)) else {
        discard_index(data_dir, id);
        return Ok(message(DAMAGED.0, DAMAGED.1));
    };
    let items: Vec<Value> = hits
        .iter()
        .filter_map(|h| guarded(|| item_json(&sel, data_dir, id, &ix.doc(h.doc), None)))
        .collect();
    Ok(json!({ "items": items }))
}

/// Order merged hits best first, across dictionaries.
fn order_hits(all: &mut [(usize, Hit)]) {
    // tier and title length (key >> 32) first, then dictionary order, then the in-dictionary key
    all.sort_by_key(|(i, h)| (std::cmp::Reverse(h.key >> 32), *i, std::cmp::Reverse(h.key)));
}

/// One merged result list across several opened (title, id) dictionaries, in search order.
fn search_all(data_dir: &str, opened: &[((String, String), Index)], q: &str) -> Result<Value> {
    let mut all: Vec<(usize, Hit)> = vec![];
    let mut damaged = false;
    for (i, (d, ix)) in opened.iter().enumerate() {
        match guarded(|| query::search(ix, q, LIMIT)) {
            Some(hits) => all.extend(hits.into_iter().map(|h| (i, h))),
            None => {
                discard_index(data_dir, &d.1);
                damaged = true;
            }
        }
    }
    order_hits(&mut all);
    all.truncate(LIMIT);
    let sel = Sel::new();
    let mut items: Vec<Value> = all
        .iter()
        .filter_map(|(i, h)| {
            let ((title, id), ix) = (&opened[*i].0, &opened[*i].1);
            guarded(|| item_json(&sel, data_dir, id, &ix.doc(h.doc), Some(title)))
        })
        .collect();
    if damaged && items.is_empty() {
        return Ok(message(DAMAGED.0, DAMAGED.1));
    }
    if damaged {
        items.push(json!({"title": DAMAGED.0, "subtitle": DAMAGED.1, "valid": false}));
    }
    Ok(json!({ "items": items }))
}

fn open_extra_pane(wf_dir: &str) {
    let _ = Command::new("open")
        .args(["-g", &format!("{wf_dir}/AlfredExtraPane.app")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn pid_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True if `path` was modified within the last `secs` seconds.
fn modified_within(path: &str, secs: u64) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|e| e.as_secs() < secs)
}

/// True while a detached `bd <job>` (migrate, setup) holds its lock file.
pub fn job_running(data_dir: &str, job: &str) -> bool {
    let lock = format!("{data_dir}/{job}.lock");
    let Ok(pid) = std::fs::read_to_string(&lock) else {
        return false;
    };
    if pid.trim().is_empty() {
        // just claimed, the child's pid is not written yet
        return modified_within(&lock, 10);
    }
    pid_alive(pid.trim())
}

/// Start `bd <job> <data_dir>` detached from this (short-lived) script-filter process.
fn spawn_job(data_dir: &str, job: &str, quiet: bool) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // Take the lock atomically so keystroke-by-keystroke lookups start only one job.
    let lock = format!("{data_dir}/{job}.lock");
    let claim = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
    };
    let mut f = match claim() {
        Ok(f) => f,
        Err(_) if job_running(data_dir, job) => return,
        Err(_) => {
            let _ = std::fs::remove_file(&lock); // stale: its process is gone
            match claim() {
                Ok(f) => f,
                Err(_) => return,
            }
        }
    };
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{data_dir}/import.log"));
    let mut c = Command::new(exe);
    c.args([job, data_dir])
        .stdin(Stdio::null())
        .process_group(0);
    if quiet {
        c.env("BD_QUIET", "1");
    }
    match log {
        Ok(l) => {
            c.stdout(l.try_clone().unwrap()).stderr(l);
        }
        Err(_) => {
            c.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    if let Ok(child) = c.spawn() {
        let _ = write!(f, "{}", child.id());
    } else {
        let _ = std::fs::remove_file(&lock);
    }
}

/// The imported dictionaries as (title, id), from `imported.json`.
pub fn imported_dicts(data_dir: &str) -> Vec<(String, String)> {
    let imported: Value = std::fs::read_to_string(format!("{data_dir}/imported.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    imported["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    Some((
                        i["title"].as_str()?.to_string(),
                        i["arg"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The single `lookup` script filter: every imported dictionary, merged by
/// relevance. The first run imports the built-in dictionary and thesaurus.
pub fn lookup(data_dir: &str, wf_dir: &str, query: &str) -> Result<Value> {
    let dicts = imported_dicts(data_dir);
    let setting_up = || {
        let mut v = message(
            "Setting up the dictionary and thesaurus (first run)…",
            "This takes a few seconds; results appear automatically",
        );
        v["rerun"] = json!(1);
        v
    };
    let setup_running = job_running(data_dir, "setup");
    if setup_running && dicts.is_empty() {
        return Ok(setting_up());
    }
    if !setup_running && !Path::new(&format!("{data_dir}/setup.done")).exists() {
        let tried = format!("{data_dir}/setup.tried");
        let failed = format!("{data_dir}/setup.failed");
        let attempts = std::fs::read_to_string(&tried)
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let failure = std::fs::read_to_string(&failed)
            .ok()
            .map(|t| t.trim().to_string());
        let mut spawned = false;
        if !modified_within(&tried, setup_window_secs(attempts, !dicts.is_empty())) {
            let next = next_attempts(attempts, failure.is_some());
            let _ = std::fs::write(&tried, next.to_string());
            // with dictionaries already imported, retries stay silent
            spawn_job(data_dir, "setup", !dicts.is_empty());
            spawned = true;
        }
        if dicts.is_empty() {
            return Ok(match failure {
                Some(why) if !spawned => message("Couldn't set up the built-in dictionaries", &why),
                _ => setting_up(),
            });
        }
    }

    // Each index is opened once; one that won't open but has data (an older version's, or damaged) is rebuilt.
    let mut indexes: Vec<((String, String), Index)> = vec![];
    let mut stale = false;
    for d in &dicts {
        match Index::open(&index_path(data_dir, &d.1)) {
            Ok(ix) => indexes.push((d.clone(), ix)),
            Err(_) => {
                stale |= Path::new(&format!("{data_dir}/{}/defs.pack", d.1)).exists()
                    || Path::new(&format!("{data_dir}/{}/html", d.1)).is_dir();
            }
        }
    }
    if stale {
        if let Ok(why) = std::fs::read_to_string(format!("{data_dir}/migrate.failed")) {
            return Ok(message(
                "Upgrading the dictionaries failed",
                &format!("See import.log in the workflow data folder. {}", why.trim()),
            ));
        }
        spawn_job(data_dir, "migrate", false);
        let mut v = message(
            "Upgrading your dictionaries (one time)…",
            "Searching is available again when it finishes; progress is in the notifications",
        );
        v["rerun"] = json!(2);
        return Ok(v);
    }

    let (scope, word) = parse_scope(query);
    let chosen = search_order(&dicts, scope);
    if word.is_empty() {
        return Ok(message(
            "Look up a word",
            "Type a word, or d: word (dictionary only) / t: word (thesaurus only)",
        ));
    }
    open_extra_pane(wf_dir);
    if chosen.is_empty() {
        return Ok(message(
            "Nothing imported for that scope",
            "Try without the d: / t: prefix",
        ));
    }
    let picked: Vec<((String, String), Index)> = chosen
        .iter()
        .filter_map(|d| {
            let at = indexes.iter().position(|x| &x.0 == d)?;
            Some(indexes.swap_remove(at))
        })
        .collect();
    search_all(data_dir, &picked, word)
}

/// Which dictionaries a lookup searches.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Scope {
    All,
    Dictionary,
    Thesaurus,
}

fn is_thesaurus(id: &str) -> bool {
    id.ends_with("-OAWT") || id.ends_with("-OTE")
}

/// Dictionaries the scope admits, dictionaries before thesauri (ties in `search_all` follow this order).
fn search_order(dicts: &[(String, String)], scope: Scope) -> Vec<(String, String)> {
    let mut chosen: Vec<(String, String)> = dicts
        .iter()
        .filter(|d| scope.admits(&d.1))
        .cloned()
        .collect();
    chosen.sort_by_key(|d| is_thesaurus(&d.1));
    chosen
}

impl Scope {
    fn admits(self, id: &str) -> bool {
        let thesaurus = is_thesaurus(id);
        match self {
            Scope::All => true,
            Scope::Dictionary => !thesaurus,
            Scope::Thesaurus => thesaurus,
        }
    }
}

/// Split a leading scope word and colon off the query: `d:`/`dict:`/`dictionary:` (dictionary
/// only) or `t:`/`thes:`/`thesaurus:` (thesaurus only), any case, spaces around the colon allowed.
fn parse_scope(query: &str) -> (Scope, &str) {
    let q = query.trim();
    let word_end = q
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(q.len());
    let rest = q[word_end..].trim_start();
    let Some(rest) = rest.strip_prefix(':') else {
        return (Scope::All, q);
    };
    let scope = match q[..word_end].to_ascii_lowercase().as_str() {
        "d" | "dict" | "dictionary" => Scope::Dictionary,
        "t" | "thes" | "thesaurus" => Scope::Thesaurus,
        _ => return (Scope::All, q),
    };
    (scope, rest.trim())
}

/// Seconds to wait before retrying setup after `attempts` failed retries: 60 s doubling, capped at 24 h.
fn retry_window_secs(attempts: u32) -> u64 {
    60u64
        .saturating_mul(1u64 << attempts.min(20))
        .min(24 * 3600)
}

/// Retry window for setup: backoff only for silent retries; with nothing imported the user is waiting, so 60 s.
fn setup_window_secs(attempts: u32, have_dicts: bool) -> u64 {
    if have_dicts {
        retry_window_secs(attempts)
    } else {
        retry_window_secs(0)
    }
}

/// The retry count to record when setup is spawned again.
fn next_attempts(attempts: u32, failed: bool) -> u32 {
    if failed {
        attempts.saturating_add(1)
    } else {
        attempts
    }
}

/// Drop cached HTML not touched for a week (regenerated on demand). Cheap
/// marker file so the directory walk happens at most once a day.
pub fn prune_cache(data_dir: &str) {
    let marker = format!("{data_dir}/cache/.pruned");
    if modified_within(&marker, 86400) || !Path::new(&format!("{data_dir}/cache")).is_dir() {
        return;
    }
    let week = std::time::Duration::from_secs(7 * 86400);
    for e in walkdir::WalkDir::new(format!("{data_dir}/cache"))
        .into_iter()
        .flatten()
    {
        if e.file_type().is_file() && e.file_name() != ".pruned" {
            if let Some(age) = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| t.elapsed().unwrap_or_default())
            {
                if age > week {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let _ = std::fs::write(&marker, "");
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn prefixes_pick_the_scope() {
        assert_eq!(parse_scope("run"), (Scope::All, "run"));
        assert_eq!(parse_scope("d: run"), (Scope::Dictionary, "run"));
        assert_eq!(parse_scope("T:happy"), (Scope::Thesaurus, "happy"));
        assert_eq!(
            parse_scope("thes: kick the bucket"),
            (Scope::Thesaurus, "kick the bucket")
        );
        assert_eq!(parse_scope("d:"), (Scope::Dictionary, ""));
        assert_eq!(parse_scope("döner"), (Scope::All, "döner"));
        assert_eq!(parse_scope("t"), (Scope::All, "t"));
        assert_eq!(parse_scope("dict: happy"), (Scope::Dictionary, "happy"));
        assert_eq!(
            parse_scope("Dictionary : happy"),
            (Scope::Dictionary, "happy")
        );
        assert_eq!(parse_scope("thesaurus:happy"), (Scope::Thesaurus, "happy"));
        assert_eq!(parse_scope("dic: happy"), (Scope::All, "dic: happy"));
    }

    #[test]
    fn dictionaries_are_searched_before_thesauri() {
        let d = |t: &str, id: &str| (t.to_string(), id.to_string());
        let dicts = vec![
            d("Thesaurus", "com-apple-dictionary-OAWT"),
            d("Wiki", "com-apple-dictionary-Wikipedia"),
            d("Dictionary", "com-apple-dictionary-NOAD"),
        ];
        let ids: Vec<String> = search_order(&dicts, Scope::All)
            .into_iter()
            .map(|x| x.1)
            .collect();
        assert_eq!(
            ids,
            [
                "com-apple-dictionary-Wikipedia",
                "com-apple-dictionary-NOAD",
                "com-apple-dictionary-OAWT"
            ]
        );
        assert_eq!(search_order(&dicts, Scope::Thesaurus).len(), 1);
    }

    #[test]
    fn thesauri_are_told_apart_from_dictionaries() {
        let (nd, ot) = ("com-apple-dictionary-NOAD", "com-apple-dictionary-OTE");
        assert!(Scope::Dictionary.admits(nd) && !Scope::Dictionary.admits(ot));
        assert!(Scope::Thesaurus.admits(ot) && !Scope::Thesaurus.admits(nd));
        assert!(Scope::All.admits(nd) && Scope::All.admits(ot));
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::index::Builder;
    use crate::item::{DocRec, Prepared};

    fn temp() -> String {
        let d = std::env::temp_dir().join(format!("bd-lookup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.to_string_lossy().into_owned()
    }

    /// One-document index for dictionary `id`; the definition is `pack` if given.
    fn one_doc(dir: &str, id: &str, pack: Option<&[u8]>) -> Index {
        std::fs::create_dir_all(format!("{dir}/{id}")).unwrap();
        let mut len = 10;
        if let Some(html) = pack {
            let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            z.write_all(html).unwrap();
            let gz = z.finish().unwrap();
            len = gz.len() as u64;
            std::fs::write(format!("{dir}/{id}/defs.pack"), gz).unwrap();
        }
        let mut b = Builder::new();
        b.add(&Prepared {
            rec: DocRec {
                title: "word".into(),
                forms: String::new(),
                name: "n0.html".into(),
                off: 0,
                len,
            },
            full_terms: vec![],
        });
        b.finish(&index_path(dir, id)).unwrap();
        Index::open(&index_path(dir, id)).unwrap()
    }

    #[test]
    fn every_result_offers_the_text_view_on_option() {
        let dir = temp();
        let ix = one_doc(
            &dir,
            "d1",
            Some(br#"<d:entry d:title="word">hello</d:entry>"#),
        );
        let v = item_json(&Sel::new(), &dir, "d1", &ix.doc(0), None);
        assert_eq!(v["mods"]["alt"]["subtitle"], TEXT_VIEW_SUBTITLE);
        assert!(
            v["mods"]["alt"].get("arg").is_none(),
            "keeps the page path as its arg"
        );
        let ipa =
            r#"<d:entry d:title="word"><span d:prn="IPA" class="ph t_IPA">wɜrd</span></d:entry>"#
                .as_bytes();
        let ix = one_doc(&dir, "d2", Some(ipa));
        let v = item_json(&Sel::new(), &dir, "d2", &ix.doc(0), None);
        assert_eq!(v["mods"]["alt"]["subtitle"], TEXT_VIEW_SUBTITLE);
        assert_eq!(v["mods"]["cmd"]["arg"], "wɜrd");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn text_view_reads_cached_pages_only() {
        let dir = temp();
        let ix = one_doc(&dir, "d1", Some(br#"<d:entry d:title="word"><span class="hg">word</span><span class="se2">one</span></d:entry>"#));
        let v = item_json(&Sel::new(), &dir, "d1", &ix.doc(0), None);
        let page = v["arg"].as_str().unwrap();
        assert_eq!(text_view(&dir, page).unwrap(), "word\none\n");
        let outside = format!("{dir}/outside.html");
        std::fs::write(&outside, "secret").unwrap();
        assert!(text_view(&dir, &outside).is_err());
        assert!(text_view(&dir, &format!("{dir}/cache/d1/../../outside.html")).is_err());
        let notes = format!("{dir}/cache/d1/notes.txt");
        std::fs::write(&notes, "text").unwrap();
        assert!(text_view(&dir, &notes).is_err());
        let gone = cache_path(&dir, "d1", "n9.html");
        assert!(text_view(&dir, &gone)
            .unwrap()
            .contains("run the lookup again"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unreadable_definition_is_not_cached() {
        let dir = temp();
        let ix = one_doc(&dir, "d1", None);
        let v = item_json(&Sel::new(), &dir, "d1", &ix.doc(0), None);
        assert_eq!(v["title"], "word");
        assert_eq!(v["valid"], false);
        assert!(!Path::new(&cache_path(&dir, "d1", "n0.html")).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn readable_definition_is_cached() {
        let dir = temp();
        let ix = one_doc(
            &dir,
            "d1",
            Some(br#"<d:entry d:title="word">hello</d:entry>"#),
        );
        let v = item_json(&Sel::new(), &dir, "d1", &ix.doc(0), None);
        let path = cache_path(&dir, "d1", "n0.html");
        assert_eq!(v["arg"], path.as_str());
        assert!(std::fs::read_to_string(&path).unwrap().contains("hello"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_stale_cached_page_is_rewritten() {
        let dir = temp();
        let ix = one_doc(
            &dir,
            "d1",
            Some(br#"<d:entry d:title="word">hello</d:entry>"#),
        );
        let path = cache_path(&dir, "d1", "n0.html");
        std::fs::create_dir_all(format!("{dir}/cache/d1")).unwrap();
        std::fs::write(&path, "<html><body></body></html>").unwrap();
        item_json(&Sel::new(), &dir, "d1", &ix.doc(0), None);
        assert!(std::fs::read_to_string(&path).unwrap().contains("hello"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn another_entrys_definition_is_not_shown() {
        // an old index read against a newer pack: the offset now holds a different entry
        let dir = temp();
        let ix = one_doc(
            &dir,
            "d1",
            Some(br#"<d:entry d:title="other">hi</d:entry>"#),
        );
        let v = item_json(&Sel::new(), &dir, "d1", &ix.doc(0), None);
        assert_eq!(v["valid"], false);
        assert!(!Path::new(&cache_path(&dir, "d1", "n0.html")).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reuse_refreshes_the_cache_mtime() {
        let dir = temp();
        let ix = one_doc(&dir, "d1", None);
        let path = materialize(&dir, "d1", &ix.doc(0), b"x").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86400);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(!modified_within(&path, 3600));
        materialize(&dir, "d1", &ix.doc(0), b"x").unwrap();
        assert!(modified_within(&path, 3600));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    fn key(tier: u64, title_chars: u64, doc: u32) -> u64 {
        (tier << 48) | (0xFFFF - title_chars) << 32 | (u32::MAX - doc) as u64
    }

    fn hit(doc: u32, tier: u64, chars: u64) -> Hit {
        Hit {
            doc,
            key: key(tier, chars, doc),
        }
    }

    fn order(mut v: Vec<(usize, Hit)>) -> Vec<(usize, u32)> {
        order_hits(&mut v);
        v.into_iter().map(|(i, h)| (i, h.doc)).collect()
    }

    #[test]
    fn equal_tier_and_length_follow_dictionary_order() {
        // dictionary 1's doc id is smaller, so its raw key is larger
        let got = order(vec![(1, hit(1, 5, 4)), (0, hit(900, 5, 4))]);
        assert_eq!(got, vec![(0, 900), (1, 1)]);
    }

    #[test]
    fn tier_and_title_length_beat_dictionary_order() {
        let got = order(vec![(0, hit(1, 4, 4)), (1, hit(9, 5, 4))]);
        assert_eq!(got, vec![(1, 9), (0, 1)]);
        let got = order(vec![(0, hit(1, 5, 9)), (1, hit(9, 5, 4))]);
        assert_eq!(got, vec![(1, 9), (0, 1)]);
    }

    #[test]
    fn same_dictionary_keeps_key_order() {
        let got = order(vec![(0, hit(7, 5, 4)), (0, hit(3, 5, 4))]);
        assert_eq!(got, vec![(0, 3), (0, 7)]);
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn window_doubles_and_caps_at_a_day() {
        assert_eq!(retry_window_secs(0), 60);
        assert_eq!(retry_window_secs(1), 120);
        assert_eq!(retry_window_secs(3), 480);
        assert_eq!(retry_window_secs(10), 61440);
        assert_eq!(retry_window_secs(11), 86400);
        assert_eq!(retry_window_secs(u32::MAX), 86400);
        // a user with nothing imported is retried every minute however often setup failed
        assert_eq!(setup_window_secs(10, false), 60);
        assert_eq!(setup_window_secs(10, true), 61440);
    }

    #[test]
    fn only_a_retry_after_failure_counts() {
        assert_eq!(next_attempts(0, false), 0);
        assert_eq!(next_attempts(0, true), 1);
        assert_eq!(next_attempts(u32::MAX, true), u32::MAX);
    }

    #[test]
    fn imported_dicts_need_an_items_array() {
        let dir = std::env::temp_dir().join(format!("bd-imp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_string_lossy().into_owned();
        assert!(imported_dicts(&d).is_empty());
        std::fs::write(dir.join("imported.json"), r#"{"items": "x"}"#).unwrap();
        assert!(imported_dicts(&d).is_empty());
        std::fs::write(
            dir.join("imported.json"),
            r#"{"items": [{"title": "A", "arg": "a"}, {"title": "no arg"}]}"#,
        )
        .unwrap();
        assert_eq!(imported_dicts(&d), vec![("A".to_string(), "a".to_string())]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

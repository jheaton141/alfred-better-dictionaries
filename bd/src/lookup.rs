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
    Ok(html)
}

/// Alfred (quicklook, the extra pane, "open") needs a real .html file, so
/// write the few hits about to be shown, once.
fn materialize(data_dir: &str, id: &str, d: &DocView, html: &[u8]) -> Result<String> {
    let path = cache_path(data_dir, id, d.name);
    if Path::new(&path).exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(format!("{data_dir}/cache/{id}"))?;
    let tmp = format!("{path}.{}.tmp", std::process::id());
    {
        let mut o = std::fs::File::create(&tmp)?;
        o.write_all(b"<!DOCTYPE html><html lang=\"en\"><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"/></head><body>")?;
        o.write_all(html)?;
        o.write_all(b"</body></html>")?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn item_json(sel: &Sel, data_dir: &str, id: &str, d: &DocView, label: Option<&str>) -> Value {
    let html = read_definition(data_dir, id, d).unwrap_or_default();
    let path =
        materialize(data_dir, id, d, &html).unwrap_or_else(|_| cache_path(data_dir, id, d.name));
    let (sub, pron) = item::display(sel, &String::from_utf8_lossy(&html));
    let subtitle = match label {
        Some(l) => format!("[{l}] {sub}"),
        None => sub,
    };
    let mut item =
        json!({"title": d.title, "subtitle": subtitle, "arg": path, "quicklookurl": path});
    if !pron.is_empty() {
        item["mods"] = json!({"cmd": {"arg": pron, "subtitle": format!("🗣 {pron}")}});
    }
    item
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

/// One merged result list across several dictionaries.
pub fn search_all(data_dir: &str, dicts: &[(String, String)], q: &str) -> Result<Value> {
    let mut opened: Vec<(&(String, String), Index)> = vec![];
    for d in dicts {
        if let Ok(ix) = Index::open(&index_path(data_dir, &d.1)) {
            opened.push((d, ix));
        }
    }
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
    all.sort_by_key(|a| std::cmp::Reverse(a.1.key)); // stable: dictionary order breaks ties
    all.truncate(LIMIT);
    let sel = Sel::new();
    let mut items: Vec<Value> = all
        .iter()
        .filter_map(|(i, h)| {
            let ((title, id), ix) = (opened[*i].0, &opened[*i].1);
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

/// True while a detached `bd <job>` (migrate, setup) holds its lock file.
pub fn job_running(data_dir: &str, job: &str) -> bool {
    let lock = format!("{data_dir}/{job}.lock");
    let Ok(pid) = std::fs::read_to_string(&lock) else {
        return false;
    };
    if pid.trim().is_empty() {
        // just claimed, the child's pid is not written yet
        return std::fs::metadata(&lock)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|e| e.as_secs() < 10);
    }
    pid_alive(pid.trim())
}

/// Start `bd <job> <data_dir>` detached from this (short-lived) script-filter process.
fn spawn_job(data_dir: &str, job: &str) {
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

/// The single `lookup` script filter: every imported dictionary, merged by
/// relevance. The first run imports the built-in dictionary and thesaurus.
pub fn lookup(data_dir: &str, wf_dir: &str, query: &str) -> Result<Value> {
    let imported: Value = std::fs::read_to_string(format!("{data_dir}/imported.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({"items": []}));
    let dicts: Vec<(String, String)> = imported["items"]
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
        .unwrap_or_default();
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
        let failure = recent_setup_failure(data_dir);
        if failure.is_none() && !tried_recently(data_dir) {
            let _ = std::fs::write(format!("{data_dir}/setup.tried"), "");
            spawn_job(data_dir, "setup");
        }
        if dicts.is_empty() {
            return Ok(match failure {
                Some(why) => message("Couldn't set up the built-in dictionaries", &why),
                None => setting_up(),
            });
        }
    }

    // Dictionaries imported by older versions need their index built once.
    let stale: Vec<&(String, String)> = dicts
        .iter()
        .filter(|d| !index_ok(data_dir, &d.1))
        .filter(|d| {
            Path::new(&format!("{data_dir}/{}/defs.pack", d.1)).exists()
                || Path::new(&format!("{data_dir}/{}/html", d.1)).is_dir()
        })
        .collect();
    if !stale.is_empty() {
        if let Ok(why) = std::fs::read_to_string(format!("{data_dir}/migrate.failed")) {
            return Ok(message(
                "Upgrading the dictionaries failed",
                &format!("See import.log in the workflow data folder. {}", why.trim()),
            ));
        }
        spawn_job(data_dir, "migrate");
        let mut v = message(
            "Upgrading your dictionaries (one time)…",
            "Searching is available again when it finishes; progress is in the notifications",
        );
        v["rerun"] = json!(2);
        return Ok(v);
    }

    let (scope, word) = parse_scope(query);
    let chosen: Vec<(String, String)> = dicts
        .iter()
        .filter(|d| scope.admits(&d.1))
        .cloned()
        .collect();
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
    search_all(data_dir, &chosen, word)
}

/// Which dictionaries a lookup searches.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Scope {
    All,
    Dictionary,
    Thesaurus,
}

impl Scope {
    fn admits(self, id: &str) -> bool {
        let thesaurus = id.ends_with("-OAWT") || id.ends_with("-OTE");
        match self {
            Scope::All => true,
            Scope::Dictionary => !thesaurus,
            Scope::Thesaurus => thesaurus,
        }
    }
}

/// Split a leading `d:`/`dict:` (dictionary only) or `t:`/`thes:` (thesaurus only) off the query.
fn parse_scope(query: &str) -> (Scope, &str) {
    let q = query.trim();
    for (prefix, scope) in [
        ("dict:", Scope::Dictionary),
        ("d:", Scope::Dictionary),
        ("thes:", Scope::Thesaurus),
        ("t:", Scope::Thesaurus),
    ] {
        if let Some(rest) = q
            .get(..prefix.len())
            .filter(|h| h.eq_ignore_ascii_case(prefix))
        {
            let _ = rest;
            return (scope, q[prefix.len()..].trim());
        }
    }
    (Scope::All, q)
}

/// True if a setup job was started within the last minute (covers a crash that left no marker).
fn tried_recently(data_dir: &str) -> bool {
    fresh_file(&format!("{data_dir}/setup.tried"))
}

fn fresh_file(p: &str) -> bool {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|e| e.as_secs() < 60)
}

/// Why the last automatic setup failed, if it did within the last minute (so a
/// fixed problem is retried soon, but keystrokes don't respawn the job).
fn recent_setup_failure(data_dir: &str) -> Option<String> {
    let p = format!("{data_dir}/setup.failed");
    if fresh_file(&p) {
        std::fs::read_to_string(&p)
            .ok()
            .map(|t| t.trim().to_string())
    } else {
        None
    }
}

/// Drop cached HTML not touched for a week (regenerated on demand). Cheap
/// marker file so the directory walk happens at most once a day.
pub fn prune_cache(data_dir: &str) {
    let marker = format!("{data_dir}/cache/.pruned");
    let fresh = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|e| e.as_secs() < 86400)
        .unwrap_or(false);
    if fresh || !Path::new(&format!("{data_dir}/cache")).is_dir() {
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
    }

    #[test]
    fn thesauri_are_told_apart_from_dictionaries() {
        let (nd, ot) = ("com-apple-dictionary-NOAD", "com-apple-dictionary-OTE");
        assert!(Scope::Dictionary.admits(nd) && !Scope::Dictionary.admits(ot));
        assert!(Scope::Thesaurus.admits(ot) && !Scope::Thesaurus.admits(nd));
        assert!(Scope::All.admits(nd) && Scope::All.admits(ot));
    }
}

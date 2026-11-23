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
    let tmp = format!("{path}.tmp");
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
    let path = materialize(data_dir, id, d, &html).unwrap_or_else(|_| cache_path(data_dir, id, d.name));
    let (sub, pron) = item::display(sel, &String::from_utf8_lossy(&html));
    let subtitle = match label {
        Some(l) => format!("[{l}] {sub}"),
        None => sub,
    };
    let mut item = json!({"title": d.title, "subtitle": subtitle, "arg": path, "quicklookurl": path});
    if !pron.is_empty() {
        item["mods"] = json!({"cmd": {"arg": pron, "subtitle": format!("🗣 {pron}")}});
    }
    item
}

fn message(title: &str, subtitle: &str) -> Value {
    json!({"items": [{"title": title, "subtitle": subtitle, "valid": false}]})
}

/// `bd search <dict_id> <query>`
pub fn search_dict(data_dir: &str, id: &str, q: &str) -> Result<Value> {
    let ix = match Index::open(&index_path(data_dir, id)) {
        Ok(ix) => ix,
        Err(_) => return Ok(message("This dictionary needs a one-time upgrade", "Run: lookup — it will upgrade in the background")),
    };
    let sel = Sel::new();
    let items: Vec<Value> = query::search(&ix, q, LIMIT).iter().map(|h| item_json(&sel, data_dir, id, &ix.doc(h.doc), None)).collect();
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
    for (i, (_, ix)) in opened.iter().enumerate() {
        all.extend(query::search(ix, q, LIMIT).into_iter().map(|h| (i, h)));
    }
    all.sort_by(|a, b| b.1.key.cmp(&a.1.key)); // stable: dictionary order breaks ties
    all.truncate(LIMIT);
    let sel = Sel::new();
    let items: Vec<Value> = all
        .iter()
        .map(|(i, h)| {
            let ((title, id), ix) = (opened[*i].0, &opened[*i].1);
            item_json(&sel, data_dir, id, &ix.doc(h.doc), Some(title))
        })
        .collect();
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
    Command::new("kill").args(["-0", pid]).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

pub fn migration_running(data_dir: &str) -> bool {
    std::fs::read_to_string(format!("{data_dir}/migrate.lock")).map(|p| pid_alive(p.trim())).unwrap_or(false)
}

/// Start `bd migrate` detached from this (short-lived) script-filter process.
fn spawn_migration(data_dir: &str) {
    use std::os::unix::process::CommandExt;
    let Ok(exe) = std::env::current_exe() else { return };
    let log = std::fs::OpenOptions::new().create(true).append(true).open(format!("{data_dir}/import.log"));
    let mut c = Command::new(exe);
    c.args(["migrate", data_dir]).stdin(Stdio::null()).process_group(0);
    match log {
        Ok(l) => {
            c.stdout(l.try_clone().unwrap()).stderr(l);
        }
        Err(_) => {
            c.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    let _ = c.spawn();
}

/// The single `lookup` script filter:
///   "word"              -> every dictionary, merged by relevance
///   "<Dictionary> word" -> only that dictionary
///   ""                  -> hint + list of dictionaries (to scope a search)
pub fn lookup(data_dir: &str, wf_dir: &str, query: &str) -> Result<Value> {
    let imported: Value = std::fs::read_to_string(format!("{data_dir}/imported.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({"items": []}));
    let dicts: Vec<(String, String)> = imported["items"]
        .as_array()
        .map(|a| a.iter().filter_map(|i| Some((i["title"].as_str()?.to_string(), i["arg"].as_str()?.to_string()))).collect())
        .unwrap_or_default();
    if dicts.is_empty() {
        return Ok(message("No dictionaries imported yet", "Type .dict-import to import one"));
    }

    // Dictionaries imported by older versions need their index built once.
    let stale: Vec<&(String, String)> = dicts
        .iter()
        .filter(|d| !index_ok(data_dir, &d.1))
        .filter(|d| Path::new(&format!("{data_dir}/{}/defs.pack", d.1)).exists() || Path::new(&format!("{data_dir}/{}/html", d.1)).is_dir())
        .collect();
    if !stale.is_empty() {
        if let Ok(why) = std::fs::read_to_string(format!("{data_dir}/migrate.failed")) {
            return Ok(message("Upgrading the dictionaries failed", &format!("See import.log in the workflow data folder. {}", why.trim())));
        }
        if !migration_running(data_dir) {
            spawn_migration(data_dir);
        }
        let mut v = message("Upgrading your dictionaries (one time)…", "Searching is available again when it finishes; progress is in the notifications");
        v["rerun"] = json!(2);
        return Ok(v);
    }

    let lq = query.to_lowercase();
    let scoped = dicts
        .iter()
        .filter(|d| lq.starts_with(&format!("{} ", d.0.to_lowercase())))
        .max_by_key(|d| d.0.chars().count())
        .or(if dicts.len() == 1 && !query.is_empty() { dicts.first() } else { None });

    if let Some((title, id)) = scoped {
        let word: String = if lq.starts_with(&format!("{} ", title.to_lowercase())) {
            query.chars().skip(title.chars().count() + 1).collect::<String>().trim_start().to_string()
        } else {
            query.to_string()
        };
        if word.is_empty() {
            return Ok(message(&format!("Search {title}…"), "Type a word"));
        }
        open_extra_pane(wf_dir);
        return search_dict(data_dir, id, &word);
    }
    if !query.is_empty() {
        open_extra_pane(wf_dir);
        return search_all(data_dir, &dicts, query);
    }
    let mut items = vec![json!({"title": "Search all dictionaries…", "subtitle": "Just type a word, or pick one dictionary below", "valid": false})];
    for (title, _) in &dicts {
        items.push(json!({"title": title, "subtitle": format!("Search only {title}"), "autocomplete": format!("{title} "), "valid": false}));
    }
    Ok(json!({ "items": items }))
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
    for e in walkdir::WalkDir::new(format!("{data_dir}/cache")).into_iter().flatten() {
        if e.file_type().is_file() && e.file_name() != ".pruned" {
            if let Some(age) = e.metadata().ok().and_then(|m| m.modified().ok()).map(|t| t.elapsed().unwrap_or_default()) {
                if age > week {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let _ = std::fs::write(&marker, "");
}

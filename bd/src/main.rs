mod appledict;
mod graph;
mod index;
mod item;
mod lookup;
mod pkg;
mod query;
mod say;
mod text;
mod ui;

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::LazyLock;

/// Directory holding info.plist, the server binary, etc. (bd lives there).
fn workflow_dir() -> String {
    if let Ok(d) = std::env::var("BD_WORKFLOW_DIR") {
        return d;
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_string_lossy().into_owned()))
        .unwrap_or_else(|| ".".into())
}

fn default_data_dir(wf_dir: &str) -> Result<String> {
    if let Ok(d) = std::env::var("alfred_workflow_data") {
        return Ok(d);
    }
    let info = plist::Value::from_file(format!("{wf_dir}/info.plist"))?;
    let id = info
        .as_dictionary()
        .and_then(|d| d.get("bundleid"))
        .and_then(|v| v.as_string())
        .context("bundleid")?
        .to_string();
    let home = std::env::var("HOME")?;
    Ok(format!(
        "{home}/Library/Application Support/Alfred/Workflow Data/{id}"
    ))
}

fn dict_id(info: &plist::Dictionary) -> String {
    let id = info
        .get("CFBundleIdentifier")
        .and_then(|v| v.as_string())
        .unwrap_or("dict");
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn dict_name(info: &plist::Dictionary) -> String {
    ["CFBundleDisplayName", "CFBundleName", "CFBundleIdentifier"]
        .iter()
        .find_map(|k| info.get(k).and_then(|v| v.as_string()))
        .unwrap_or("Dictionary")
        .to_string()
}

fn dict_info(dict_path: &str) -> Result<plist::Dictionary> {
    plist::Value::from_file(format!("{dict_path}/Contents/Info.plist"))?
        .into_dictionary()
        .context("Info.plist is not a dictionary")
}

fn body_path(dict_path: &str) -> Option<String> {
    [
        format!("{dict_path}/Contents/Resources/Body.data"),
        format!("{dict_path}/Contents/Body.data"),
    ]
    .into_iter()
    .find(|p| Path::new(p).exists())
}

fn all_dict_paths() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let roots = vec![
        "/System/Library/AssetsV2".to_string(),
        "/Library/Dictionaries".to_string(),
        format!("{home}/Library/Dictionaries"),
    ];
    let mut out = vec![];
    for root in roots {
        let mut it = walkdir::WalkDir::new(&root).sort_by_file_name().into_iter();
        while let Some(entry) = it.next() {
            let Ok(e) = entry else { continue };
            if e.file_type().is_dir() && e.file_name().to_string_lossy().ends_with(".dictionary") {
                it.skip_current_dir();
                let p = e.path().to_string_lossy().into_owned();
                if body_path(&p).is_some() {
                    out.push(p);
                }
            }
        }
    }
    out
}

// literal patterns: compilation cannot fail
static TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"d:title="(.*?)""#).expect("literal pattern"));
static BODY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<body>(.*)</body>").expect("literal pattern"));

/// Open `path` (creating it) and take an exclusive lock, released when the file drops.
fn lock_exclusive(path: &str) -> Result<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("opening {path}"))?;
    f.lock().with_context(|| format!("locking {path}"))?;
    Ok(f)
}

fn read_imported(data_dir: &str) -> Value {
    std::fs::read_to_string(format!("{data_dir}/imported.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(|v| v["items"].is_array())
        .unwrap_or_else(|| json!({"items": []}))
}

/// Values of `key` over the items of imported.json.
fn imported_field(data_dir: &str, key: &str) -> Vec<String> {
    read_imported(data_dir)["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i[key].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Ids of the dictionaries listed in imported.json.
fn imported_ids(data_dir: &str) -> Vec<String> {
    imported_field(data_dir, "arg")
}

/// Display names of the dictionaries listed in imported.json.
fn imported_titles(data_dir: &str) -> Vec<String> {
    imported_field(data_dir, "title")
}

// ---------------------------------------------------------------- packing

struct Entry {
    word: String,
    defn: String,
}

fn group_by_word(defs: Vec<String>) -> Vec<(String, Vec<String>)> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<(String, Vec<String>)> = vec![];
    for defn in defs {
        let Some(c) = TITLE_RE.captures(&defn) else {
            continue;
        };
        let word = c[1].to_string();
        let i = *index.entry(word.clone()).or_insert_with(|| {
            out.push((word.clone(), vec![]));
            out.len() - 1
        });
        out[i].1.push(defn);
    }
    out
}

fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut e = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::best());
    e.write_all(data)?;
    Ok(e.finish()?)
}

/// Cache file name of the n-th entry titled `word`; the separator keeps ("X1", 0) and ("X", 10) apart.
fn entry_name(word: &str, n: usize) -> String {
    format!("{}.html", item::base16(&format!("{word}\u{1}{n}")))
}

/// Deletes its files when dropped (temp files left by a failed build; renamed files are already gone).
struct RemoveOnDrop(Vec<String>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Compress every definition into one pack file (one gzip member each) and
/// build the search index. Returns the number of documents.
/// Holds the dictionary's lock throughout; the new pack and index are renamed into place back to
/// back, pack first. A lookup that pairs the old index with the new pack fails the title check in
/// `lookup::read_definition` and shows "Definition unavailable" for that hit.
fn pack_and_index(data_dir: &str, id: &str, words: Vec<(String, Vec<String>)>) -> Result<usize> {
    std::fs::create_dir_all(format!("{data_dir}/{id}"))?;
    let _lock = lock_exclusive(&format!("{data_dir}/{id}.lock"))?;
    let pack_path = format!("{data_dir}/{id}/defs.pack");
    let pack_tmp = format!("{pack_path}.tmp");
    let index_path = lookup::index_path(data_dir, id);
    let index_new = format!("{index_path}.new");
    let _cleanup = RemoveOnDrop(vec![
        pack_tmp.clone(),
        index_new.clone(),
        format!("{index_new}.tmp"),
    ]);

    let entries: Vec<Entry> = words
        .into_iter()
        .filter(|(w, _)| !w.is_empty())
        .flat_map(|(word, defs)| {
            defs.into_iter().map(move |defn| Entry {
                word: word.clone(),
                defn,
            })
        })
        .collect();
    let total = entries.len();

    // n counts the entries of one word
    let mut counters: HashMap<&str, usize> = HashMap::new();
    let names: Vec<String> = entries
        .iter()
        .map(|e| {
            let n = counters.entry(e.word.as_str()).or_insert(0);
            let name = entry_name(&e.word, *n);
            *n += 1;
            name
        })
        .collect();

    let mut prog = ui::Progress::new(data_dir, "Packing definitions & building index");
    let sel = item::Sel::new();
    let mut builder = index::Builder::new();
    let chunk = 2000;
    let mut pack = std::io::BufWriter::new(std::fs::File::create(&pack_tmp)?);
    let mut offset: u64 = 0;
    let mut done = 0;
    for (ci, group) in entries.chunks(chunk).enumerate() {
        let names_slice = &names[ci * chunk..ci * chunk + group.len()];
        let built: Vec<(Vec<u8>, item::Prepared)> = group
            .par_iter()
            .zip(names_slice.par_iter())
            .map(|(e, name)| {
                Ok((
                    gzip(e.defn.as_bytes())?,
                    item::prepare(&sel, &e.word, name, &e.defn),
                ))
            })
            .collect::<Result<_>>()?;
        for (blob, mut p) in built {
            p.rec.off = offset;
            p.rec.len = blob.len() as u64;
            pack.write_all(&blob)?;
            offset += blob.len() as u64;
            builder.add(&p);
        }
        done += group.len();
        prog.update(done, total, &group[group.len() - 1].word);
    }
    pack.flush()?;
    drop(pack);

    ui::notify(data_dir, "Writing the search index", "almost done…");
    builder.finish(&index_new)?;
    std::fs::rename(&pack_tmp, &pack_path)?;
    std::fs::rename(&index_new, &index_path)?;
    prog.done();
    Ok(total)
}

/// Rebuild only the search index of a dictionary from its existing pack
/// (the pack alone holds every definition, in import order).
fn reindex_from_pack(data_dir: &str, id: &str) -> Result<usize> {
    let _lock = lock_exclusive(&format!("{data_dir}/{id}.lock"))?;
    let pack_path = format!("{data_dir}/{id}/defs.pack");
    let buf = std::fs::read(&pack_path).with_context(|| format!("reading {pack_path}"))?;
    let sel = item::Sel::new();
    let mut builder = index::Builder::new();
    let mut counters: HashMap<String, usize> = HashMap::new();
    let mut prog = ui::Progress::new(data_dir, &format!("Building search index ({id})"));
    let mut pos = 0usize;
    while pos < buf.len() {
        // decode up to 2000 members, remembering where each one sits
        let mut chunk: Vec<(u64, u64, String, String, String)> = vec![];
        while pos < buf.len() && chunk.len() < 2000 {
            let mut dec = flate2::bufread::GzDecoder::new(&buf[pos..]);
            let mut raw = Vec::new();
            dec.read_to_end(&mut raw)
                .with_context(|| format!("corrupt member at byte {pos} of {pack_path}"))?;
            let consumed = buf.len() - pos - dec.into_inner().len();
            let defn = String::from_utf8_lossy(&raw).into_owned();
            let word = TITLE_RE
                .captures(&defn)
                .map(|c| c[1].to_string())
                .unwrap_or_default();
            let n = counters.entry(word.clone()).or_insert(0);
            let name = entry_name(&word, *n);
            *n += 1;
            chunk.push((pos as u64, consumed as u64, word, name, defn));
            pos += consumed;
        }
        let built: Vec<item::Prepared> = chunk
            .par_iter()
            .map(|(off, len, word, name, defn)| {
                let mut p = item::prepare(&sel, word, name, defn);
                p.rec.off = *off;
                p.rec.len = *len;
                p
            })
            .collect();
        for p in &built {
            builder.add(p);
        }
        let last_word = chunk.last().map_or("", |c| c.2.as_str());
        prog.update(pos, buf.len(), last_word);
    }
    let n = builder.len();
    builder.finish(&lookup::index_path(data_dir, id))?;
    prog.done();
    Ok(n)
}

// ----------------------------------------------------------- workflow graph

fn create_workflow_objects(wf_dir: &str, name: &str, id: &str) -> Result<()> {
    use graph::{Graph, CMD};
    let mut wf = Graph::load(&format!("{wf_dir}/info.plist"))?;
    let junction = wf.new_junction()?;
    let hotkey = wf.new_hotkey(name)?;
    let fallback = wf.new_fallback(name, &format!("search {name}"))?;
    let sf = wf.new_script_filter(
        &format!("search {name}"),
        &format!(
            "open -g ./AlfredExtraPane.app\n\nquery=\"$1\"\n\n./search.sh \"$query\" \"{id}\""
        ),
        name,
    )?;
    let label = |wf: &Graph, l: &str| {
        wf.uid_with_label(l)
            .with_context(|| format!("workflow object '{l}' not found"))
    };
    let router = label(&wf, "router")?;
    let (router_out, rows) = wf.add_router_output(&router, id)?;
    let ipa = label(&wf, "Play pronunciation audio")?;
    let opener = label(&wf, "Open definition in browser")?;
    wf.connect(&hotkey, &sf, 0, None)?;
    wf.connect(&fallback, &sf, 0, None)?;
    wf.connect(&junction, &sf, 0, None)?;
    wf.connect(&sf, &ipa, CMD, None)?;
    wf.connect(&sf, &opener, 0, None)?;
    wf.connect(&router, &junction, 0, Some(&router_out))?;

    let off = 180 * (rows as i64 - 1);
    for (obj, dummy) in [
        (&junction, "dummy junction"),
        (&hotkey, "dummy hotkey"),
        (&fallback, "dummy fallback search"),
        (&sf, "dummy script filter"),
    ] {
        let d = label(&wf, dummy)?;
        let (x, y) = (wf.x(&d), wf.y(&d));
        wf.set_pos(obj, "xpos", x)?;
        wf.set_pos(obj, "ypos", y + off)?;
    }
    let y = wf.y(&sf);
    wf.set_pos(&opener, "ypos", y)?;
    wf.save()
}

// ----------------------------------------------------------------- commands

/// Import one dictionary; `objects` also gives it its own keyword and hotkey in the workflow.
fn cmd_import(dict_path: &str, data_dir: &str, objects: bool) -> Result<()> {
    let wf_dir = workflow_dir();
    let info = dict_info(dict_path)?;
    let id = dict_id(&info);
    let name = dict_name(&info);
    let body = body_path(dict_path).context("no Body.data in dictionary")?;

    ui::notify(
        data_dir,
        &format!("Importing {name}"),
        "reading dictionary…",
    );
    let words = group_by_word(appledict::definitions(&body)?);
    let n = pack_and_index(data_dir, &id, words)?;

    // Serialise the read-modify-write of imported.json (setup and .dict-import can overlap).
    let _lock = lock_exclusive(&format!("{data_dir}/imported.lock"))?;
    let mut imported = read_imported(data_dir);
    let Some(items) = imported["items"].as_array_mut() else {
        bail!("imported.json has no items list");
    };
    if items.iter().any(|i| i["arg"] == id.as_str()) {
        ui::notify(
            data_dir,
            &format!("Re-imported {name}"),
            &format!("{n} entries (already in the workflow)"),
        );
        return Ok(());
    }
    items.push(json!({"title": name, "arg": id}));
    if objects {
        create_workflow_objects(&wf_dir, &name, &id)?;
    }
    let tmp = format!("{data_dir}/imported.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&imported)?)?;
    std::fs::rename(&tmp, format!("{data_dir}/imported.json"))?;
    ui::notify(
        data_dir,
        &format!("Imported {name}"),
        &format!("{n} entries"),
    );
    Ok(())
}

fn cmd_list_unimported(data_dir: &str) -> Result<()> {
    let imported = imported_titles(data_dir);
    let mut items = vec![];
    for p in all_dict_paths() {
        match dict_info(&p) {
            Ok(info) => {
                let n = dict_name(&info);
                if !imported.contains(&n) {
                    items.push(json!({"title": n, "arg": p}));
                }
            }
            Err(e) => eprintln!("Unable to process \"{p}\": {e}"),
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"items": items}))?
    );
    Ok(())
}

/// Apple's own English dictionaries and thesauri (US and UK), imported automatically.
fn is_builtin(id: &str) -> bool {
    ["NOAD", "OAWT", "ODE", "OTE"]
        .iter()
        .any(|s| id == format!("com-apple-dictionary-{s}"))
}

/// Paths of the built-in dictionaries found on this Mac, one per dictionary.
fn builtin_dict_paths() -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    all_dict_paths()
        .into_iter()
        .filter_map(|p| {
            let id = dict_id(&dict_info(&p).ok()?);
            (is_builtin(&id) && seen.insert(id.clone())).then_some((p, id))
        })
        .collect()
}

/// First-run import of the built-in dictionary and thesaurus; no workflow objects.
fn cmd_setup(data_dir: &str) -> Result<()> {
    let _ = std::fs::remove_file(format!("{data_dir}/setup.failed"));
    std::fs::write(
        format!("{data_dir}/setup.lock"),
        std::process::id().to_string(),
    )?; // the spawning lookup already holds it
    let r = setup_inner(data_dir);
    if let Err(e) = &r {
        // marker first, so a lookup never sees neither the lock nor the marker
        let _ = std::fs::write(format!("{data_dir}/setup.failed"), format!("{e}"));
        ui::log_line(data_dir, &format!("SETUP ERROR: {e:?}"));
        // a user who already has dictionaries gets no repeated pop-up on each retry
        if imported_ids(data_dir).is_empty() {
            ui::notify(data_dir, "Setup failed", &format!("{e}"));
        }
    }
    let _ = std::fs::remove_file(format!("{data_dir}/setup.lock"));
    r
}

fn setup_inner(data_dir: &str) -> Result<()> {
    let found = builtin_dict_paths();
    if found.is_empty() {
        bail!("No built-in dictionary found. Open Dictionary.app > Settings and tick New Oxford American Dictionary and the Thesaurus, then try again (or .dict-import another)");
    }
    let have = imported_ids(data_dir);
    let mut failed = vec![];
    for (path, id) in &found {
        if have.contains(id) {
            continue;
        }
        if let Err(e) = cmd_import(path, data_dir, false) {
            ui::log_line(data_dir, &format!("ERROR importing {id}: {e:?}"));
            failed.push(format!("{id}: {e}"));
        }
    }
    if !failed.is_empty() {
        bail!("{}", failed.join("; "));
    }
    std::fs::write(format!("{data_dir}/setup.done"), "")?;
    ui::notify(
        data_dir,
        "Dictionary and thesaurus ready",
        "just type: lookup <word>",
    );
    Ok(())
}

fn read_legacy_html_dir(dir: &str) -> Result<Vec<(String, Vec<String>)>> {
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".html"))
        .collect();
    names.sort();
    let mut defs = vec![];
    for n in names {
        let text = String::from_utf8_lossy(&std::fs::read(format!("{dir}/{n}"))?).into_owned();
        if let Some(m) = BODY_RE.captures(&text) {
            let defn = m[1].to_string();
            if TITLE_RE.is_match(&defn) {
                defs.push(defn);
            }
        }
    }
    Ok(group_by_word(defs))
}

/// Bring every imported dictionary to the current layout: a pack of
/// definitions plus the built-in search index. Works from the old HTML-folder
/// layout or from the Meilisearch-era pack (no re-import from Apple's files).
fn cmd_migrate(data_dir: &str, keep_backup: bool) -> Result<()> {
    let _ = std::fs::remove_file(format!("{data_dir}/migrate.failed"));
    std::fs::write(
        format!("{data_dir}/migrate.lock"),
        std::process::id().to_string(),
    )?; // child already holds it when spawned by lookup
    let r = migrate_inner(data_dir, keep_backup);
    let _ = std::fs::remove_file(format!("{data_dir}/migrate.lock"));
    match &r {
        Ok(()) => ui::notify(data_dir, "Dictionaries upgraded", "search is ready"),
        Err(e) => {
            ui::log_line(data_dir, &format!("MIGRATE ERROR: {e:?}"));
            let _ = std::fs::write(format!("{data_dir}/migrate.failed"), format!("{e}"));
            ui::notify(data_dir, "Upgrade failed", &format!("{e}"));
        }
    }
    r
}

fn migrate_inner(data_dir: &str, keep_backup: bool) -> Result<()> {
    let wf_dir = workflow_dir();
    let ids = imported_ids(data_dir);
    let mut done_any = false;
    for id in &ids {
        if lookup::index_ok(data_dir, id) {
            continue;
        }
        let pack = format!("{data_dir}/{id}/defs.pack");
        let html = format!("{data_dir}/{id}/html");
        if Path::new(&pack).exists() {
            println!("[{id}] building search index from the existing pack…");
            let n = reindex_from_pack(data_dir, id)?;
            println!("[{id}] {n} entries indexed");
        } else if Path::new(&html).is_dir() {
            println!("[{id}] reading old HTML files…");
            let words = read_legacy_html_dir(&html)?;
            println!("[{id}] {} words; packing and indexing…", words.len());
            pack_and_index(data_dir, id, words)?;
            if !keep_backup {
                let _ = std::fs::remove_dir_all(&html);
            }
        } else {
            println!("[{id}] no data found, skipping");
            continue;
        }
        done_any = true;
    }
    if !done_any {
        println!("Nothing to migrate.");
    }
    // The old Meilisearch server and its database are no longer used.
    let _ = std::process::Command::new("pkill")
        .args(["-x", "alfred-dict-server"])
        .stderr(std::process::Stdio::null())
        .status();
    let _ = std::fs::remove_file(format!("{data_dir}/server.pid"));
    if !keep_backup {
        for d in ["db", "db.new", "db.premigrate"] {
            let _ = std::fs::remove_dir_all(format!("{data_dir}/{d}"));
        }
        for f in ["db.log", "db.new.log"] {
            let _ = std::fs::remove_file(format!("{data_dir}/{f}"));
        }
        let _ = std::fs::remove_dir_all(format!("{data_dir}/.venv"));
        let _ = std::fs::remove_file(format!("{wf_dir}/alfred-dict-server"));
    }
    println!("Migration complete.");
    Ok(())
}

fn cmd_factory_reset(data_dir: &str) -> Result<()> {
    let wf = workflow_dir();
    let home = std::env::var("HOME").unwrap_or_default();
    if data_dir.len() < 10
        || data_dir == home
        || !data_dir.contains("Workflow Data") && std::env::var("alfred_workflow_data").is_err()
    {
        bail!("refusing to delete suspicious data folder {data_dir:?}");
    }
    if !Path::new(&format!("{wf}/info.plist.orig")).exists() {
        bail!("{wf}/info.plist.orig is missing; nothing was deleted");
    }
    std::fs::remove_dir_all(data_dir)?;
    std::fs::copy(format!("{wf}/info.plist"), format!("{wf}/info.plist.bkp"))?;
    std::fs::copy(format!("{wf}/info.plist.orig"), format!("{wf}/info.plist"))?;
    std::fs::create_dir_all(data_dir)?;
    Ok(())
}

/// The workflow folder of `bd upgrade <repo> [workflow_dir] [flags]`: the first non-flag argument after the repo.
fn upgrade_dest(args: &[String]) -> Option<&str> {
    args.iter()
        .skip(2)
        .map(String::as_str)
        .find(|a| !a.starts_with("--"))
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let keep_backup = args.iter().any(|a| a == "--keep-backup");
    args.retain(|a| a != "--keep-backup");
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let wf_dir = workflow_dir();
    let data_arg = |n: usize| -> Result<String> {
        let d = match args.get(n) {
            Some(d) => d.clone(),
            None => default_data_dir(&wf_dir)?,
        };
        std::fs::create_dir_all(&d)?;
        Ok(d)
    };
    match cmd {
        "import" => {
            let dict = args.get(1).context("usage: bd import <dict_path> [data_dir]")?;
            let data = data_arg(2)?;
            ui::log_line(&data, &format!("\n=== import {dict}  (bd {}, workflow_dir={wf_dir})", env!("CARGO_PKG_VERSION")));
            match cmd_import(dict, &data, true) {
                Ok(()) => ui::log_line(&data, "OK: import finished"),
                Err(e) => {
                    ui::log_line(&data, &format!("ERROR: {e:?}"));
                    ui::notify(&data, "Import failed", &format!("{e}"));
                    return Err(e);
                }
            }
            Ok(())
        }
        "list-unimported" => cmd_list_unimported(&data_arg(1)?),
        "migrate" => cmd_migrate(&data_arg(1)?, keep_backup),
        "setup" => cmd_setup(&data_arg(1)?),
        "search" => {
            // bd search <dict_id> <query>
            let id = args.get(1).context("usage: bd search <dict_id> <query>")?;
            let q = args.get(2).map(String::as_str).unwrap_or("");
            let data = data_arg(3)?;
            println!("{}", lookup::search_dict(&data, id, q)?);
            Ok(())
        }
        "lookup" => {
            let q = args.get(1).map(String::as_str).unwrap_or("");
            let data = data_arg(2)?;
            let out = lookup::lookup(&data, &wf_dir, q)?;
            println!("{out}");
            lookup::prune_cache(&data);
            Ok(())
        }
        "factory-reset" => cmd_factory_reset(&data_arg(1)?),
        "package" => {
            pkg::package(args.get(1).context("usage: bd package <repo> <out_dir>")?, args.get(2).context("usage: bd package <repo> <out_dir>")?)
        }
        "upgrade" => {
            let repo = args.get(1).context("usage: bd upgrade <repo> [workflow_dir] [--no-migrate] [--keep-backup]")?;
            let no_migrate = args.iter().any(|a| a == "--no-migrate");
            let dest = upgrade_dest(&args);
            pkg::upgrade(repo, dest, |dest, _| {
                if no_migrate {
                    return Ok(());
                }
                let data = default_data_dir(dest)?;
                std::fs::create_dir_all(&data)?;
                println!("Building the search index for your imported dictionaries (a minute or two)...");
                cmd_migrate(&data, keep_backup)
            })
        }
        "say-ipa" => {
            let ipa = args.get(1).context("usage: bd say-ipa <ipa> [--voice Salli]")?;
            let voice = args.iter().position(|a| a == "--voice").and_then(|i| args.get(i + 1)).map(String::as_str).unwrap_or("Salli");
            say::say_ipa(ipa, voice, args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1)).map(String::as_str), !args.iter().any(|a| a == "--no-play"))
        }
        "version" => {
            println!("bd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => bail!("usage: bd <import|search|lookup|setup|list-unimported|migrate|factory-reset|package|upgrade|say-ipa|version> ..."),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:?}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_apples_own_dictionaries_are_builtin() {
        assert!(is_builtin("com-apple-dictionary-NOAD"));
        assert!(is_builtin("com-apple-dictionary-OAWT"));
        assert!(!is_builtin("com-apple-dictionary-Wikipedia"));
        assert!(!is_builtin("com-apple-dictionary-NOAD-extra"));
    }

    #[test]
    fn legacy_html_dir_preserves_body_whitespace() {
        let dir = std::env::temp_dir().join(format!("bd-legacy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let body = "\n<d:entry d:title=\"word\"><span>sub</span>\n</d:entry>\n";
        std::fs::write(
            dir.join("a.html"),
            format!("<html><body>{body}</body></html>"),
        )
        .unwrap();
        let words = read_legacy_html_dir(dir.to_str().unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(words, vec![("word".to_string(), vec![body.to_string()])]);
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bd-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn defn(word: &str, filler: usize) -> String {
        format!(
            "<d:entry d:title=\"{word}\">{}</d:entry>",
            "lorem ipsum ".repeat(filler)
        )
    }

    /// Every index entry must decode, from its offset in the pack, to a definition with its own title.
    fn assert_pack_matches_index(data_dir: &str, id: &str) {
        let idx = index::Index::open(&lookup::index_path(data_dir, id)).unwrap();
        let pack = std::fs::read(format!("{data_dir}/{id}/defs.pack")).unwrap();
        for i in 0..idx.n as u32 {
            let d = idx.doc(i);
            let end = (d.off + d.len) as usize;
            assert!(end <= pack.len(), "doc {i} lies outside the pack");
            let mut html = String::new();
            flate2::read::GzDecoder::new(&pack[d.off as usize..end])
                .read_to_string(&mut html)
                .unwrap_or_else(|e| panic!("doc {i} is not a gzip member: {e}"));
            assert!(
                html.contains(&format!("d:title=\"{}\"", d.title)),
                "doc {i} has the wrong body"
            );
        }
    }

    fn words_set(prefix: &str, n: usize) -> Vec<(String, Vec<String>)> {
        (0..n)
            .map(|i| {
                let w = format!("{prefix}{i}");
                (w.clone(), vec![defn(&w, 40 + i % 7)])
            })
            .collect()
    }

    #[test]
    fn concurrent_imports_of_one_dictionary_stay_consistent() {
        let dir = tmp_dir("race");
        let data = dir.to_str().unwrap().to_string();
        for _ in 0..3 {
            let hs: Vec<_> = ["a", "b"]
                .into_iter()
                .map(|p| {
                    let data = data.clone();
                    std::thread::spawn(move || {
                        pack_and_index(&data, "dict", words_set(p, 2500)).unwrap();
                    })
                })
                .collect();
            for h in hs {
                h.join().unwrap();
            }
            assert_pack_matches_index(&data, "dict");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cache_names_are_unique_across_titles() {
        let dir = tmp_dir("names");
        let data = dir.to_str().unwrap();
        let words = vec![
            ("X1".to_string(), vec![defn("X1", 1)]),
            ("X".to_string(), (0..11).map(|_| defn("X", 1)).collect()),
        ];
        pack_and_index(data, "dict", words).unwrap();
        let idx = index::Index::open(&lookup::index_path(data, "dict")).unwrap();
        let names: std::collections::HashSet<String> = (0..idx.n as u32)
            .map(|i| idx.doc(i).name.to_string())
            .collect();
        assert_eq!(names.len(), idx.n);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn upgrade_takes_the_folder_after_any_flag() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            upgrade_dest(&a(&["upgrade", ".", "--no-migrate", "/wf"])),
            Some("/wf")
        );
        assert_eq!(
            upgrade_dest(&a(&["upgrade", ".", "/wf", "--no-migrate"])),
            Some("/wf")
        );
        assert_eq!(upgrade_dest(&a(&["upgrade", ".", "--no-migrate"])), None);
        assert_eq!(upgrade_dest(&a(&["upgrade", "."])), None);
    }

    #[test]
    fn entry_names_differ_for_colliding_title_counts() {
        assert_ne!(entry_name("X1", 0), entry_name("X", 10));
        assert_eq!(entry_name("X", 3), entry_name("X", 3));
    }

    #[test]
    fn reindex_from_pack_reproduces_the_import_names() {
        let dir = tmp_dir("reindex");
        let data = dir.to_str().unwrap();
        let words = vec![
            ("X1".to_string(), vec![defn("X1", 1)]),
            ("X".to_string(), (0..11).map(|_| defn("X", 1)).collect()),
        ];
        pack_and_index(data, "dict", words).unwrap();
        let names = |data: &str| -> Vec<String> {
            let idx = index::Index::open(&lookup::index_path(data, "dict")).unwrap();
            (0..idx.n as u32)
                .map(|i| idx.doc(i).name.to_string())
                .collect()
        };
        let before = names(data);
        assert_eq!(reindex_from_pack(data, "dict").unwrap(), 12);
        assert_eq!(names(data), before);
        assert_pack_matches_index(data, "dict");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_build_leaves_no_temp_files() {
        let dir = tmp_dir("failbuild");
        let data = dir.to_str().unwrap();
        // a non-empty directory where the final index goes makes the last rename fail
        std::fs::create_dir_all(format!("{data}/dict/index.bin/x")).unwrap();
        assert!(pack_and_index(data, "dict", words_set("w", 5)).is_err());
        for f in ["defs.pack.tmp", "index.bin.new", "index.bin.new.tmp"] {
            assert!(!dir.join("dict").join(f).exists(), "{f} was left behind");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

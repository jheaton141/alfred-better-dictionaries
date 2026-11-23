mod appledict;
mod graph;
mod item;
mod meili;
mod ui;

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

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
    let id = info.as_dictionary().and_then(|d| d.get("bundleid")).and_then(|v| v.as_string()).context("bundleid")?.to_string();
    let home = std::env::var("HOME")?;
    Ok(format!("{home}/Library/Application Support/Alfred/Workflow Data/{id}"))
}

fn dict_id(info: &plist::Dictionary) -> String {
    let id = info.get("CFBundleIdentifier").and_then(|v| v.as_string()).unwrap_or("dict");
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' }).collect()
}

fn dict_name(info: &plist::Dictionary) -> String {
    ["CFBundleDisplayName", "CFBundleName", "CFBundleIdentifier"]
        .iter()
        .find_map(|k| info.get(*k).and_then(|v| v.as_string()))
        .unwrap_or("Dictionary")
        .to_string()
}

fn dict_info(dict_path: &str) -> Result<plist::Dictionary> {
    plist::Value::from_file(format!("{dict_path}/Contents/Info.plist"))?
        .into_dictionary()
        .context("Info.plist is not a dictionary")
}

fn body_path(dict_path: &str) -> Option<String> {
    [format!("{dict_path}/Contents/Resources/Body.data"), format!("{dict_path}/Contents/Body.data")]
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

fn read_imported(data_dir: &str) -> Value {
    std::fs::read_to_string(format!("{data_dir}/imported.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({"items": []}))
}

// ---------------------------------------------------------------- packing

struct Entry {
    word: String,
    defn: String,
}

fn group_by_word(defs: Vec<String>) -> Vec<(String, Vec<String>)> {
    let re = Regex::new(r#"d:title="(.*?)""#).unwrap();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<(String, Vec<String>)> = vec![];
    for defn in defs {
        let Some(c) = re.captures(&defn) else { continue };
        let word = c[1].to_string();
        let i = *index.entry(word.clone()).or_insert_with(|| {
            out.push((word.clone(), vec![]));
            out.len() - 1
        });
        out[i].1.push(defn);
    }
    out
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::GzBuilder::new().mtime(0).write(Vec::new(), flate2::Compression::best());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// Compress every definition into one pack file (one gzip member each) and
/// push search documents to the index. Returns the number of documents.
fn pack_and_index(
    data_dir: &str,
    id: &str,
    words: Vec<(String, Vec<String>)>,
) -> Result<usize> {
    let wf_dir = workflow_dir();
    let cache_dir = format!("{data_dir}/cache/{id}");
    std::fs::create_dir_all(&cache_dir)?;
    std::fs::create_dir_all(format!("{data_dir}/{id}"))?;
    let pack_path = format!("{data_dir}/{id}/defs.pack");

    let entries: Vec<Entry> = words
        .into_iter()
        .filter(|(w, _)| !w.is_empty())
        .flat_map(|(word, defs)| {
            defs.into_iter().map(move |defn| Entry { word: word.clone(), defn })
        })
        .collect();
    let total = entries.len();

    // filename = base16(word + n) where n counts within the word
    let mut counters: HashMap<&str, usize> = HashMap::new();
    let names: Vec<String> = entries
        .iter()
        .map(|e| {
            let n = counters.entry(e.word.as_str()).or_insert(0);
            let name = format!("{}.html", item::base16(&format!("{}{}", e.word, n)));
            *n += 1;
            name
        })
        .collect();

    let mut prog = ui::Progress::new(data_dir, "Packing definitions & building index");
    meili::ensure_server(&wf_dir, data_dir, &format!("{data_dir}/db"))?;
    meili::create_index(id)?;

    let sel = item::Sel::new();
    let chunk = 2000;
    let mut tasks = vec![];
    let mut pack = std::io::BufWriter::new(std::fs::File::create(&pack_path)?);
    let mut offset: u64 = 0;
    let mut done = 0;
    for (ci, group) in entries.chunks(chunk).enumerate() {
        let names_slice = &names[ci * chunk..ci * chunk + group.len()];
        let built: Vec<(Vec<u8>, Value)> = group
            .par_iter()
            .zip(names_slice.par_iter())
            .map(|(e, name)| {
                let html_path = format!("{cache_dir}/{name}");
                let blob = gzip(e.defn.as_bytes());
                let it = item::make_item(&sel, &e.word, name, &e.defn, &html_path);
                (blob, it)
            })
            .collect();
        let mut docs = Vec::with_capacity(built.len());
        for (blob, mut it) in built {
            it["off"] = json!(offset);
            it["len"] = json!(blob.len());
            pack.write_all(&blob)?;
            offset += blob.len() as u64;
            docs.push(it);
        }
        tasks.push(meili::add_documents(id, &docs)?);
        done += group.len();
        prog.update(done, total, &group[group.len() - 1].word);
    }
    pack.flush()?;
    drop(pack);

    ui::notify(data_dir, "Building index for instant search", "waiting for the index to finish…");
    for t in tasks {
        meili::wait_task(t, false)?;
    }
    prog.done();
    Ok(total)
}

// ----------------------------------------------------------- workflow graph

fn create_workflow_objects(wf_dir: &str, name: &str, id: &str) -> Result<()> {
    use graph::{Graph, CMD};
    let mut wf = Graph::load(&format!("{wf_dir}/info.plist"))?;
    let junction = wf.new_junction();
    let hotkey = wf.new_hotkey(name);
    let fallback = wf.new_fallback(name, &format!("search {name}"));
    let sf = wf.new_script_filter(
        &format!("search {name}"),
        &format!("open -g ./AlfredExtraPane.app\n\nquery=\"$1\"\n\n./search.sh \"$query\" \"{id}\""),
        name,
    );
    let label = |wf: &Graph, l: &str| wf.uid_with_label(l).with_context(|| format!("workflow object '{l}' not found"));
    let router = label(&wf, "router")?;
    let (router_out, rows) = wf.add_router_output(&router, id)?;
    let ipa = label(&wf, "Play pronunciation audio")?;
    let opener = label(&wf, "Open definition in browser")?;
    wf.connect(&hotkey, &sf, 0, None);
    wf.connect(&fallback, &sf, 0, None);
    wf.connect(&junction, &sf, 0, None);
    wf.connect(&sf, &ipa, CMD, None);
    wf.connect(&sf, &opener, 0, None);
    wf.connect(&router, &junction, 0, Some(&router_out));

    let off = 180 * (rows as i64 - 1);
    for (obj, dummy) in [
        (&junction, "dummy junction"),
        (&hotkey, "dummy hotkey"),
        (&fallback, "dummy fallback search"),
        (&sf, "dummy script filter"),
    ] {
        let d = label(&wf, dummy)?;
        let (x, y) = (wf.x(&d), wf.y(&d));
        wf.set_pos(obj, "xpos", x);
        wf.set_pos(obj, "ypos", y + off);
    }
    let y = wf.y(&sf);
    wf.set_pos(&opener, "ypos", y);
    wf.save()
}

// ----------------------------------------------------------------- commands

fn cmd_import(dict_path: &str, data_dir: &str) -> Result<()> {
    let wf_dir = workflow_dir();
    let info = dict_info(dict_path)?;
    let id = dict_id(&info);
    let name = dict_name(&info);
    let body = body_path(dict_path).context("no Body.data in dictionary")?;

    ui::notify(data_dir, &format!("Importing {name}"), "reading dictionary…");
    let words = group_by_word(appledict::definitions(&body)?);
    let n = pack_and_index(data_dir, &id, words)?;

    let mut imported = read_imported(data_dir);
    imported["items"].as_array_mut().unwrap().push(json!({"title": name, "arg": id}));
    create_workflow_objects(&wf_dir, &name, &id)?;
    std::fs::write(format!("{data_dir}/imported.json"), serde_json::to_string_pretty(&imported)?)?;
    ui::notify(data_dir, &format!("Imported {name}"), &format!("{n} entries"));
    Ok(())
}

fn cmd_list_unimported(data_dir: &str) -> Result<()> {
    let imported: Vec<String> = read_imported(data_dir)["items"]
        .as_array()
        .map(|a| a.iter().filter_map(|i| i["title"].as_str().map(String::from)).collect())
        .unwrap_or_default();
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
    println!("{}", serde_json::to_string_pretty(&json!({"items": items}))?);
    Ok(())
}

fn read_legacy_html_dir(dir: &str) -> Result<Vec<(String, Vec<String>)>> {
    let body_re = Regex::new(r"(?s)<body>(.*)</body>").unwrap();
    let title_re = Regex::new(r#"d:title="(.*?)""#).unwrap();
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".html"))
        .collect();
    names.sort();
    let mut defs = vec![];
    for n in names {
        let text = String::from_utf8_lossy(&std::fs::read(format!("{dir}/{n}"))?).into_owned();
        if let Some(m) = body_re.captures(&text) {
            let defn = m[1].trim().to_string();
            if title_re.is_match(&defn) {
                defs.push(defn);
            }
        }
    }
    Ok(group_by_word(defs))
}

fn cmd_migrate(data_dir: &str, keep_backup: bool) -> Result<()> {
    let wf_dir = workflow_dir();
    let imported = read_imported(data_dir);
    let ids: Vec<String> = imported["items"].as_array().map(|a| a.iter().filter_map(|i| i["arg"].as_str().map(String::from)).collect()).unwrap_or_default();
    let todo: Vec<&String> = ids.iter().filter(|d| Path::new(&format!("{data_dir}/{d}/html")).is_dir()).collect();
    if todo.is_empty() {
        println!("Nothing to migrate.");
        return Ok(());
    }
    println!("Migrating: {}", todo.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    let new_db = format!("{data_dir}/db.new");
    meili::kill_server(data_dir);
    let _ = std::fs::remove_dir_all(&new_db);
    meili::start_server(&wf_dir, data_dir, &new_db)?;
    // pack_and_index talks to whichever server is up (the new one)
    for id in &todo {
        println!("[{id}] reading old HTML files…");
        let words = read_legacy_html_dir(&format!("{data_dir}/{id}/html"))?;
        println!("[{id}] {} words; packing and indexing…", words.len());
        pack_and_index(data_dir, id, words)?;
    }
    meili::kill_server(data_dir);
    let old = format!("{data_dir}/db.premigrate");
    let _ = std::fs::remove_dir_all(&old);
    if Path::new(&format!("{data_dir}/db")).exists() {
        std::fs::rename(format!("{data_dir}/db"), &old)?;
    }
    std::fs::rename(&new_db, format!("{data_dir}/db"))?;
    let _ = std::fs::rename(format!("{new_db}.log"), format!("{data_dir}/db.log"));
    if !keep_backup {
        let _ = std::fs::remove_dir_all(&old);
        for id in todo {
            let _ = std::fs::remove_dir_all(format!("{data_dir}/{id}/html"));
        }
    }
    println!("Migration complete.");
    Ok(())
}

fn cmd_factory_reset(data_dir: &str) -> Result<()> {
    let wf = workflow_dir();
    std::fs::remove_dir_all(data_dir)?;
    std::fs::copy(format!("{wf}/info.plist"), format!("{wf}/info.plist.bkp"))?;
    std::fs::copy(format!("{wf}/info.plist.orig"), format!("{wf}/info.plist"))?;
    std::fs::create_dir_all(data_dir)?;
    Ok(())
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let keep_backup = args.iter().any(|a| a == "--keep-backup");
    args.retain(|a| !a.starts_with("--"));
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
            match cmd_import(dict, &data) {
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
        "factory-reset" => cmd_factory_reset(&data_arg(1)?),
        "version" => {
            println!("bd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => bail!("usage: bd <import|list-unimported|migrate|factory-reset|version> ..."),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:?}");
        std::process::exit(1);
    }
}

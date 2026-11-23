//! `bd package` (release-ready workflow folder) and `bd upgrade` (in-place
//! upgrade of an installed workflow), replacing the old Python scripts.
use anyhow::{bail, Context, Result};
use plist::{Dictionary, Value};
use std::path::Path;

/// Files that make up the workflow (besides `bd` itself).
pub const FILES: &[&str] = &["AlfredExtraPane.app", "dict-entry.css", "extra-pane-config.json", "icon.png", "info.plist", "lookup.sh", "README.md", "sayipa.sh", "search.sh", "setup.sh"];
/// Scripts refreshed on upgrade.
const SCRIPTS: &[&str] = &["lookup.sh", "search.sh", "sayipa.sh", "setup.sh"];
/// Left behind by older versions.
const OBSOLETE: &[&str] = &["pyapp", "uv", "sayipa.py", "lib.sh", "alfred-dict-server", "jq", "mkapps.sh"];

fn dict(v: &mut Value) -> Result<&mut Dictionary> {
    v.as_dictionary_mut().context("not a dictionary")
}

fn uid_of(o: &Value) -> Option<String> {
    o.as_dictionary()?.get("uid")?.as_string().map(String::from)
}

/// Strip what Alfred must not export: `noexport` objects, their connections,
/// variables flagged "don't export", all but the first router condition; add the readme.
pub fn make_export_ready(wf: &mut Value, readme: &str) -> Result<()> {
    let root = dict(wf)?;
    root.insert("variablesdontexport".into(), Value::Array(vec![]));
    let uidata = root.get("uidata").and_then(|v| v.as_dictionary()).context("uidata")?.clone();
    let noexport: Vec<String> = uidata.iter().filter(|(_, d)| d.as_dictionary().is_some_and(|d| d.contains_key("noexport"))).map(|(k, _)| k.clone()).collect();
    let routers: Vec<String> = uidata.iter().filter(|(_, d)| d.as_dictionary().and_then(|d| d.get("note")).and_then(|n| n.as_string()) == Some("router")).map(|(k, _)| k.clone()).collect();
    if let Some(Value::Dictionary(u)) = root.get_mut("uidata") {
        for k in &noexport {
            u.remove(k);
        }
    }
    if let Some(Value::Dictionary(c)) = root.get_mut("connections") {
        for k in &noexport {
            c.remove(k);
        }
        for (_, list) in c.iter_mut() {
            if let Value::Array(a) = list {
                a.retain(|ci| !ci.as_dictionary().and_then(|d| d.get("destinationuid")).and_then(|u| u.as_string()).is_some_and(|u| noexport.iter().any(|n| n == u)));
            }
        }
    }
    if let Some(Value::Array(objs)) = root.get_mut("objects") {
        objs.retain(|o| !uid_of(o).is_some_and(|u| noexport.contains(&u)));
        for o in objs.iter_mut() {
            if uid_of(o).is_some_and(|u| routers.contains(&u)) {
                if let Some(Value::Array(conds)) = o.as_dictionary_mut().and_then(|d| d.get_mut("config")).and_then(|c| c.as_dictionary_mut()).and_then(|c| c.get_mut("conditions")) {
                    conds.truncate(1);
                }
            }
        }
    }
    root.insert("readme".into(), Value::String(readme.into()));
    Ok(())
}

fn copy_recursive(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_recursive(&e.path(), &to.join(e.file_name()))?;
        }
    } else {
        std::fs::copy(from, to).with_context(|| format!("copying {}", from.display()))?;
    }
    Ok(())
}

/// `bd package <repo> <out_dir>`: `<out_dir>` becomes the exportable workflow folder.
pub fn package(repo: &str, out: &str) -> Result<()> {
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out)?;
    for f in FILES {
        copy_recursive(&Path::new(repo).join(f), &Path::new(out).join(f))?;
    }
    std::fs::copy(std::env::current_exe()?, format!("{out}/bd"))?;
    let mut wf = Value::from_file(format!("{out}/info.plist"))?;
    make_export_ready(&mut wf, &std::fs::read_to_string(format!("{repo}/README.md"))?)?;
    let name = wf.as_dictionary().and_then(|d| d.get("name")).and_then(|n| n.as_string()).unwrap_or("workflow").to_string();
    wf.to_file_xml(format!("{out}/info.plist"))?;
    println!("{name}");
    Ok(())
}

fn find_installed() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let mut roots = vec![format!("{home}/Library/Application Support/Alfred/Alfred.alfredpreferences")];
    if let Ok(o) = std::process::Command::new("defaults").args(["read", "com.runningwithcrayons.Alfred", "syncfolder"]).output() {
        let s = String::from_utf8_lossy(&o.stdout).trim().replacen('~', &home, 1);
        if !s.is_empty() {
            roots.insert(0, format!("{s}/Alfred.alfredpreferences"));
        }
    }
    for r in roots {
        for e in std::fs::read_dir(format!("{r}/workflows")).into_iter().flatten().flatten() {
            let p = e.path().join("info.plist");
            if let Ok(Value::Dictionary(d)) = Value::from_file(&p) {
                if d.get("bundleid").and_then(|b| b.as_string()) == Some("mr.pennyworth.betterDicts") {
                    return Some(e.path().to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

/// Make an installed workflow's own plist use the new scripts, leaving every
/// dictionary, keyword and hotkey it holds untouched.
fn patch_plist(path: &str, new: &Value) -> Result<()> {
    let mut wf = Value::from_file(path)?;
    let new_objs: Vec<Value> = new.as_dictionary().and_then(|d| d.get("objects")).and_then(|o| o.as_array()).cloned().unwrap_or_default();
    if let Some(Value::Array(objs)) = dict(&mut wf)?.get_mut("objects") {
        for o in objs.iter_mut() {
            let Some(uid) = uid_of(o) else { continue };
            let Some(n) = new_objs.iter().find(|n| uid_of(n).as_deref() == Some(&uid)) else { continue };
            let (Some(oc), Some(nc)) = (o.as_dictionary_mut().and_then(|d| d.get_mut("config")).and_then(|c| c.as_dictionary_mut()), n.as_dictionary().and_then(|d| d.get("config")).and_then(|c| c.as_dictionary())) else { continue };
            for k in ["script", "scriptfile", "alfredfiltersresults", "alfredfiltersresultsmatchmode", "subtext"] {
                if oc.contains_key("script") || k == "script" {
                    if let Some(v) = nc.get(k) {
                        oc.insert(k.into(), v.clone());
                    }
                }
            }
        }
    }
    if let Some(v) = new.as_dictionary().and_then(|d| d.get("version")) {
        dict(&mut wf)?.insert("version".into(), v.clone());
    }
    wf.to_file_xml(path)?;
    Ok(())
}

/// `bd upgrade <repo> [workflow_dir]`
pub fn upgrade(repo: &str, dest: Option<&str>, migrate: impl FnOnce(&str, &str) -> Result<()>) -> Result<()> {
    let dest = match dest {
        Some(d) => d.to_string(),
        None => find_installed().context("Couldn't find the installed workflow; pass its folder (Alfred > Workflows > right-click > Open in Finder).")?,
    };
    if !Path::new(&format!("{dest}/info.plist")).exists() {
        bail!("{dest} has no info.plist");
    }
    let new = Value::from_file(format!("{repo}/info.plist"))?;
    let ver = new.as_dictionary().and_then(|d| d.get("version")).and_then(|v| v.as_string()).unwrap_or("new").to_string();
    println!("Upgrading {dest} -> {ver}");
    for name in ["info.plist", "info.plist.orig"] {
        let p = format!("{dest}/{name}");
        if Path::new(&p).exists() {
            std::fs::copy(&p, format!("{p}.pre-{ver}"))?;
            patch_plist(&p, &new)?;
            println!("  patched {name} (backup: {name}.pre-{ver})");
        }
    }
    for s in SCRIPTS {
        std::fs::copy(format!("{repo}/{s}"), format!("{dest}/{s}"))?;
    }
    let me = std::env::current_exe()?;
    let target = format!("{dest}/bd");
    let tmp = format!("{dest}/bd.new");
    std::fs::copy(&me, &tmp)?;
    std::fs::rename(&tmp, &target)?; // atomic; also fine if the old bd is running
    println!("  installed bd and scripts");
    migrate(&dest, "")?;
    for o in OBSOLETE {
        let p = Path::new(&dest).join(o);
        if p.is_dir() {
            let _ = std::fs::remove_dir_all(&p);
        } else if p.exists() {
            let _ = std::fs::remove_file(&p);
        }
    }
    println!("Done. Existing dictionaries, keywords and hotkeys are untouched.");
    Ok(())
}

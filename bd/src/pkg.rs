//! `bd package` (release-ready workflow folder) and `bd upgrade` (in-place
//! upgrade of an installed workflow), replacing the old Python scripts.
use anyhow::{bail, Context, Result};
use plist::{Dictionary, Value};
use std::path::Path;

use crate::graph;

/// Files that make up the workflow (besides `bd` itself).
pub const FILES: &[&str] = &[
    "AlfredExtraPane.app",
    "dict-entry.css",
    "extra-pane-config.json",
    "icon.png",
    "info.plist",
    "lookup.sh",
    "README.md",
    "sayipa.sh",
    "search.sh",
    "setup.sh",
];
/// Scripts refreshed on upgrade.
const SCRIPTS: &[&str] = &["lookup.sh", "search.sh", "sayipa.sh", "setup.sh"];
/// Left behind by older versions.
const OBSOLETE: &[&str] = &[
    "pyapp",
    "uv",
    "sayipa.py",
    "lib.sh",
    "alfred-dict-server",
    "jq",
    "mkapps.sh",
];

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
    let uidata = root
        .get("uidata")
        .and_then(|v| v.as_dictionary())
        .context("uidata")?
        .clone();
    let noexport: Vec<String> = uidata
        .iter()
        .filter(|(_, d)| {
            d.as_dictionary()
                .is_some_and(|d| d.contains_key("noexport"))
        })
        .map(|(k, _)| k.clone())
        .collect();
    let routers: Vec<String> = uidata
        .iter()
        .filter(|(_, d)| {
            d.as_dictionary()
                .and_then(|d| d.get("note"))
                .and_then(|n| n.as_string())
                == Some("router")
        })
        .map(|(k, _)| k.clone())
        .collect();
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
                a.retain(|ci| {
                    !ci.as_dictionary()
                        .and_then(|d| d.get("destinationuid"))
                        .and_then(|u| u.as_string())
                        .is_some_and(|u| noexport.iter().any(|n| n == u))
                });
            }
        }
    }
    if let Some(Value::Array(objs)) = root.get_mut("objects") {
        objs.retain(|o| !uid_of(o).is_some_and(|u| noexport.contains(&u)));
        for o in objs.iter_mut() {
            if uid_of(o).is_some_and(|u| routers.contains(&u)) {
                if let Some(Value::Array(conds)) = o
                    .as_dictionary_mut()
                    .and_then(|d| d.get_mut("config"))
                    .and_then(|c| c.as_dictionary_mut())
                    .and_then(|c| c.get_mut("conditions"))
                {
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
    make_export_ready(
        &mut wf,
        &std::fs::read_to_string(format!("{repo}/README.md"))?,
    )?;
    let name = wf
        .as_dictionary()
        .and_then(|d| d.get("name"))
        .and_then(|n| n.as_string())
        .unwrap_or("workflow")
        .to_string();
    wf.to_file_xml(format!("{out}/info.plist"))?;
    println!("{name}");
    Ok(())
}

/// Alfred preference folders to search, synced location first.
fn pref_roots(home: &str, synced: Option<&str>) -> Vec<String> {
    let mut roots = Vec::new();
    if let Some(s) = synced.map(str::trim).filter(|s| !s.is_empty()) {
        let s = s.replacen('~', home, 1);
        roots.push(format!(
            "{}/Alfred.alfredpreferences",
            s.trim_end_matches('/')
        ));
    }
    roots.push(format!(
        "{home}/Library/Application Support/Alfred/Alfred.alfredpreferences"
    ));
    roots
}

/// Alfred 5/6 keep the sync folder in the Alfred-Preferences domain (older builds used Alfred's).
fn synced_folder() -> Option<String> {
    [
        "com.runningwithcrayons.Alfred-Preferences",
        "com.runningwithcrayons.Alfred",
    ]
    .iter()
    .find_map(|d| {
        let o = std::process::Command::new("defaults")
            .args(["read", d, "syncfolder"])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (o.status.success() && !s.is_empty()).then_some(s)
    })
}

fn find_installed() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let roots = pref_roots(&home, synced_folder().as_deref());
    for r in roots {
        for e in std::fs::read_dir(format!("{r}/workflows"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = e.path().join("info.plist");
            if let Ok(Value::Dictionary(d)) = Value::from_file(&p) {
                if d.get("bundleid").and_then(|b| b.as_string())
                    == Some("mr.pennyworth.betterDicts")
                {
                    return Some(e.path().to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

/// The script that prints an entry's text, and the Text View that shows it.
const TEXT_VIEW_OBJECTS: [&str; 2] = [
    "5D7A3F10-2B4E-4C8A-9E61-7A1C0B94D2E3",
    "B2E8C4A6-1D93-4F57-A0B8-6C3E9F1A7D52",
];

/// Give an installed workflow the Text View objects, and wire Option-Return from
/// every search that already has a Command-Return pronunciation action.
fn add_text_view(wf: &mut Value, new: &Value) -> Result<()> {
    let [action, _] = TEXT_VIEW_OBJECTS;
    let root = dict(wf)?;
    let find = |r: &Dictionary, uid: &str| {
        r.get("objects").and_then(Value::as_array).and_then(|a| {
            a.iter()
                .find(|o| uid_of(o).as_deref() == Some(uid))
                .cloned()
        })
    };
    let Some(new_root) = new.as_dictionary().filter(|n| find(n, action).is_some()) else {
        return Ok(());
    };
    for uid in TEXT_VIEW_OBJECTS {
        let Some(obj) = find(new_root, uid).filter(|_| find(root, uid).is_none()) else {
            continue;
        };
        if let Some(Value::Array(objs)) = root.get_mut("objects") {
            objs.push(obj);
        }
        for key in ["uidata", "connections"] {
            let from = new_root.get(key).and_then(Value::as_dictionary);
            if let (Some(v), Some(Value::Dictionary(to))) =
                (from.and_then(|f| f.get(uid)), root.get_mut(key))
            {
                to.insert(uid.into(), v.clone());
            }
        }
    }
    let is_mod = |c: &Value, m: i64| {
        c.as_dictionary()
            .and_then(|c| c.get("modifiers"))
            .and_then(Value::as_signed_integer)
            == Some(m)
    };
    if let Some(Value::Dictionary(conns)) = root.get_mut("connections") {
        for (src, list) in conns.iter_mut() {
            let Value::Array(list) = list else { continue };
            let wired = list.iter().any(|c| {
                c.as_dictionary()
                    .and_then(|c| c.get("destinationuid"))
                    .and_then(Value::as_string)
                    == Some(action)
            });
            if src != action && !wired && list.iter().any(|c| is_mod(c, graph::CMD)) {
                let mut c = Dictionary::new();
                c.insert("destinationuid".into(), Value::String(action.into()));
                c.insert("modifiers".into(), Value::Integer(graph::ALT.into()));
                c.insert("modifiersubtext".into(), Value::String(String::new()));
                c.insert("vitoclose".into(), Value::Boolean(false));
                list.push(Value::Dictionary(c));
            }
        }
    }
    Ok(())
}

/// Make an installed workflow's own plist use the new scripts, leaving every
/// dictionary, keyword and hotkey it holds untouched.
fn patch_plist(path: &str, new: &Value) -> Result<()> {
    let mut wf = Value::from_file(path)?;
    let new_objs: Vec<Value> = new
        .as_dictionary()
        .and_then(|d| d.get("objects"))
        .and_then(|o| o.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(Value::Array(objs)) = dict(&mut wf)?.get_mut("objects") {
        for o in objs.iter_mut() {
            let Some(uid) = uid_of(o) else { continue };
            let Some(n) = new_objs.iter().find(|n| uid_of(n).as_deref() == Some(&uid)) else {
                continue;
            };
            let (Some(oc), Some(nc)) = (
                o.as_dictionary_mut()
                    .and_then(|d| d.get_mut("config"))
                    .and_then(|c| c.as_dictionary_mut()),
                n.as_dictionary()
                    .and_then(|d| d.get("config"))
                    .and_then(|c| c.as_dictionary()),
            ) else {
                continue;
            };
            for k in [
                "script",
                "scriptfile",
                "alfredfiltersresults",
                "alfredfiltersresultsmatchmode",
                "subtext",
            ] {
                if oc.contains_key("script") || k == "script" {
                    if let Some(v) = nc.get(k) {
                        oc.insert(k.into(), v.clone());
                    }
                }
            }
        }
    }
    add_text_view(&mut wf, new)?;
    if let Some(v) = new.as_dictionary().and_then(|d| d.get("version")) {
        dict(&mut wf)?.insert("version".into(), v.clone());
    }
    wf.to_file_xml(path)?;
    Ok(())
}

/// `bd upgrade <repo> [workflow_dir]`
pub fn upgrade(
    repo: &str,
    dest: Option<&str>,
    migrate: impl FnOnce(&str, &str) -> Result<()>,
) -> Result<()> {
    let dest = match dest {
        Some(d) => d.to_string(),
        None => find_installed().context("Couldn't find the installed workflow; pass its folder (Alfred > Workflows > right-click > Open in Finder).")?,
    };
    if !Path::new(&format!("{dest}/info.plist")).exists() {
        bail!("{dest} has no info.plist");
    }
    let new = Value::from_file(format!("{repo}/info.plist"))?;
    let ver = new
        .as_dictionary()
        .and_then(|d| d.get("version"))
        .and_then(|v| v.as_string())
        .unwrap_or("new")
        .to_string();
    println!("Upgrading {dest} -> {ver}");
    for name in ["info.plist", "info.plist.orig"] {
        let p = format!("{dest}/{name}");
        if Path::new(&p).exists() {
            let bak = format!("{p}.pre-{ver}");
            if !Path::new(&bak).exists() {
                std::fs::copy(&p, &bak)?;
            }
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

#[cfg(test)]
mod tests {
    use super::{add_text_view, patch_plist, pref_roots, uid_of, TEXT_VIEW_OBJECTS};
    use plist::Value;

    fn plist(xml: &str) -> Value {
        let doc = format!("<?xml version=\"1.0\"?><plist version=\"1.0\">{xml}</plist>");
        Value::from_reader_xml(doc.as_bytes()).unwrap()
    }

    fn conn(dst: &str, modifiers: i64) -> String {
        format!("<dict><key>destinationuid</key><string>{dst}</string><key>modifiers</key><integer>{modifiers}</integer></dict>")
    }

    #[test]
    fn upgrade_adds_the_text_view_once_and_wires_every_search() {
        let [action, view] = TEXT_VIEW_OBJECTS;
        let obj = |uid: &str| format!("<dict><key>uid</key><string>{uid}</string></dict>");
        let old = plist(&format!(
            "<dict><key>objects</key><array>{}{}{}</array><key>uidata</key><dict/><key>connections</key><dict><key>S1</key><array>{}{}</array><key>S2</key><array>{}{}</array><key>S3</key><array>{}</array></dict></dict>",
            obj("S1"), obj("S2"), obj("S3"),
            conn("IPA", 1048576), conn("OPEN", 0),
            conn("IPA", 1048576), conn("OPEN", 0),
            conn("OPEN", 0)
        ));
        let new = plist(&format!(
            "<dict><key>objects</key><array>{}{}</array><key>uidata</key><dict><key>{action}</key><dict><key>note</key><string>Show definition as text</string></dict><key>{view}</key><dict/></dict><key>connections</key><dict><key>{action}</key><array>{}</array><key>{view}</key><array/></dict></dict>",
            obj(action), obj(view), conn(view, 0)
        ));
        let mut wf = old.clone();
        add_text_view(&mut wf, &new).unwrap();
        add_text_view(&mut wf, &new).unwrap();
        let root = wf.as_dictionary().unwrap();
        assert_eq!(root["objects"].as_array().unwrap().len(), 5);
        let uidata = root["uidata"].as_dictionary().unwrap();
        assert!(uidata.contains_key(action) && uidata.contains_key(view));
        let conns = root["connections"].as_dictionary().unwrap();
        let alt_to_action = |src: &str| {
            conns[src]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| {
                    let c = c.as_dictionary().unwrap();
                    c["destinationuid"].as_string() == Some(action)
                        && c["modifiers"].as_signed_integer() == Some(524288)
                })
                .count()
        };
        assert_eq!(
            (
                alt_to_action("S1"),
                alt_to_action("S2"),
                alt_to_action("S3")
            ),
            (1, 1, 0)
        );
        assert_eq!(conns[action].as_array().unwrap().len(), 1);
        // the Text View alone was deleted: it comes back, the action is not duplicated
        let mut cut = wf.clone();
        if let Value::Array(o) = cut.as_dictionary_mut().unwrap().get_mut("objects").unwrap() {
            o.retain(|o| uid_of(o).as_deref() != Some(view));
        }
        add_text_view(&mut cut, &new).unwrap();
        assert_eq!(
            cut.as_dictionary().unwrap()["objects"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        // a plist whose release lacks the objects is left alone
        let mut bare = old.clone();
        add_text_view(&mut bare, &old).unwrap();
        assert_eq!(bare, old);
    }

    #[test]
    fn patching_an_installed_plist_adds_the_text_view() {
        let [action, view] = TEXT_VIEW_OBJECTS;
        let obj = |uid: &str| format!("<dict><key>uid</key><string>{uid}</string></dict>");
        let old = plist("<dict><key>objects</key><array/><key>uidata</key><dict/><key>connections</key><dict/></dict>");
        let new = plist(&format!(
            "<dict><key>objects</key><array>{}{}</array><key>uidata</key><dict/><key>connections</key><dict/><key>version</key><string>9</string></dict>",
            obj(action), obj(view)
        ));
        let path = std::env::temp_dir().join(format!("bd-patch-{}.plist", uuid::Uuid::new_v4()));
        old.to_file_xml(&path).unwrap();
        patch_plist(path.to_str().unwrap(), &new).unwrap();
        let got = Value::from_file(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            got.as_dictionary().unwrap()["objects"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn synced_folder_is_searched_before_the_default() {
        let r = pref_roots("/Users/me", Some("~/Documents/Personal\n"));
        assert_eq!(
            r[0],
            "/Users/me/Documents/Personal/Alfred.alfredpreferences"
        );
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn no_sync_folder_falls_back_to_application_support() {
        let r = pref_roots("/Users/me", None);
        assert_eq!(
            r,
            ["/Users/me/Library/Application Support/Alfred/Alfred.alfredpreferences"]
        );
        assert_eq!(pref_roots("/Users/me", Some("  ")), r);
    }
}

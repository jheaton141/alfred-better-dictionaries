//! Alfred workflow graph editing (port of WorkflowGraph.py).
use anyhow::{Context, Result};
use plist::{Dictionary, Value};

pub const CMD: i64 = 1048576;

pub struct Graph {
    path: String,
    pub root: Dictionary,
}

fn d(pairs: Vec<(&str, Value)>) -> Dictionary {
    let mut m = Dictionary::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    m
}
fn s(x: &str) -> Value {
    Value::String(x.into())
}
fn i(x: i64) -> Value {
    Value::Integer(x.into())
}
fn b(x: bool) -> Value {
    Value::Boolean(x)
}

impl Graph {
    pub fn load(path: &str) -> Result<Self> {
        let v = Value::from_file(path).with_context(|| format!("reading {path}"))?;
        Ok(Graph {
            path: path.into(),
            root: v
                .into_dictionary()
                .context("info.plist is not a dictionary")?,
        })
    }

    pub fn save(&self) -> Result<()> {
        plist::to_file_xml(&self.path, &Value::Dictionary(self.root.clone()))?;
        Ok(())
    }

    fn arr(&mut self, key: &str) -> &mut Vec<Value> {
        self.root
            .get_mut(key)
            .and_then(|v| v.as_array_mut())
            .expect(key)
    }
    fn dict(&mut self, key: &str) -> &mut Dictionary {
        self.root
            .get_mut(key)
            .and_then(|v| v.as_dictionary_mut())
            .expect(key)
    }

    fn add_object(
        &mut self,
        typ: &str,
        version: i64,
        config: Option<Dictionary>,
        note: Option<&str>,
    ) -> String {
        let uid = uuid::Uuid::new_v4().to_string().to_uppercase();
        let mut o = d(vec![
            ("type", s(typ)),
            ("uid", s(&uid)),
            ("version", i(version)),
        ]);
        if let Some(c) = config {
            o.insert("config".into(), Value::Dictionary(c));
        }
        self.arr("objects").push(Value::Dictionary(o));
        let mut ui = Dictionary::new();
        if let Some(n) = note {
            ui.insert("note".into(), s(n));
        }
        ui.insert("noexport".into(), i(1));
        ui.insert("xpos".into(), i(0));
        ui.insert("ypos".into(), i(0));
        self.dict("uidata")
            .insert(uid.clone(), Value::Dictionary(ui));
        self.dict("connections")
            .insert(uid.clone(), Value::Array(vec![]));
        uid
    }

    pub fn new_junction(&mut self) -> String {
        self.add_object("alfred.workflow.utility.junction", 1, None, None)
    }

    pub fn new_hotkey(&mut self, note: &str) -> String {
        let c = d(vec![
            ("action", i(0)),
            ("argument", i(0)),
            ("focusedappvariable", b(false)),
            ("focusedappvariablename", s("")),
            ("hotkey", i(0)),
            ("hotmod", i(0)),
            ("hotstring", s("")),
            ("leftcursor", b(false)),
            ("modsmode", i(0)),
            ("relatedAppsMode", i(0)),
        ]);
        self.add_object("alfred.workflow.trigger.hotkey", 2, Some(c), Some(note))
    }

    pub fn new_fallback(&mut self, note: &str, title: &str) -> String {
        self.add_object(
            "alfred.workflow.trigger.fallback",
            1,
            Some(d(vec![("text", s(title))])),
            Some(note),
        )
    }

    pub fn new_script_filter(&mut self, title: &str, script: &str, note: &str) -> String {
        let c = d(vec![
            ("title", s(title)),
            ("script", s(script)),
            ("runningsubtext", s("loading...")),
            ("subtext", s("")),
            ("alfredfiltersresults", b(false)),
            ("alfredfiltersresultsmatchmode", i(1)),
            ("argumenttreatemptyqueryasnil", b(true)),
            ("argumenttrimmode", i(0)),
            ("argumenttype", i(0)),
            ("escaping", i(102)),
            ("queuedelaycustom", i(3)),
            ("queuedelayimmediatelyinitially", b(true)),
            ("queuedelaymode", i(0)),
            ("queuemode", i(2)),
            ("scriptargtype", i(1)),
            ("scriptfile", s("")),
            ("type", i(0)),
            ("withspace", b(true)),
        ]);
        self.add_object("alfred.workflow.input.scriptfilter", 3, Some(c), Some(note))
    }

    pub fn uid_with_label(&self, label: &str) -> Option<String> {
        self.root
            .get("uidata")?
            .as_dictionary()?
            .iter()
            .find_map(|(uid, data)| {
                (data.as_dictionary()?.get("note")?.as_string()? == label).then(|| uid.clone())
            })
    }

    fn object_mut(&mut self, uid: &str) -> Option<&mut Dictionary> {
        self.arr("objects")
            .iter_mut()
            .filter_map(|o| o.as_dictionary_mut())
            .find(|o| o.get("uid").and_then(|u| u.as_string()) == Some(uid))
    }

    pub fn add_router_output(&mut self, router_uid: &str, output: &str) -> Result<(String, usize)> {
        let out_uid = uuid::Uuid::new_v4().to_string().to_uppercase();
        let cond = d(vec![
            ("inputstring", s("{var:dict_id}")),
            ("matchcasesensitive", b(false)),
            ("matchmode", i(0)),
            ("matchstring", s(output)),
            ("outputlabel", s(output)),
            ("uid", s(&out_uid)),
        ]);
        let router = self
            .object_mut(router_uid)
            .context("router object missing")?;
        let conds = router
            .get_mut("config")
            .and_then(|c| c.as_dictionary_mut())
            .and_then(|c| c.get_mut("conditions"))
            .and_then(|c| c.as_array_mut())
            .context("router has no conditions")?;
        conds.push(Value::Dictionary(cond));
        Ok((out_uid, conds.len()))
    }

    pub fn connect(&mut self, src: &str, dst: &str, modifiers: i64, src_out: Option<&str>) {
        let mut c = d(vec![
            ("destinationuid", s(dst)),
            ("modifiers", i(modifiers)),
            ("modifiersubtext", s("")),
            ("vitoclose", b(false)),
        ]);
        if let Some(o) = src_out {
            c.insert("sourceoutputuid".into(), s(o));
        }
        let conns = self.dict("connections");
        if !conns.contains_key(src) {
            conns.insert(src.into(), Value::Array(vec![]));
        }
        conns
            .get_mut(src)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(Value::Dictionary(c));
    }

    fn coord(&self, uid: &str, key: &str) -> i64 {
        self.root
            .get("uidata")
            .and_then(|u| u.as_dictionary())
            .and_then(|u| u.get(uid))
            .and_then(|u| u.as_dictionary())
            .and_then(|u| u.get(key))
            .map(|v| {
                v.as_signed_integer()
                    .or_else(|| v.as_real().map(|r| r as i64))
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }
    pub fn x(&self, uid: &str) -> i64 {
        self.coord(uid, "xpos")
    }
    pub fn y(&self, uid: &str) -> i64 {
        self.coord(uid, "ypos")
    }
    pub fn set_pos(&mut self, uid: &str, key: &str, v: i64) {
        if let Some(u) = self
            .dict("uidata")
            .get_mut(uid)
            .and_then(|u| u.as_dictionary_mut())
        {
            u.insert(key.into(), i(v));
        }
    }
}

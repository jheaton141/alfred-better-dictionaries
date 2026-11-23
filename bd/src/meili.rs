//! Minimal Meilisearch client + server process management.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn base() -> String {
    let ip = std::env::var("SEARCH_IP").unwrap_or_else(|_| "127.0.0.1".into());
    let port = std::env::var("SEARCH_PORT").unwrap_or_else(|_| "6789".into());
    format!("http://{ip}:{port}")
}

pub fn healthy() -> bool {
    ureq::get(&format!("{}/health", base())).timeout(Duration::from_secs(1)).call().is_ok()
}

fn pid_of(data_dir: &str) -> Option<String> {
    std::fs::read_to_string(format!("{data_dir}/server.pid")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// A server we started earlier that is still booting (pidfile + kill -0).
fn process_running(data_dir: &str) -> bool {
    pid_of(data_dir)
        .map(|pid| Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false))
        .unwrap_or(false)
}

/// Make sure a server serving `db_path` is up (start one if none runs).
pub fn ensure_server(workflow_dir: &str, data_dir: &str, db_path: &str) -> Result<()> {
    if !healthy() && !process_running(data_dir) {
        start_server(workflow_dir, data_dir, db_path)?;
    }
    let t = Instant::now();
    while !healthy() {
        if t.elapsed() > Duration::from_secs(60) {
            bail!("search server did not become healthy (see {db_path}.log)");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

pub fn start_server(workflow_dir: &str, data_dir: &str, db_path: &str) -> Result<()> {
    let log = std::fs::File::create(format!("{db_path}.log"))?;
    let child = Command::new(format!("{workflow_dir}/alfred-dict-server"))
        .args(["--db-path", db_path, "--http-addr"])
        .arg(base().trim_start_matches("http://"))
        .args(["--http-payload-size-limit", "1000000000", "--no-analytics"])
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("starting alfred-dict-server")?;
    let _ = std::fs::write(format!("{data_dir}/server.pid"), child.id().to_string());
    Ok(()) // intentionally not waited on: it outlives us
}

pub fn kill_server(data_dir: &str) {
    if let Some(pid) = pid_of(data_dir) {
        let _ = Command::new("kill").arg(&pid).stderr(Stdio::null()).status();
    }
    // servers started by other means (older workflow versions)
    let _ = Command::new("pkill").args(["-x", "alfred-dict-server"]).stderr(Stdio::null()).status();
    let t = Instant::now();
    while (healthy() || process_running(data_dir)) && t.elapsed() < Duration::from_secs(15) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_file(format!("{data_dir}/server.pid"));
}

fn task_uid(v: Value) -> Result<u64> {
    v["taskUid"].as_u64().context("no taskUid in response")
}

fn post(path: &str, body: &Value) -> Result<Value> {
    ureq::post(&format!("{}{path}", base()))
        .timeout(Duration::from_secs(600))
        .send_json(body.clone())
        .map_err(|e| anyhow::anyhow!("POST {path}: {e}"))?
        .into_json()
        .map_err(Into::into)
}

pub fn wait_task(uid: u64, tolerate_failure: bool) -> Result<()> {
    let mut delay = 50;
    loop {
        let t: Value = ureq::get(&format!("{}/tasks/{uid}", base())).call()?.into_json()?;
        match t["status"].as_str() {
            Some("succeeded") => return Ok(()),
            Some("failed") | Some("canceled") => {
                if tolerate_failure {
                    return Ok(());
                }
                bail!("search index task {uid} failed: {}", t["error"]);
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(delay));
        delay = (delay * 2).min(2000);
    }
}

/// Create the index and apply the same settings the Python importer used.
pub fn create_index(id: &str) -> Result<()> {
    let t = task_uid(post("/indexes", &json!({"uid": id, "primaryKey": "id"}))?)?;
    wait_task(t, true)?; // already exists is fine
    let settings = json!({
        "searchableAttributes": ["title", "forms", "fulltext"],
        "rankingRules": ["exactness", "attribute", "typo", "words", "proximity"],
        "proximityPrecision": "byAttribute",
        "displayedAttributes": ["off", "len", "arg", "mods", "title", "subtitle", "quicklookurl"],
    });
    let r: Value = ureq::request("PATCH", &format!("{}/indexes/{id}/settings", base()))
        .send_json(settings)
        .map_err(|e| anyhow::anyhow!("settings: {e}"))?
        .into_json()?;
    wait_task(task_uid(r)?, false)
}

pub fn add_documents(id: &str, docs: &[Value]) -> Result<u64> {
    task_uid(post(&format!("/indexes/{id}/documents"), &Value::Array(docs.to_vec()))?)
}

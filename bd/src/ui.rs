//! Progress reporting: macOS notifications (osascript, works everywhere,
//! no helper app) plus a status file and a log the user can read.
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

pub fn notify(data_dir: &str, title: &str, message: &str) {
    let clean = |s: &str| s.replace(['"', '\\'], "'").chars().take(120).collect::<String>();
    let _ = Command::new("osascript")
        .args([
            "-e",
            &format!(
                "display notification \"{}\" with title \"BetterDict\" subtitle \"{}\"",
                clean(message),
                clean(title)
            ),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let _ = std::fs::write(format!("{data_dir}/progress.txt"), format!("{title}: {message}\n"));
}

pub struct Progress {
    data_dir: String,
    title: String,
    start: Instant,
    last_decile: i64,
}

impl Progress {
    pub fn new(data_dir: &str, title: &str) -> Self {
        notify(data_dir, title, "started");
        Progress { data_dir: data_dir.into(), title: title.into(), start: Instant::now(), last_decile: -1 }
    }
    pub fn update(&mut self, done: usize, total: usize, msg: &str) {
        let pct = if total == 0 { 100.0 } else { done as f64 * 100.0 / total as f64 };
        let decile = (pct as i64) / 10;
        if decile <= self.last_decile {
            return;
        }
        self.last_decile = decile;
        let el = self.start.elapsed().as_secs_f64();
        let rem = (el * 100.0 / pct.max(0.01) - el) as u64;
        notify(&self.data_dir, &self.title, &format!("{}%  [ETA {:02}:{:02}] {}", pct as i64, rem / 60, rem % 60, msg));
    }
    pub fn done(&self) {
        notify(&self.data_dir, &self.title, "done");
    }
}

pub fn log_line(data_dir: &str, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{data_dir}/import.log")) {
        let _ = writeln!(f, "{line}");
    }
}

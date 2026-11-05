//! Port of src/main/log.js: one log file in the data dir (agu-chat.log), also echoed to stdout in dev.
//! JS `log(...args)` joined its arguments; here callers pass one already formatted message (`format!`),
//! and an error is shown as its message (Rust errors carry no stack).
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Logger {
    path: PathBuf,
    packaged: bool,
    file: Mutex<Option<std::fs::File>>,
}

impl Logger {
    pub fn new(dir: &Path, packaged: bool) -> Self {
        Logger { path: dir.join("agu-chat.log"), packaged, file: Mutex::new(None) }
    }

    pub fn log(&self, msg: &str) {
        let line = format!("{} {}", crate::util::iso_ms(crate::util::now_ms()), msg);
        if !self.packaged {
            println!("{line}");
        }
        let mut guard = self.file.lock().unwrap();
        if guard.is_none() {
            if let Some(parent) = self.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            *guard = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).ok();
        }
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

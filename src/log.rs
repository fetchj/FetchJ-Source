use crate::ids::IdMap;
use crate::privacy::scrub;
use once_cell::sync::Lazy;
use regex::Regex;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub type Ids = Arc<Mutex<IdMap>>;
pub type Sink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn word(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

static HEADER_RX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\[\d+\] ").unwrap());
static STATUS_RX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^ {6}[A-Z][A-Z _]*: ").unwrap());

pub fn console_class(msg: &str, level: Level) -> Option<u32> {
    if level != Level::Info {
        return Some(1);
    }
    if HEADER_RX.is_match(msg) {
        return None;
    }
    if STATUS_RX.is_match(msg) {
        return Some(1);
    }
    if msg.trim().is_empty() {
        return Some(2);
    }
    if msg.starts_with("      ") || msg.starts_with("   time (") {
        return Some(255);
    }
    if msg.starts_with("   ") {
        return Some(2);
    }
    Some(1)
}

struct Inner {
    file: Option<File>,
    pending_header: Option<String>,
}

#[derive(Clone)]
pub struct Logger {
    inner: Arc<Mutex<Inner>>,
    pub path: Option<PathBuf>,
    ids: Option<Ids>,
    console_level: u32,
    sink: Option<Sink>,
}

impl Logger {
    pub fn new(log_dir: &Path, run_id: &str, ids: Option<Ids>, console_level: u32, sink: Option<Sink>) -> Logger {
        let _ = std::fs::create_dir_all(log_dir);
        let path = log_dir.join(format!("FetchJ_log_{run_id}.txt"));
        let file = OpenOptions::new().create(true).append(true).open(&path).ok();
        Logger {
            inner: Arc::new(Mutex::new(Inner {
                file,
                pending_header: None,
            })),
            path: Some(path),
            ids,
            console_level,
            sink,
        }
    }

    pub fn memory(sink: Option<Sink>) -> Logger {
        Logger {
            inner: Arc::new(Mutex::new(Inner {
                file: None,
                pending_header: None,
            })),
            path: None,
            ids: None,
            console_level: 255,
            sink,
        }
    }

    pub fn filter(&self, text: &str) -> String {
        scrub(&crate::i18n::tr(text), self.ids.as_deref())
    }

    pub fn log(&self, msg: &str, level: Level) {
        let text = self.filter(msg);
        let line = format!("{} [{}] {}", crate::pyfmt::now_human(), crate::i18n::tr(level.word()), text);
        let mut inner = self.inner.lock().unwrap();
        if let Some(f) = inner.file.as_mut() {
            let _ = write!(f, "{line}{}", crate::pyfmt::NL);
            let _ = f.flush();
        }
        let Some(sink) = &self.sink else { return };
        match console_class(msg, level) {
            None => {
                if self.console_level >= 2 {
                    sink(&text);
                } else {
                    inner.pending_header = Some(text);
                }
            }
            Some(c) if c <= self.console_level => {
                if let Some(h) = inner.pending_header.take() {
                    sink(&h);
                }
                sink(&text);
            }
            _ => {}
        }
    }

    pub fn info(&self, msg: impl AsRef<str>) {
        self.log(msg.as_ref(), Level::Info)
    }
    pub fn warn(&self, msg: impl AsRef<str>) {
        self.log(msg.as_ref(), Level::Warn)
    }
    pub fn error(&self, msg: impl AsRef<str>) {
        self.log(msg.as_ref(), Level::Error)
    }
}

pub fn short_error(e: &str) -> String {
    let first = e.lines().next().unwrap_or("").trim();
    crate::privacy::redact(first)
}

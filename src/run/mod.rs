pub mod download;
pub mod entry;
pub mod keep;
pub mod list;
pub mod media;
pub mod session;

use crate::bridge::{BridgeError, Portal};
use crate::config::Conf;
use crate::log::{short_error, Ids, Logger};
use crate::protocol::{row, Protocol};
use crate::pyfmt::{ts_file, ts_human};
use crate::state::{CaseInfoState, FailMemory, Pages, Parties, RunState};
use chrono::{DateTime, Local};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type R = tauri::Wry;

#[derive(Debug, Clone)]
pub enum RunErr {
    SessionLost(String),
    BrowserClosed,
    BrowserLost(String),

    Stopped(String),
    CaseOpenFailed { msg: String, broken: bool },
    PortalNoData(String),
    DocNoData(String),

    Gateway(String),

    DiskFull(String),

    LoginTimeout,

    PortalDown(String),
    Other(String),
}

impl std::fmt::Display for RunErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunErr::SessionLost(m) => write!(f, "{m}"),
            RunErr::BrowserClosed => write!(f, "the browser window was closed"),
            RunErr::LoginTimeout => write!(f, "no login within 30 minutes"),
            RunErr::PortalDown(m) => write!(f, "the portal is not available: {m}"),
            RunErr::BrowserLost(m) => write!(f, "{m}"),
            RunErr::Stopped(s) => write!(f, "stopped ({s})"),
            RunErr::CaseOpenFailed { msg, .. } => write!(f, "{msg}"),
            RunErr::PortalNoData(m) | RunErr::DocNoData(m) | RunErr::Gateway(m) | RunErr::DiskFull(m) | RunErr::Other(m) => {
                write!(f, "{m}")
            }
        }
    }
}

impl RunErr {
    pub fn short(&self) -> String {
        let (t, m) = match self {
            RunErr::SessionLost(m) => ("SessionLost", m.clone()),
            RunErr::DocNoData(m) => ("DocNoData", m.clone()),
            RunErr::PortalNoData(m) => ("PortalNoData", m.clone()),
            RunErr::CaseOpenFailed { msg, .. } => ("CaseOpenFailed", msg.clone()),
            RunErr::Gateway(m) | RunErr::Other(m) => ("RuntimeError", m.clone()),
            RunErr::BrowserLost(m) => ("BrowserConnectionLost", m.clone()),
            RunErr::BrowserClosed => ("BrowserClosed", "the browser window was closed".into()),
            RunErr::Stopped(s) => ("KeyboardInterrupt", s.clone()),
            RunErr::DiskFull(m) => ("DiskFull", m.clone()),
            RunErr::LoginTimeout => ("LoginTimeout", "no login within 30 minutes".into()),
            RunErr::PortalDown(m) => ("PortalDown", m.clone()),
        };
        short_error(&format!("{t}: {m}"))
    }
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            RunErr::SessionLost(_)
                | RunErr::BrowserClosed
                | RunErr::BrowserLost(_)
                | RunErr::Stopped(_)
                | RunErr::DiskFull(_)
                | RunErr::LoginTimeout
                | RunErr::PortalDown(_)
        )
    }
}

impl From<BridgeError> for RunErr {
    fn from(e: BridgeError) -> Self {
        match e {
            BridgeError::Closed => RunErr::BrowserClosed,
            BridgeError::Timeout => RunErr::Other("TimeoutError: the portal page did not answer".into()),
            BridgeError::NotReady => RunErr::Other("the portal page is still loading".into()),
            BridgeError::Js(m) => RunErr::Other(format!("page script error: {m}")),
        }
    }
}

pub type RResult<T> = Result<T, RunErr>;

#[derive(Default)]
pub struct RunControl {
    pub running: AtomicBool,
    pub stop: AtomicBool,
    pub stop_source: Mutex<String>,

    pub web_retry: AtomicBool,

    pub online_logout: AtomicBool,

    pub online_active: AtomicBool,

    pub wish_online: AtomicBool,

    pub check_now: AtomicBool,
    pub check_result: Mutex<Option<(i64, bool, String)>>,
}

impl RunControl {
    pub fn request_stop(&self, source: &str) {
        *self.stop_source.lock().unwrap() = source.into();
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub trait Sink: Send + Sync {
    fn status(&self, v: &Value);
    fn log_line(&self, line: &str);

    fn overview_changed(&self);

    fn portal_phase(&self, _phase: &str) {}

    fn notify(&self, _body: &str) {}
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub resume_key: String,
    pub seq: usize,
    pub number: String,
    pub kind: String,
    pub date: String,
    pub office: String,
    pub class_text: String,
    pub portal: usize,
    pub ok_total: usize,
    pub new: usize,
    pub failed: usize,
    pub aud: usize,
    pub status: String,
    pub error: String,
    pub missing: Vec<Value>,
    pub before_date: usize,
    pub already: usize,

    pub carpeta: Option<Value>,
    pub media_missing: usize,
    pub media_docs: Vec<Value>,
}

#[derive(Clone, Debug, Default)]
pub struct Card {
    pub kind: String,
    pub number: String,
    pub date: String,
    pub office: String,
    pub clase: String,
    pub asunto: String,
    pub clase_asunto: String,
    pub class_text: String,
    pub raw: String,
}

#[derive(Clone, Debug, Default)]
pub struct EntryData {
    pub datos: Value,
    pub partes: Value,
    pub docs_req: Value,
    pub docs: Value,
    pub aud: Value,
    pub general_text: String,
    pub partes_text: String,

    pub light: bool,
}

#[derive(Clone, Debug)]
pub struct FailedDoc {
    pub info: Card,
    pub carpeta: Value,
    pub contexto: Value,
    pub doc: Value,
    pub case_dir: PathBuf,

    pub entry: usize,
}

pub struct Fetcher {
    pub entry_check: crate::entry_check::EntryCheck,
    pub light_entries: usize,

    pub api_times: Vec<(String, f64)>,
    pub cfg: Conf,
    pub log: Logger,
    pub ids: Ids,
    pub portal: Portal<R>,
    pub ctl: Arc<RunControl>,
    pub sink: Arc<dyn Sink>,
    pub version: String,
    pub run_id: String,
    pub run_start: DateTime<Local>,
    pub proto: Protocol,
    pub failmem: FailMemory,
    pub parties: Parties,
    pub pages: Pages,
    pub ci_state: CaseInfoState,
    pub run_state: RunState,
    pub portal_host: String,

    pub carpetas: Vec<Value>,
    pub carpeta_legajo: Vec<bool>,
    pub carpeta_used: HashSet<usize>,
    pub portal_list: Vec<Value>,
    pub list_loads: Vec<(String, f64, Option<f64>)>,
    pub dup_cache: HashMap<String, (usize, EntryData)>,
    pub dup_docs: HashMap<usize, BTreeSet<String>>,
    pub dup_reused: usize,

    pub seq: usize,
    pub prog_i: usize,
    pub prog_total: usize,
    pub docs_downloaded: usize,
    pub consecutive_errors: u32,
    pub resume_skipped: usize,
    pub adopted_from_disk: usize,
    pub skipped_by_date: usize,
    pub open_api: usize,
    pub open_click: usize,
    pub entries: Vec<Summary>,
    pub failed_entries: Vec<usize>,
    pub failed: Vec<FailedDoc>,
    pub gw_failed: HashSet<String>,
    pub new_docs: usize,
    pub downloads: Vec<String>,
    pub audiencias: Vec<(Card, Vec<Value>)>,

    pub media: crate::media::Register,
    pub media_new: usize,
    pub current: Option<(Summary, Option<Value>, Vec<Value>)>,
    pub current_doc_ctx: (String, String),
    pub entry_key_counts: HashMap<String, usize>,
    pub contact_phone: String,

    pub last_ui_action: Instant,
    pub last_activity_click: Instant,
    pub token_wait_skip: Instant,
    pub assisted: bool,
    pub assist_tried: Instant,
    pub login_stage: String,
    pub live_alert: String,
    pub login_ok_at: Option<Instant>,
    pub status_early: bool,

    pub wake_gap: Arc<std::sync::atomic::AtomicU64>,
    pub salir_since: Option<Instant>,

    pub down_since: Option<Instant>,
    pub probe_gone: u32,
    pub probe_log: Instant,

    pub check_last: Instant,

    pub ka_case: Option<Value>,

    pub ka_logged: Option<Instant>,

    pub mouse_sent: Option<Instant>,
    pub mouse_logged: Option<Instant>,

    pub dl_max_secs: f64,
    pub dl_rate: f64,

    pub totp_tries: u8,

    pub blank_since: Option<Instant>,
    pub blank_nav: Option<Instant>,

    pub live_version: u64,
    pub live_progress: (String, usize, usize),
    pub live_phase: String,
    pub live_status_time: Option<Instant>,
    pub live_counts: (usize, usize, usize),
    pub live_written: Option<Instant>,
    pub browser_dl_off: bool,
    pub bdl_fail: u32,

    pub lock_mode: u8,
}

pub const BROWSER_LABEL: &str = "Portal";

impl Fetcher {
    pub fn space_ok(&self, need: u64) -> RResult<()> {
        let keep = self.cfg.min_free;
        for d in [&self.cfg.case.dir, &self.cfg.protocol_dir] {
            if let Some((free, _)) = crate::disk::space(d) {
                if free < need.saturating_add(keep) {
                    return Err(RunErr::DiskFull(format!(
                        "{:.1} GB < {:.1} GB · {}",
                        free as f64 / 1073741824.0,
                        need.saturating_add(keep) as f64 / 1073741824.0,
                        d.display()
                    )));
                }
            }
        }
        Ok(())
    }
}

impl Fetcher {
    pub fn info(&self, m: impl AsRef<str>) {
        self.log.info(m)
    }
    pub fn warn(&self, m: impl AsRef<str>) {
        self.log.warn(m)
    }
    pub fn err(&self, m: impl AsRef<str>) {
        self.log.error(m)
    }

    pub fn id_case(&self, n: &str) -> String {
        let s = self.ids.lock().unwrap().case(n, true);
        if s.is_empty() {
            "E?".into()
        } else {
            s
        }
    }
    pub fn id_entry(&self, n: &str, kind: &str, date: &str, office: &str) -> String {
        let s = self.ids.lock().unwrap().entry(n, kind, date, office);
        if s.is_empty() {
            "E?".into()
        } else {
            s
        }
    }
    pub fn id_doc(&self, id: &str) -> String {
        let s = self.ids.lock().unwrap().doc(id, true);
        if s.is_empty() {
            "D?".into()
        } else {
            s
        }
    }

    pub fn check_stop(&self) -> RResult<()> {
        if self.ctl.stop.load(Ordering::SeqCst) {
            return Err(RunErr::Stopped(self.ctl.stop_source.lock().unwrap().clone()));
        }

        self.session_room(0)
    }

    pub fn session_room(&self, need: i64) -> RResult<()> {
        if crate::portalsess::max_due(self.cfg.session_max_min, need) {
            return Err(RunErr::Stopped("session_max".into()));
        }
        Ok(())
    }

    pub fn sleep(&self, secs: f64) -> RResult<()> {
        let end = Instant::now() + Duration::from_secs_f64(secs.max(0.0));
        while Instant::now() < end {
            self.check_stop()?;
            if !self.portal.alive() {
                return Err(RunErr::BrowserClosed);
            }
            std::thread::sleep(Duration::from_millis(100.min((end - Instant::now()).as_millis() as u64 + 1)));
        }
        self.check_stop()
    }

    pub fn wait_ms(&self, ms: u64) -> RResult<()> {
        self.sleep(ms as f64 / 1000.0)
    }

    /* removed in the public source: validation / licence check · eliminado en el código público: validación / licencia */

    pub fn keep_online(&mut self) -> RResult<()> {
        let interval = self.cfg.online_watch_interval.max(15.0);

        self.lock_mode = 0;
        self.apply_lock();

        self.online_to_list()?;
        self.sink.portal_phase("online");
        loop {
            if self.ctl.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if !self.portal.alive() {
                return Err(RunErr::BrowserClosed);
            }

            self.session_room(0)?;

            match self.is_logged_in() {
                Ok(true) => {}
                Ok(false) => {
                    self.sink.portal_phase("login");
                    self.info("Online switch: portal session lost - logging in again");

                    self.session_lost("");
                    self.login_at_start()?;
                    self.online_to_list()?;
                    self.sink.portal_phase("online");
                }
                Err(RunErr::BrowserClosed) => return Err(RunErr::BrowserClosed),
                Err(_) => {}
            }

            let _ = self.ensure_session_fresh();
            let _ = self.keep_alive(true);

            match self.fresh_headers(session::HDR_MAX_AGE_ONLINE) {
                Err(RunErr::SessionLost(_)) => continue,
                Err(e) if e.is_fatal() => return Err(e),
                _ => {}
            }

            let forced = self.ctl.check_now.swap(false, Ordering::SeqCst);
            match self.online_check(forced) {
                Ok(Some(c)) if c != crate::run::keep::Check::Ok => {
                    if self.recheck_with_fresh_headers()? {
                        continue;
                    }
                    self.sink.portal_phase("login");
                    self.info("   the portal no longer accepts the session (server check) - logging in again");
                    self.session_lost("");
                    self.end_portal_login()?;
                    self.login_at_start()?;
                    self.online_to_list()?;
                    self.sink.portal_phase("online");
                }
                Err(e) if e.is_fatal() => return Err(e),
                _ => {}
            }
            let steps = (interval * 10.0) as u64;
            for _ in 0..steps {
                if self.ctl.stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                if !self.portal.alive() {
                    return Err(RunErr::BrowserClosed);
                }
                if self.ctl.check_now.load(Ordering::SeqCst) {
                    break;
                }
                if crate::portalsess::max_due(self.cfg.session_max_min, 0) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    fn online_to_list(&mut self) -> RResult<()> {
        match self.go_to_list(false, "online switch") {
            Ok(()) => {}

            Err(e @ (RunErr::Stopped(_) | RunErr::BrowserClosed | RunErr::BrowserLost(_))) => return Err(e),
            Err(e) => self.warn(format!("   online switch: 'Mis expedientes' could not be opened: {}", e.short())),
        }
        self.info(format!("   online switch: browser page after the login: {}", self.short_url()));
        Ok(())
    }

    pub fn js(&self, expr: &str) -> RResult<Value> {
        self.check_stop()?;
        Ok(self.portal.call(expr)?)
    }

    pub fn js_opt(&self, expr: &str) -> RResult<Option<Value>> {
        self.check_stop()?;
        match self.portal.call(expr) {
            Ok(v) => Ok(Some(v)),
            Err(BridgeError::Closed) => Err(RunErr::BrowserClosed),
            Err(_) => Ok(None),
        }
    }

    pub fn short_url(&self) -> String {
        self.portal.short_url()
    }

    pub fn alert_key(&self) -> String {
        let a = &self.live_alert;
        if (a == "login" || a == "session")
            && ["code_app", "code_email", "mask", "badpw", "pwchange", "later", "dirty"].contains(&self.login_stage.as_str())
        {
            return format!("login_{}", self.login_stage);
        }
        a.clone()
    }

    pub fn set_live_alert(&mut self, alert: Option<&str>) {
        self.status_early = true;
        let a = alert.unwrap_or("").to_string();
        if a != self.live_alert {
            self.live_alert = a;
            self.write_live_status(false, true, false, "");

            self.apply_lock();
        }
    }

    pub fn apply_lock(&self) {
        let login = self.live_alert == "login" || self.live_alert == "session";
        if self.lock_mode == 0 || login {
            let _ = self.portal.call("FJ.lock(false)");
            return;
        }
        let online = self.lock_mode == 2;
        let (banner, dlg) = if online {
            (
                crate::i18n::tr("FetchJ is keeping the portal online – please do not click."),
                crate::i18n::tr("For safety this window is locked while FetchJ keeps the portal online.\nDo you want to take over and work in it yourself?"),
            )
        } else {
            (
                crate::i18n::tr("FetchJ is updating – please do not click."),
                crate::i18n::tr("An update is running.\nYou can watch, but you cannot change anything right now."),
            )
        };
        let ok = if online { crate::i18n::tr("Take over") } else { "OK".to_string() };
        let cancel = if online { crate::i18n::tr("Cancel") } else { String::new() };
        let _ = self.portal.call(&format!(
            "FJ.lock(true,{},{},{},{},{})",
            crate::bridge::js(if online { "online" } else { "wait" }),
            crate::bridge::js(&banner),
            crate::bridge::js(&dlg),
            crate::bridge::js(&ok),
            crate::bridge::js(&cancel),
        ));
    }

    pub fn set_login_stage(&mut self, st: &str) {
        self.login_stage = st.into();
        if !self.live_alert.is_empty() {
            self.write_live_status(false, true, false, "");
        }
    }

    pub fn dur_text(&self) -> String {
        let s = (Local::now() - self.run_start).num_seconds().max(0);
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    }

    pub fn write_live_status(&mut self, done: bool, force: bool, aborted: bool, res: &str) {
        if !force && !done {
            if let Some(t) = self.live_status_time {
                if t.elapsed() < Duration::from_millis(1500) {
                    return;
                }
            }
        }
        self.live_status_time = Some(Instant::now());
        let (case, cur, total) = self.live_progress.clone();
        let v = json!({
            "v": self.live_version, "done": done, "aborted": aborted, "case": case, "cur": cur, "total": total,
            "t": Local::now().format("%H:%M").to_string(), "alert": self.alert_key(), "phase": self.live_phase,
            "retry": if self.live_alert.is_empty() { "" } else { "app" }, "dur": self.dur_text(), "res": res,
            "b": BROWSER_LABEL, "ei": self.prog_i, "et": self.prog_total,
            "lok": if self.login_ok_at.map(|t| t.elapsed() < Duration::from_secs(30)).unwrap_or(false) { 1 } else { 0 },
            "run": self.run_id, "stage": self.login_stage,

            "sdur": if self.live_alert.is_empty() { String::new() } else { crate::portalsess::end_text() },
        });
        self.sink.status(&v);
    }

    pub fn live_report(&mut self, force: bool) {
        if !self.cfg.html_report_live {
            return;
        }
        let counts = (self.entries.len(), self.new_docs, self.failed_entries.len());
        if !force && counts == self.live_counts {
            return;
        }
        if !force {
            if let Some(t) = self.live_written {
                if t.elapsed() < Duration::from_secs(self.cfg.html_report_live_interval.max(5)) {
                    return;
                }
            }
        }
        self.live_counts = counts;
        self.live_written = Some(Instant::now());
        self.save_run_info(true, "RUNNING", "", &[], None);
        self.live_version += 1;
        if let Ok(mut ids) = self.ids.lock() {
            ids.save();
        }
        self.pages.save();
        self.sink.overview_changed();

        self.log.info(format!(
            "   case overview: new data for the app - list and totals (documents downloaded in this update so far: {})",
            self.new_docs
        ));
        self.write_live_status(false, true, false, "");
    }

    pub fn live_tick(&mut self) {
        let iv = self.cfg.html_report_live_interval.max(30);
        if self.live_written.map(|t| t.elapsed() >= Duration::from_secs(iv)).unwrap_or(true) {
            self.live_counts = (usize::MAX, 0, 0);
            self.live_report(false);
        }
    }

    pub fn save_run_info(&self, live: bool, status: &str, why: &str, files: &[PathBuf], tech: Option<&Path>) {
        use crate::pyfmt::py_or_empty;
        let plist: Vec<Value> = self
            .portal_list
            .iter()
            .map(|x| {
                let a = py_or_empty(x.get("numeroExpedienteConFormato"));
                json!({
                    "case": if a.is_empty() { py_or_empty(x.get("numero")) } else { a },
                    "office": py_or_empty(x.get("contexto").and_then(|c| c.get("descripcion"))),
                    "kind": py_or_empty(x.get("estado").and_then(|c| c.get("expedienteLegajo"))),
                    "date": py_or_empty(x.get("fechaEntrada")).chars().take(10).collect::<String>(),
                    "clase": py_or_empty(x.get("clase").and_then(|c| c.get("descripcion"))),
                    "estado": py_or_empty(x.get("estado").and_then(|c| c.get("descripcion"))),
                })
            })
            .collect();
        let mut now_keys: std::collections::BTreeMap<String, Vec<(String, String, String)>> = Default::default();
        for c in &self.carpetas {
            let (n, k, d, o) = list::carpeta_ident(c);
            let v = now_keys.entry(n).or_default();
            let key = (k, d, crate::text::norm_office(&o));
            if !v.contains(&key) {
                v.push(key);
            }
        }
        let ent = |s: &Summary| {
            json!({"number": s.number, "kind": s.kind, "office": s.office, "date": s.date, "class_text": s.class_text,
            "before_date": s.before_date, "status": s.status, "missing": s.missing})
        };
        let mut entries: Vec<Value> = self.entries.iter().map(ent).collect();
        if let Some((s, _, _)) = &self.current {
            if !self.entries.iter().any(|e| e.seq == s.seq) {
                entries.push(ent(s));
            }
        }
        let ri = crate::overview::RunInfo {
            run_id: self.run_id.clone(),
            start_ms: self.run_start.timestamp_millis(),
            end_ms: Local::now().timestamp_millis(),
            start: self.run_start.format("%d/%m/%Y %H:%M").to_string(),
            end: Local::now().format("%H:%M:%S").to_string(),
            status: status.into(),
            why: why.into(),
            live,
            entries_done: self.entries.len() + self.resume_skipped,
            plist,
            now_keys,
            entries,
            failed_entries: self.failed_entries.iter().map(|i| ent(&self.entries[*i])).collect(),
            auds: self.audiencias.iter().map(|(c, a)| (c.number.clone(), a.clone())).collect(),
            error_files: files.iter().map(|p| p.to_string_lossy().to_string()).collect(),
            tech_report: tech.map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            dup_lines: {
                let keys: Vec<String> = self.carpetas.iter().map(list::dup_key).collect();
                let set: HashSet<&String> = keys.iter().collect();
                keys.len() - set.len()
            },
        };
        crate::overview::save_run_info(&self.cfg, &ri);
    }

    pub fn resume_key(&mut self, c: &Card) -> String {
        let base = format!("{}|{}|{}|{}|{}", c.kind, c.number, c.date, c.office, c.class_text);
        let n = self.entry_key_counts.entry(base.clone()).or_insert(0);
        *n += 1;
        format!("{base}#{n}")
    }

    pub fn write_entry_row(&mut self, s: &Summary) {
        if s.status != "INTERRUPTED" && !s.resume_key.is_empty() && !self.cfg.dry_run {
            self.run_state.mark(&s.resume_key);
        }
        self.write_entry_row_csv(s);
    }

    pub fn write_entry_row_csv(&self, s: &Summary) {
        self.proto.append_entry(&row(&[
            ("run_id", self.run_id.clone()),
            ("run_start", ts_human(&self.run_start)),
            ("seq", s.seq.to_string()),
            ("case_number", s.number.clone()),
            ("entry_kind", s.kind.clone()),
            ("entry_date", s.date.clone()),
            ("entry_office", s.office.clone()),
            ("entry_class", s.class_text.clone()),
            ("docs_on_portal", s.portal.to_string()),
            ("docs_ok_total", s.ok_total.to_string()),
            ("docs_new_this_run", s.new.to_string()),
            ("docs_failed", s.failed.to_string()),
            ("audiencias", s.aud.to_string()),
            ("status", s.status.clone()),
            ("error", s.error.clone()),
        ]));
    }

    pub fn note_success(&mut self) {
        self.consecutive_errors = 0;
    }

    pub fn note_error(&mut self, reason: &str) -> RResult<()> {
        self.consecutive_errors += 1;
        if self.consecutive_errors >= self.cfg.max_consecutive_errors {
            let n = self.consecutive_errors;
            self.handle_session_problem(&format!("{n} errors in a row (last: {reason})"), true)?;
        }
        Ok(())
    }

    pub fn run(&mut self) -> RResult<()> {
        self.info(format!("Browser page at start: {}", self.short_url()));

        if let Err(e) = self.ensure_list(true, "start of the update") {
            match e {
                RunErr::SessionLost(m) => {
                    self.handle_session_problem(&m, true)?;
                    self.ensure_list(true, "after a new login")?;
                }
                e => return Err(e),
            }
        }
        self.fetch_contact_phone();
        self.live_report(true);
        if self.cfg.list_only {
            let n = if self.carpetas.is_empty() {
                self.portal_list.len()
            } else {
                self.carpetas.len()
            };
            self.info(format!(
                "Case list only (list_only = 1): {n} entries in the portal list - no entry opened, nothing downloaded."
            ));
            self.sink_settings_remove("download", "list_only");
            self.info("   'case list only' switched off again in the settings - the next run is a normal one.");
            self.run_state.complete();
            return Ok(());
        }
        let pages = self.total_pages()?;
        self.info(format!("List 'Mis expedientes': {pages} page(s)."));
        self.prog_i = 0;
        let total = self
            .carpetas
            .iter()
            .filter(|c| {
                let (n, k, d, o) = list::carpeta_ident(c);
                self.cfg.entry_mode(&n, &k, &d, &o) != "skip"
            })
            .count();
        self.prog_total = total;
        if total > 0 {
            self.info(format!(
                "Entries to check in this run: {total} (cases set to 'do not load' not counted)."
            ));
        }
        self.write_live_status(false, true, false, "");
        let mut page_no = 1;
        while page_no <= pages {
            self.ensure_list(false, "")?;
            self.go_to_page(page_no)?;
            let count = self.case_link_count()?;
            for pos in 0..count {
                if self.cfg.max_documents_per_run > 0 && self.docs_downloaded as i64 >= self.cfg.max_documents_per_run {
                    self.warn("max_documents_per_run reached - stopping.");
                    return Ok(());
                }
                match self.process_entry(page_no, pos) {
                    Ok(()) => self.live_report(false),
                    Err(e) if e.is_fatal() => return Err(e),
                    Err(e) => {
                        if !self.portal.alive() {
                            return Err(RunErr::BrowserClosed);
                        }
                        let s = e.short();
                        self.err(format!(
                            "   Entry on list page {page_no}, position {} skipped: {s} (page: {})",
                            pos + 1,
                            self.short_url()
                        ));
                        self.note_error(&s.chars().take(200).collect::<String>())?;
                        match self.ensure_list(false, "after an unexpected error") {
                            Err(RunErr::SessionLost(m)) => return Err(RunErr::SessionLost(m)),
                            Err(e) if e.is_fatal() => return Err(e),
                            _ => {}
                        }
                    }
                }
            }
            page_no += 1;
        }
        self.final_retry()?;

        self.media_pass()?;
        self.save_dups();
        if self.cfg.entry_fetch_mode == "api" {
            self.info(format!(
                "Entries read by direct query: {}, opened in the web page: {}",
                self.open_api, self.open_click
            ));
            if self.dup_reused > 0 {
                self.info(format!(
                    "   of them taken over from the same line without a new query (the portal lists them twice): {}",
                    self.dup_reused
                ));
            }
            if self.light_entries > 0 {
                self.info(format!(
                    "Entries without asking parties and hearings again (nothing changed): {}",
                    self.light_entries
                ));
            }
        }
        if !self.list_loads.is_empty() {
            let sum: f64 = self.list_loads.iter().map(|x| x.1).sum();
            let parts: Vec<String> = self
                .list_loads
                .iter()
                .map(|x| format!("{}: {:.0} s", crate::i18n::tr(&x.0), x.1))
                .collect();
            self.info(format!(
                "List 'Mis expedientes' loaded {} time(s) in this update, together {:.0} s ({})",
                self.list_loads.len(),
                sum,
                parts.join("; ")
            ));
        }
        self.run_state.complete();
        if self.cfg.overrule_protocol && !self.cfg.dry_run {
            self.sink_settings_remove("protocol", "overrule_protocol");
            self.info("   'Download existing files again from date' switched off again - the next run is a normal one.");
        }
        Ok(())
    }

    fn sink_settings_remove(&self, section: &str, key: &str) {
        let v = json!({"settingOff": [section, key]});
        self.sink.status(&v);
    }

    pub fn fetch_contact_phone(&mut self) {
        self.contact_phone = self.cfg.contact_phone_fallback.clone();
        if !self.cfg.contact_phone.eq_ignore_ascii_case("auto") {
            self.contact_phone = self.cfg.contact_phone.clone();
            return;
        }
        let url = format!("{}/{}", self.cfg.api_base_url, self.cfg.api["api_configuration"]);
        let extra = json!({"p_codigoconfiguracion": self.cfg.api["config_code_contact_phone"]});
        match self.api_call("GET", &url, &extra, "", self.cfg.ui_element_timeout, false) {
            Ok(r) if r.status == 200 => {
                let j: Value = serde_json::from_str(&r.text).unwrap_or(Value::Null);
                let v = if crate::pyfmt::truthy(j.get("huboError")) {
                    None
                } else {
                    j.get("valorDevuelto")
                        .and_then(|a| a.get(0))
                        .and_then(|x| x.get("valor"))
                        .filter(|v| crate::pyfmt::truthy(Some(v)))
                        .cloned()
                };
                if let Some(v) = v {
                    self.contact_phone = crate::pyfmt::py_str(Some(&v)).trim().to_string();
                    self.info(format!(
                        "Contact phone of the portal (for the error report): {}",
                        self.contact_phone
                    ));
                    return;
                }
                self.warn("   contact phone not received from the portal - using contact_phone_fallback");
            }
            Ok(_) => self.warn("   contact phone not received from the portal - using contact_phone_fallback"),
            Err(e) => self.warn(format!(
                "   contact phone not received from the portal ({}) - using contact_phone_fallback",
                e.short()
            )),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ApiResp {
    pub status: u16,
    pub text: String,
    pub headers: HashMap<String, String>,
    pub dur: f64,
    pub err: String,
}

pub struct StartArgs {
    pub conf: Conf,
    pub version: String,
    pub portal: Portal<R>,
    pub ctl: Arc<RunControl>,
    pub sink: Arc<dyn Sink>,

    pub from_app: bool,

    pub lock: Option<Option<crate::lock::InstanceLock>>,

    pub online_only: bool,
}

#[derive(Default)]
pub struct RunOutcome {
    pub status: String,
    pub why: String,

    pub msg: String,
}

pub fn run_update(args: StartArgs) -> RunOutcome {
    let StartArgs {
        conf,
        version,
        portal,
        ctl,
        sink,
        lock,
        online_only,
        ..
    } = args;
    let start = Local::now();
    let run_id = ts_file(&start);
    crate::i18n::set_language(&conf.language);
    let ids: Ids = crate::ids::shared(&conf.pfile("FetchJ_ids.json"));
    let sink2 = sink.clone();
    crate::portalsess::init(&conf.log_dir);
    let log = Logger::new(
        &conf.log_dir,
        &run_id,
        Some(ids.clone()),
        conf.console_log_level,
        Some(Arc::new(move |l: &str| sink2.log_line(l))),
    );
    log.info(format!("FetchJ {version} started. Run ID {run_id}"));

    if crate::portalsess::stale(conf.session_max_min) {
        let e = crate::portalsess::End {
            cause: "max_time".into(),
            page: "-".into(),
            internet: "-".into(),
            ..Default::default()
        };
        log.info(crate::i18n::tr(
            "The portal login remembered from an earlier start is older than the maximum time of the portal session - it counts as ended.",
        ));
        for l in crate::portalsess::ended(&version, &e) {
            log.info(l);
        }
    }

    let _focus_log = crate::focus::RunLogGuard::new(&log, conf.console_log_level);
    log.info(format!("Folder for the case files: {}", conf.case.dir.display()));
    let early = |why: &str, sink: &Arc<dyn Sink>| {
        let v = json!({"v": 999999, "done": true, "aborted": true, "case": "", "cur": 0, "total": 0,
            "t": Local::now().format("%H:%M").to_string(), "alert": "", "phase": "", "dur": "0:00:00", "res": "aborted",
            "why": why, "run": run_id});
        sink.status(&v);
    };

    if let Some((code, path)) = crate::paths::check_case_dir(&conf.case) {
        log.error(format!(
            "The case files cannot be saved: {} ({})",
            why_text_en(code),
            path.display()
        ));
        early(code, &sink);
        return RunOutcome {
            status: "ABORTED".into(),
            why: code.into(),
            ..Default::default()
        };
    }
    let _lock = match lock.map(Ok).unwrap_or_else(|| crate::lock::acquire(&conf.data_dir)) {
        Ok(l) => l,
        Err(info) => {
            log.error(format!("Another update is already running on this folder for the case files (started {info}) - this update does not start. Please wait until it has ended."));
            let why = format!("busy|{info}");
            early(&why, &sink);
            return RunOutcome {
                status: "ABORTED".into(),
                why,
                ..Default::default()
            };
        }
    };
    for m in conf.move_app_data() {
        if m.contains("could not") {
            log.warn(m)
        } else {
            log.info(m)
        }
    }
    log.info(format!("Data folder (protocol, case overview): {}", conf.data_dir.display()));

    log.info(format!("Settings, licence and logs: {}", conf.app_dir.display()));
    log.info(format!(
        "Browser profile of this Windows user: {}",
        conf.portal_profile_dir.display()
    ));
    if conf.list_only {
        log.info("Run type: case list only (list_only = 1).");
    }
    log.info(format!("File name pattern: {}.<real extension>", conf.filename_pattern));
    log.info(format!(
        "Mode: {}{}",
        if conf.overrule_protocol {
            format!("OVERRULE protocol from {}", conf.overrule_start_date)
        } else {
            "use protocol (new and failed documents only)".into()
        },
        if conf.dry_run { " - DRY RUN" } else { "" }
    ));
    let awake = conf.prevent_sleep && keep_awake(true);
    if awake {
        log.info("While the update runs the computer does not go to sleep (screen lock and screen saver work as usual).");
    }
    let mut proto = Protocol::load(&conf.protocol_dir, &conf.case.dir, &version, conf.redownload_missing_files);
    for n in proto.notes.drain(..) {
        log.info(n);
    }
    {
        let mut m = ids.lock().unwrap();
        let mut rows: Vec<_> = proto
            .ok
            .values()
            .map(|r| {
                (
                    r.get("case_number").cloned().unwrap_or_default(),
                    r.get("doc_id").cloned().unwrap_or_default(),
                )
            })
            .collect();
        rows.sort();
        for (c, d) in rows {
            m.case(&c, true);
            m.doc(&d, true);
        }
        m.save();
    }
    let failmem = FailMemory::load(
        &conf.protocol_dir,
        conf.skip_after_failed_runs,
        conf.retry_skipped_after_days,
        conf.retry_paused,
    );
    let np = failmem.docs.keys().filter(|k| failmem.paused_until("docs", k).is_some()).count();
    let ne = failmem
        .entries
        .keys()
        .filter(|k| failmem.paused_until("entries", k).is_some())
        .count();
    if np + ne > 0 {
        if conf.retry_paused {
            log.info(format!("Paused after repeated failures: {np} document(s), {ne} case entr(y/ies) - tried again in this run anyway (retry_paused = 1)."));
        } else {
            log.info(format!(
                "Paused after repeated failures: {np} document(s), {ne} case entr(y/ies) - not tried in this run (skip_after_failed_runs = {}).",
                conf.skip_after_failed_runs
            ));
        }
    }
    log.info(format!(
        "Protocol: {} document(s) have already been downloaded successfully.",
        proto.ok.len()
    ));
    let t = Instant::now();
    let (nf, nc, nn) = proto.scan_disk(&conf.case.dir);
    log.info(format!(
        "Case folder: {nf} file(s) in {nc} case folder(s) found ({:.1} s) - {nn} of them not yet in the protocol; documents found there are taken over and not downloaded again.",
        t.elapsed().as_secs_f64()
    ));

    let present = proto.present_ids();
    let mut came = crate::read_state::sync_presence(&conf.read_state_file, &present, true);

    let back_rows: Vec<(crate::protocol::Row, std::path::PathBuf)> = proto
        .last
        .values()
        .filter(|r| r.get("status").map(|s| s == "FAILED").unwrap_or(false))
        .filter_map(|r| {
            let id = r.get("doc_id")?;
            let case = r.get("case_number")?;
            proto.found_on_disk(case, id).map(|p| (r.clone(), p))
        })
        .collect();
    let mut back: Vec<String> = Vec::new();
    for (r, p) in back_rows {
        back.push(r.get("doc_id").cloned().unwrap_or_default());

        let mut r = r;
        r.insert("run_id".into(), run_id.clone());
        r.insert("run_start".into(), ts_human(&start));
        proto.adopt_existing(r, &p);
    }
    if !back.is_empty() {
        let _ = crate::read_state::mark(&conf.read_state_file, "doc", &back, false);
        let mut all = came.take().unwrap_or_default();
        for id in back {
            if !all.iter().any(|x| x.eq_ignore_ascii_case(&id)) {
                all.push(id);
            }
        }
        came = Some(all);
    }
    match came {
        None => log
            .info("   documents in the case folders recorded - from the next update on, every document that comes (back) is marked unread"),
        Some(new) if !new.is_empty() => {
            log.info(format!(
                "   {} document(s) came (back) into the case folders since the last update - marked as unread",
                new.len()
            ));
            for id in &new {
                log.info(format!("      back in the case folder - marked as unread: document {id}"));
            }
        }
        Some(_) => {}
    }
    let pages = Pages::load(&conf.protocol_dir, &conf.case.dir);
    let parties = Parties::load(&conf.protocol_dir);
    let ci_state = CaseInfoState::load(&conf.protocol_dir);
    let run_state = RunState::start(&conf.run_state_file, &run_id, &ts_human(&start), conf.resume_after_abort);
    if let Some(from) = &run_state.resumed_from {
        log.info(format!(
            "Resuming the interrupted run {from}: {} entries have already been processed and will be skipped (resume_after_abort = 1).",
            run_state.done.len()
        ));
    }

    let todo: Vec<PathBuf> = proto
        .ok
        .values()
        .filter_map(|r| r.get("target_path").filter(|p| !p.is_empty()).map(PathBuf::from))
        .filter(|p| !pages.known(p))
        .collect();
    if !todo.is_empty() {
        log.info(format!(
            "Page count: {} document(s) still to count - counting in the background during the run.",
            todo.len()
        ));
        let (pdir, cdir, ctl2) = (conf.protocol_dir.clone(), conf.case.dir.clone(), ctl.clone());
        std::thread::spawn(move || {
            let mut bg = Pages::load(&pdir, &cdir);
            for (i, p) in todo.iter().enumerate() {
                if ctl2.stop.load(std::sync::atomic::Ordering::SeqCst) || !ctl2.running.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                if p.exists() && !bg.known(p) {
                    bg.count(p);
                }
                if i % 100 == 99 {
                    bg.save();
                }
            }
            bg.save();
        });
    }
    let host = conf
        .portal_url
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    let mut f = Fetcher {
        entry_check: crate::entry_check::EntryCheck::load(&conf.protocol_dir),
        light_entries: 0,
        api_times: Vec::new(),
        log: log.clone(),
        ids: ids.clone(),
        portal,
        ctl: ctl.clone(),
        sink: sink.clone(),
        version: version.clone(),
        run_id: run_id.clone(),
        run_start: start,
        proto,
        failmem,
        parties,
        pages,
        ci_state,
        run_state,
        portal_host: host,
        carpetas: vec![],
        carpeta_legajo: vec![],
        carpeta_used: HashSet::new(),
        portal_list: vec![],
        list_loads: vec![],
        dup_cache: HashMap::new(),
        dup_docs: HashMap::new(),
        dup_reused: 0,
        seq: 0,
        prog_i: 0,
        prog_total: 0,
        docs_downloaded: 0,
        consecutive_errors: 0,
        resume_skipped: 0,
        adopted_from_disk: 0,
        skipped_by_date: 0,
        open_api: 0,
        open_click: 0,
        entries: vec![],
        failed_entries: vec![],
        failed: vec![],
        gw_failed: HashSet::new(),
        new_docs: 0,
        downloads: vec![],
        audiencias: vec![],
        media: crate::media::Register::load(&conf.protocol_dir),
        media_new: 0,
        current: None,
        current_doc_ctx: (String::new(), String::new()),
        entry_key_counts: HashMap::new(),
        contact_phone: conf.contact_phone_fallback.clone(),
        last_ui_action: Instant::now(),
        last_activity_click: Instant::now(),
        token_wait_skip: Instant::now(),
        assisted: false,
        assist_tried: Instant::now() - Duration::from_secs(60),
        login_stage: String::new(),
        live_alert: String::new(),
        login_ok_at: None,
        status_early: false,
        wake_gap: session::start_wake_watch(ctl.clone()),
        salir_since: None,
        down_since: None,
        probe_gone: 0,
        probe_log: Instant::now(),
        check_last: Instant::now(),
        ka_logged: None,
        mouse_sent: None,
        mouse_logged: None,
        dl_max_secs: 0.0,
        dl_rate: 0.0,
        totp_tries: 0,
        ka_case: None,
        blank_since: None,
        blank_nav: None,
        live_version: 0,
        live_progress: (String::new(), 0, 0),
        live_phase: "start".into(),
        live_status_time: None,
        live_counts: (0, 0, 0),
        live_written: None,
        browser_dl_off: false,
        bdl_fail: 0,
        lock_mode: 0,
        cfg: conf,
    };
    f.write_live_status(false, true, false, "");
    f.live_phase.clear();

    let mut status = "COMPLETED".to_string();
    let mut why = String::new();
    let mut down_msg = String::new();

    f.sink.portal_phase("login");
    let result = f.login_at_start().and_then(|_| {
        f.info("Logged in - starting.");
        f.portal_minimize_after_login();
        f.sink.portal_phase("online");
        if online_only {
            f.keep_online()
        } else {
            f.lock_mode = 1;
            f.apply_lock();
            f.run()
        }
    });

    f.lock_mode = 0;
    f.apply_lock();
    let run_ok = result.is_ok();
    match result {
        Ok(()) => {
            if online_only {
                if f.ctl.online_logout.load(Ordering::SeqCst) {
                    f.logout_portal_as(true);
                }
            } else {
                if !f.ctl.wish_online.load(Ordering::SeqCst) {
                    f.logout_portal();
                }
                let missing = !f.failed.is_empty() || !f.failed_entries.is_empty() || f.entries.iter().any(|e| !e.missing.is_empty());
                if missing {
                    status = "COMPLETED WITH MISSING DOCUMENTS".into();
                }
            }
        }
        Err(e) => {
            status = "ABORTED".into();
            match &e {
                RunErr::SessionLost(m) => {
                    f.err(format!("No connection / session lost: {m}"));
                    why = "session".into();
                }
                RunErr::BrowserClosed => {
                    f.err(format!(
                        "The {BROWSER_LABEL} window of the program was closed - the run was stopped. Start it again on the program page - it continues where it stopped."
                    ));
                    why = "browser_closed".into();
                }
                RunErr::BrowserLost(m) => {
                    f.err(format!(
                        "The control connection to the browser has stopped ({m}). Please start FetchJ again - it continues where it stopped."
                    ));
                    why = "browser".into();
                }
                RunErr::PortalDown(m) => {
                    f.err(format!("The portal of the Poder Judicial is not available right now - it says: \"{m}\". The run was stopped; it continues where it stopped at the next start."));
                    f.portal.close();
                    why = "portal_down".into();
                    down_msg = m.clone();
                }
                RunErr::LoginTimeout => {
                    f.err("Nobody logged in to the portal within 30 minutes - the run was stopped (the portal window is closed). Start it again when you are at the computer - it continues where it stopped.");
                    f.portal.close();
                    why = "login_timeout".into();
                }
                RunErr::DiskFull(m) => {
                    f.err(format!("Not enough free space - the update was stopped: {m}. Free some space (or lower \"Keep free on the drive\" in the advanced settings) and start it again - it continues where it stopped."));
                    why = "disk_full".into();
                }
                RunErr::Stopped(s) if s == "session_max" => {
                    crate::portalsess::max_handled();
                    let m = f.cfg.session_max_min;
                    if online_only {
                        f.warn(crate::i18n::trf("The portal session reached its maximum time ({} min) - FetchJ logs out (Salir). Switch the portal online again when you need it.", &[&m]));
                    } else {
                        f.warn(crate::i18n::trf(
                            "The portal session reached its maximum time ({} min) - the update was not finished. FetchJ logs out (Salir). Please start the update again - it continues where it stopped.",
                            &[&m],
                        ));
                    }
                    f.session_lost("max_time");
                    f.logout_portal_as(true);
                    if f.cfg.close_browser_at_end {
                        f.portal.close();
                    }
                    why = "session_max".into();
                }
                RunErr::Stopped(_) => {
                    if !online_only {
                        f.warn("Stopped with the button '■ Stop' on the program page - saving the results, please wait ...");
                    }
                    why = "web".into();
                }
                other => {
                    f.err(format!("Unexpected error: {}", other.short()));
                    f.err(format!("Browser page: {}", f.short_url()));
                    why = "error".into();
                }
            }
        }
    }

    if !online_only && f.ctl.online_logout.load(Ordering::SeqCst) {
        f.logout_portal_as(true);
    }

    let stay_online = run_ok && !f.ctl.online_logout.load(Ordering::SeqCst) && (online_only || f.ctl.wish_online.load(Ordering::SeqCst));
    if !online_only {
        let still_in = !stay_online && f.portal.alive() && f.is_logged_in().unwrap_or(false);
        if still_in {
            crate::focus::app_event("update ended without logging out - the portal window is still logged in");
        }
        f.sink.portal_phase(if stay_online || still_in { "online" } else { "offline" });
    }

    if online_only {
        if awake {
            keep_awake(false);
        }
        let logged_out = f.ctl.online_logout.load(Ordering::SeqCst);

        if why == "session" {
            f.session_lost("");
        }
        let takeover = f.ctl.stop_source.lock().map(|s| s.as_str() == "sync_takeover").unwrap_or(false);

        let reason = if logged_out {
            "switched off by the user - logged out"
        } else if takeover && why == "web" {
            "paused - an update takes the open session over (no new login)"
        } else {
            match why.as_str() {
                "browser_closed" => "the portal window was closed",
                "login_timeout" => "nobody logged in within 30 minutes",
                "portal_down" => "the portal is not available",
                "session" => "the portal ended the session",
                "browser" => "the connection to the portal window was lost",
                "session_max" => "maximum time of the portal session reached - logged out",
                "" => "ended",
                _ => "error - see the lines above",
            }
        };

        let hard_off = matches!(why.as_str(), "browser_closed" | "login_timeout" | "portal_down" | "session_max");
        if !logged_out && !(takeover && why == "web") && hard_off {
            f.ctl.wish_online.store(false, Ordering::SeqCst);
        }
        f.info(format!("Portal online session ended: {reason}."));
        crate::focus::app_event(&format!(
            "online session ended: {reason} - switch wish now: {}",
            if f.ctl.wish_online.load(Ordering::SeqCst) { "on" } else { "off" }
        ));
        f.sink.portal_phase(if takeover && why == "web" && !logged_out {
            "online"
        } else {
            "offline"
        });
        return RunOutcome {
            status: if logged_out { "OFFLINE".into() } else { "ONLINE_PAUSED".into() },

            why: if matches!(why.as_str(), "portal_down" | "browser_closed" | "login_timeout" | "session_max") {
                why.clone()
            } else {
                String::new()
            },
            msg: down_msg.clone(),
        };
    }

    if let Some((mut s, carpeta, docs)) = f.current.take() {
        f.finish_summary(&mut s, carpeta.as_ref(), &docs);
        if !s.missing.is_empty() {
            s.status = "INTERRUPTED".into();
        }
        match f.entries.iter().position(|e| e.seq == s.seq) {
            Some(i) => f.entries[i] = s.clone(),
            None => f.entries.push(s.clone()),
        }
        f.write_entry_row_csv(&s);
    }
    f.pages.save();
    if let Ok(mut m) = f.ids.lock() {
        m.save();
    }
    let end = Local::now();
    let (efiles, tech) = crate::report::write_end_reports(&mut f, &status, &why, &end);
    f.save_run_info(false, &status, &why, &efiles, tech.as_deref());
    if awake {
        keep_awake(false);
    }
    f.info(format!("FetchJ finished: {}", crate::i18n::tr(&status)));
    let ab = status == "ABORTED";
    let res_word = match status.as_str() {
        "COMPLETED" => "complete",
        "COMPLETED WITH MISSING DOCUMENTS" => "missing",
        _ => "aborted",
    };
    f.live_version = 999999;
    f.live_alert.clear();
    f.live_progress = (String::new(), 0, 0);
    let mut v = json!({"v": 999999, "done": true, "aborted": ab, "case": "", "cur": 0, "total": 0,
        "t": Local::now().format("%H:%M").to_string(), "alert": "", "phase": "", "dur": f.dur_text(), "res": res_word,
        "why": why, "run": f.run_id});
    v["b"] = json!(BROWSER_LABEL);

    v["listOnly"] = json!(f.cfg.list_only);
    f.sink.status(&v);
    f.sink.overview_changed();
    if f.cfg.close_browser_at_end && status.starts_with("COMPLETED") {
        f.portal.close();
    }
    RunOutcome {
        status,
        why,
        msg: down_msg,
    }
}

pub fn why_text_en(code: &str) -> String {
    let t: Value = serde_json::from_str(include_str!("../../../ui/i18n/py.en.json")).unwrap_or(Value::Null);
    t["why"][code].as_str().map(String::from).unwrap_or_else(|| code.to_string())
}

#[cfg(windows)]
pub fn keep_awake(on: bool) -> bool {
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};
    unsafe { SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS }) != 0 }
}

#[cfg(not(windows))]
pub fn keep_awake(_on: bool) -> bool {
    false
}

pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

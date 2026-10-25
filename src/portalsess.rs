use chrono::{Local, TimeZone};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const CSV: &str = "FetchJ_sessions.csv";
const STATE: &str = "FetchJ_session.json";
const HEAD_OLD: &str = "version;login;login_source;last_renewal;token_valid_until;noticed;duration;duration_min;cause;slept_min;idle_min;portal_page;portal_notice;internet";

const HEAD: &str = "version;login;login_source;last_renewal;token_valid_until;noticed;duration;duration_min;cause;slept_min;idle_min;portal_page;portal_notice;internet;keep_alive;last_server_ok;last_keep_alive";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sess {
    pub login: i64,
    pub source: String,
    pub last_renew: i64,
    pub token_exp: i64,
    pub slept: i64,
    pub logged_hour: i64,
}

#[derive(Clone, Debug, Default)]
pub struct End {
    pub cause: String,
    pub idle_min: Option<i64>,
    pub page: String,
    pub notice: String,
    pub internet: String,
}

static DIR: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));
static CUR: Lazy<Mutex<Sess>> = Lazy::new(|| Mutex::new(Sess::default()));

static END_TEXT: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

#[derive(Default)]
struct Keep {
    setting: String,
    last: std::collections::VecDeque<String>,
    server_ok: i64,
}
static KEEP: Lazy<Mutex<Keep>> = Lazy::new(|| Mutex::new(Keep::default()));

pub fn keep_setting(technique: u8, timer_min: u64) {
    KEEP.lock().unwrap().setting = format!("{technique} / {timer_min} min");
}

pub fn keep_event(text: &str) {
    let mut k = KEEP.lock().unwrap();
    k.last.push_back(format!("{} {}", hms(now()), clean(text)));
    while k.last.len() > 10 {
        k.last.pop_front();
    }
}

pub fn server_ok() {
    KEEP.lock().unwrap().server_ok = now();
}

pub fn minutes_logged_in() -> Option<i64> {
    let c = CUR.lock().unwrap().clone();
    (c.login > 0).then(|| (now() - c.login) / 60)
}

pub fn secs_left(max_min: u64) -> Option<i64> {
    let c = CUR.lock().unwrap().clone();
    (c.login > 0).then(|| c.login + max_min as i64 * 60 - now())
}

static MAX_FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn max_due(max_min: u64, margin: i64) -> bool {
    match secs_left(max_min) {
        Some(left) if left <= margin => !MAX_FIRED.load(std::sync::atomic::Ordering::SeqCst),
        _ => false,
    }
}

pub fn stale(max_min: u64) -> bool {
    secs_left(max_min).map(|l| l <= 0).unwrap_or(false)
}

pub fn max_handled() {
    MAX_FIRED.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn max_reset() {
    MAX_FIRED.store(false, std::sync::atomic::Ordering::SeqCst);
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn hms(t: i64) -> String {
    if t <= 0 {
        return "-".into();
    }
    Local
        .timestamp_opt(t, 0)
        .single()
        .map(|d| d.format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

fn stamp(t: i64) -> String {
    if t <= 0 {
        return String::new();
    }
    Local
        .timestamp_opt(t, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

pub fn dur(secs: i64) -> String {
    let m = (secs.max(0) + 30) / 60;
    if m >= 60 {
        format!("{} h {} min", m / 60, m % 60)
    } else {
        format!("{m} min")
    }
}

pub fn init(dir: &Path) {
    *DIR.lock().unwrap() = Some(dir.to_path_buf());
    let mut c = CUR.lock().unwrap();
    if c.login == 0 {
        if let Ok(s) = std::fs::read_to_string(dir.join(STATE)) {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                c.login = v["login"].as_i64().unwrap_or(0);
                c.source = v["source"].as_str().unwrap_or("").into();
                c.last_renew = v["last_renew"].as_i64().unwrap_or(0);
                c.token_exp = v["token_exp"].as_i64().unwrap_or(0);
                c.slept = v["slept"].as_i64().unwrap_or(0);
            }
        }
    }
}

fn save(c: &Sess) {
    let Some(d) = DIR.lock().unwrap().clone() else { return };
    let v = json!({"login": c.login, "source": c.source, "last_renew": c.last_renew, "token_exp": c.token_exp, "slept": c.slept});
    let _ = std::fs::write(d.join(STATE), v.to_string());
}

pub fn current() -> Sess {
    CUR.lock().unwrap().clone()
}

pub fn end_text() -> String {
    END_TEXT.lock().unwrap().clone()
}

pub fn login(at: i64, source: &str) {
    let mut c = CUR.lock().unwrap();
    *c = Sess {
        login: at,
        source: source.into(),
        logged_hour: now(),
        ..Default::default()
    };
    save(&c);
    END_TEXT.lock().unwrap().clear();
    max_reset();
}

pub fn known_or(at: i64, source: &str) {
    let mut c = CUR.lock().unwrap();
    if c.login > 0 && (at <= 0 || (c.login - at).abs() < 120) {
        if c.logged_hour == 0 {
            c.logged_hour = now();
        }
        return;
    }
    *c = Sess {
        login: at,
        source: source.into(),
        logged_hour: now(),
        ..Default::default()
    };
    save(&c);
    END_TEXT.lock().unwrap().clear();
    max_reset();
}

pub fn renewed(exp: i64) -> Option<String> {
    let mut c = CUR.lock().unwrap();
    c.last_renew = now();
    c.token_exp = exp;
    save(&c);
    if c.login > 0 && now() - c.logged_hour >= 3600 {
        c.logged_hour = now();
        return Some(format!(
            "   portal login: logged in for {} (since {})",
            dur(now() - c.login),
            hms(c.login)
        ));
    }
    None
}

pub fn slept(secs: i64) {
    let mut c = CUR.lock().unwrap();
    c.slept += secs.max(0);
    save(&c);
}

pub fn history() -> Vec<i64> {
    let Some(d) = DIR.lock().unwrap().clone() else { return vec![] };
    let Ok(s) = std::fs::read_to_string(d.join(CSV)) else {
        return vec![];
    };
    s.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split(';').collect();
            if f.len() < 10 || f[8] != "no_renewal" || f[9] != "0" || f[2] == "unknown" {
                return None;
            }
            f[7].parse().ok()
        })
        .collect()
}

pub fn expectation(login: i64) -> Option<String> {
    let mut h = history();
    if h.is_empty() || login <= 0 {
        return None;
    }
    h.sort();
    let med = h[h.len() / 2];
    let list = h.iter().rev().take(5).map(|m| dur(m * 60)).collect::<Vec<_>>().join(", ");
    Some(format!(
        "   earlier portal logins ended by the portal after: {list} ({} time(s)) - if the portal has a fixed limit, this login ends around {}",
        h.len(),
        hms(login + med * 60)
    ))
}

fn clean(s: &str) -> String {
    crate::privacy::redact(s)
        .replace([';', '\n', '\r'], " ")
        .chars()
        .take(300)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn ended(version: &str, e: &End) -> Vec<String> {
    let c = std::mem::take(&mut *CUR.lock().unwrap());
    let t = now();

    let endt = if e.cause == "no_renewal" && c.token_exp > 0 && c.token_exp < t {
        c.token_exp
    } else {
        t
    };
    let secs = if c.login > 0 { endt - c.login } else { 0 };
    let mut out = vec!["=== Portal login ended - details for the evaluation (FetchJ_sessions.csv) ===".to_string()];
    if c.login > 0 {
        out.push(format!(
            "   login: {} ({}) - duration until the end: {}",
            stamp(c.login),
            c.source,
            dur(secs)
        ));
        *END_TEXT.lock().unwrap() = dur(secs);
    } else {
        out.push("   login: time unknown (the portal window was already logged in when FetchJ started)".into());
        END_TEXT.lock().unwrap().clear();
    }
    out.push(format!(
        "   last renewal of the access token: {} - last token valid until: {} - noticed: {}",
        hms(c.last_renew),
        hms(c.token_exp),
        hms(t)
    ));
    out.push(format!(
        "   cause as FetchJ sees it: {}",
        match e.cause.as_str() {
            "no_renewal" => "the web page stopped renewing the access token and the portal no longer accepts the session",
            "sleep" => "the computer was in sleep mode",
            _ => "the portal no longer accepts the session (access token still valid)",
        }
    ));
    let idle = e.idle_min.map(|m| m.to_string()).unwrap_or_default();
    out.push(format!(
        "   computer: sleep mode since the login {} min - last keyboard/mouse input {} min ago",
        c.slept / 60,
        if idle.is_empty() { "?".into() } else { idle.clone() }
    ));
    out.push(format!(
        "   portal page now: {} - notice on the page: {}",
        clean(&e.page),
        if e.notice.is_empty() { "none".into() } else { clean(&e.notice) }
    ));
    out.push(format!("   internet: {}", e.internet));
    let k = std::mem::take(&mut *KEEP.lock().unwrap());
    KEEP.lock().unwrap().setting = k.setting.clone();
    out.push(format!(
        "   keep-alive: technique / timer {} - last server check accepted by the portal: {}",
        if k.setting.is_empty() { "-".to_string() } else { k.setting.clone() },
        hms(k.server_ok)
    ));
    if k.last.is_empty() {
        out.push("   last keep-alive actions: none".into());
    } else {
        out.push("   last keep-alive actions:".into());
        for l in &k.last {
            out.push(format!("      {l}"));
        }
    }
    let row = [
        version.to_string(),
        stamp(c.login),
        if c.login > 0 { c.source.clone() } else { "unknown".into() },
        stamp(c.last_renew),
        stamp(c.token_exp),
        stamp(t),
        if c.login > 0 { dur(secs) } else { String::new() },
        if c.login > 0 {
            ((secs + 30) / 60).to_string()
        } else {
            String::new()
        },
        e.cause.clone(),
        (c.slept / 60).to_string(),
        idle,
        clean(&e.page),
        clean(&e.notice),
        clean(&e.internet),
        k.setting.clone(),
        stamp(k.server_ok),
        k.last.back().cloned().unwrap_or_default(),
    ]
    .join(";");
    if let Some(d) = DIR.lock().unwrap().clone() {
        let p = d.join(CSV);

        if let Ok(t) = std::fs::read_to_string(&p) {
            if t.lines().next() == Some(HEAD_OLD) {
                let _ = std::fs::write(&p, format!("{HEAD}{}", &t[HEAD_OLD.len()..]));
            }
        }
        let new = !p.exists();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            if new {
                let _ = writeln!(f, "{HEAD}");
            }
            let _ = writeln!(f, "{row}");
        }
        let _ = std::fs::remove_file(d.join(STATE));
    }
    out
}

pub fn idle_min() -> Option<i64> {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::SystemInformation::GetTickCount;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
        let mut li = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if GetLastInputInfo(&mut li) != 0 {
            let ms = GetTickCount().wrapping_sub(li.dwTime) as i64;
            return Some(ms / 60000);
        }
        None
    }
    #[cfg(not(windows))]
    None
}

pub fn reachable(host: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let Ok(addrs) = (host, 443).to_socket_addrs() else { return false };
    addrs
        .into_iter()
        .take(2)
        .any(|a| TcpStream::connect_timeout(&a, std::time::Duration::from_secs(5)).is_ok())
}

use crate::ids::IdMap;
use once_cell::sync::Lazy;
use regex::Regex;
use std::sync::Mutex;

static REDACT: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    vec![
        (Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._\-]+").unwrap(), "${1}<hidden>"),
        (
            Regex::new(r"(?i)((?:authorization|cookie|sec_ch_sucontentsys|xyz-bitacoratransaccionbase|p_archivo)\s*[:=]\s*).*")
                .unwrap(),
            "${1}<hidden>",
        ),
        (Regex::new(r"eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+").unwrap(), "<token hidden>"),
        (
            Regex::new(
                r#"(?i)([?&#](?:code|state|id_token|access_token|refresh_token|session_state|client_info|p|x-client-ver|nonce)=)[^&#\s"']+"#,
            )
            .unwrap(),
            "${1}<hidden>",
        ),
        (
            Regex::new(
                r#"(?i)((?:asp\.net_sessionid|jsessionid|phpsessid|sessionid|session_id|x-ms-cpim-[\w.-]+|\.aspnetcore\.[\w.-]+)\s*[:=]\s*)[^\s;,"']+"#,
            )
            .unwrap(),
            "${1}<hidden>",
        ),
    ]
});

pub fn redact(text: &str) -> String {
    let mut t = text.to_string();
    for (rx, rep) in REDACT.iter() {
        t = rx.replace_all(&t, *rep).to_string();
    }
    t
}

static ANON_CASE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(^|[^0-9])(\d{2}-\d{6}-\d{4}-[A-Za-z]{2})($|[^A-Za-z])").unwrap());
static ANON_GUID: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(^|[^0-9A-Fa-f])([0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12})($|[^0-9A-Fa-f])").unwrap()
});
static ANON_NUM: Lazy<Regex> = Lazy::new(|| Regex::new(r"(^|[^0-9])(\d{6,10})($|[^0-9])").unwrap());
static ANON_FOLDER: Lazy<Regex> = Lazy::new(|| Regex::new(r"(^|[^0-9])(\d{14})($|[^0-9])").unwrap());

fn replace_bounded(rx: &Regex, text: &str, f: &mut dyn FnMut(&str) -> String) -> String {
    let mut cur = text.to_string();
    for _ in 0..8 {
        let mut changed = false;
        let next = rx
            .replace_all(&cur, |c: &regex::Captures| {
                let r = f(&c[2]);
                if r != c[2] {
                    changed = true;
                }
                format!("{}{}{}", &c[1], r, &c[3])
            })
            .to_string();
        cur = next;
        if !changed {
            break;
        }
    }
    cur
}

pub static HOME: Lazy<String> = Lazy::new(|| dirs::home_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());

pub fn anon(text: &str, ids: Option<&Mutex<IdMap>>) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut t = text.to_string();
    match ids.and_then(|m| m.lock().ok()) {
        Some(mut ids) => {
            t = replace_bounded(&ANON_CASE, &t, &mut |s| ids.case(s, true));
            t = replace_bounded(&ANON_GUID, &t, &mut |s| ids.doc(s, true));
            t = replace_bounded(&ANON_NUM, &t, &mut |s| {
                let d = ids.doc(s, false);
                if d.is_empty() {
                    s.to_string()
                } else {
                    d
                }
            });
        }
        None => {
            t = replace_bounded(&ANON_CASE, &t, &mut |_| "E?".into());
            t = replace_bounded(&ANON_GUID, &t, &mut |_| "D?".into());
        }
    }
    t = replace_bounded(&ANON_FOLDER, &t, &mut |_| "#".into());
    if HOME.chars().count() > 3 {
        let rx = Regex::new(&format!("(?i){}", regex::escape(&HOME))).unwrap();
        t = rx.replace_all(&t, "~").to_string();
    }
    t
}

pub fn scrub(text: &str, ids: Option<&Mutex<IdMap>>) -> String {
    anon(&redact(text), ids)
}

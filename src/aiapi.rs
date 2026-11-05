use crate::aipack::Pack;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const API: &str = "https://api.anthropic.com/v1";

fn api_base() -> String {
    #[cfg(test)]
    if let Ok(b) = std::env::var("FJ_TEST_API_BASE") {
        return b;
    }
    API.to_string()
}

const POLL: u64 = if cfg!(test) { 1 } else { 20 };

fn key_file(state: &Path) -> PathBuf {
    state.join("FetchJ_ai_key.bin")
}

pub fn key_stored(state: &Path) -> bool {
    key_file(state).is_file()
}

pub fn key_save(state: &Path, key: &str) -> Result<(), String> {
    let k = key.trim();
    if k.len() < 20 || k.contains(char::is_whitespace) || !k.is_ascii() {
        return Err("format".into());
    }
    let enc = crate::totp::protect(k.as_bytes()).ok_or_else(|| "os".to_string())?;
    let _ = std::fs::create_dir_all(state);
    std::fs::write(key_file(state), enc).map_err(|_| "io".to_string())
}

pub fn key_clear(state: &Path) {
    let _ = std::fs::remove_file(key_file(state));
}

fn key(state: &Path) -> Result<String, String> {
    #[cfg(test)]
    if let Ok(k) = std::env::var("FJ_TEST_API_KEY") {
        return Ok(k);
    }
    let enc = std::fs::read(key_file(state)).map_err(|_| "nokey".to_string())?;
    let k = crate::totp::unprotect(&enc).ok_or_else(|| "nokey".to_string())?;
    String::from_utf8(k).map_err(|_| "nokey".to_string())
}

fn content(p: &Pack, keys: &[String], prompt: &str, case: Option<(&Path, &str)>, cache: &str) -> Vec<Value> {
    let t = crate::aipack::texts(&p.lang);
    let mut c = Vec::new();
    let doc = |path: &Path, title: &str| -> Option<Value> {
        let s = std::fs::read_to_string(path).ok().filter(|s| !s.trim().is_empty())?;
        Some(json!({ "type": "document", "source": { "type": "text", "media_type": "text/plain", "data": s }, "title": title }))
    };
    if keys.iter().any(|k| k == "case") {
        let (path, title) = case.unwrap_or((&p.api_case, t["api_case"].as_str().unwrap_or("Case file")));
        if let Some(d) = doc(path, title) {
            c.push(d);
        }
    }
    if keys.iter().any(|k| k == "research") {
        if let Some(d) = doc(&p.api_research, t["api_rs"].as_str().unwrap_or("Research")) {
            c.push(d);
        }
    }

    if let Some(last) = c.last_mut() {
        match cache {
            "1h" => last["cache_control"] = json!({ "type": "ephemeral", "ttl": "1h" }),
            "5m" => last["cache_control"] = json!({ "type": "ephemeral" }),
            _ => {}
        }
    }
    let media = keys.iter().any(|k| k != "case" && k != "research");
    let mut text = prompt.to_string();
    if media {
        text.push_str("\n\n");
        text.push_str(t["api_audio_note"].as_str().unwrap_or(""));
    }
    c.push(json!({ "type": "text", "text": text }));
    c
}

const SUM_V: &str = "s1";

const SHORT: usize = 1500;

const SUM_MAX: u32 = 4000;

const PAR: usize = 4;

static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn stop() {
    STOP.store(true, std::sync::atomic::Ordering::SeqCst);
}
fn stopped() -> bool {
    STOP.load(std::sync::atomic::Ordering::SeqCst)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    pub head: String,
    pub body: String,
}

pub fn split_docs(text: &str) -> Vec<Doc> {
    let mut out: Vec<Doc> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.len() > 12 && l.starts_with("===== ") && l.ends_with(" =====") {
            out.push(Doc {
                head: l[6..l.len() - 6].trim().to_string(),
                body: String::new(),
            });
        } else if let Some(d) = out.last_mut() {
            d.body.push_str(line);
            d.body.push('\n');
        }
    }
    for d in &mut out {
        d.body = d.body.trim().to_string();
    }
    out
}

fn short(d: &Doc) -> bool {
    d.body.chars().count() < SHORT
}

fn sum_key(model: &str, d: &Doc) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(format!("{SUM_V}|{model}|{}", d.body).as_bytes()))
}

fn sum_dir(p: &Pack) -> PathBuf {
    p.dir
        .parent()
        .map(|x| x.join("_summaries"))
        .unwrap_or_else(|| p.dir.join("_summaries"))
}

fn cached(dir: &Path, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{key}.json"))).ok()?).ok()?;
    v["text"].as_str().filter(|s| !s.trim().is_empty()).map(|s| s.to_string())
}

fn keep(dir: &Path, key: &str, model: &str, text: &str, usage: &Value) {
    let _ = std::fs::create_dir_all(dir);
    let v = json!({ "v": SUM_V, "model": model, "at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(), "text": text, "usage": usage });
    let _ = std::fs::write(dir.join(format!("{key}.json")), serde_json::to_vec_pretty(&v).unwrap_or_default());
}

pub fn with_effort(mut body: Value, level: &str) -> Value {
    let m = body["model"].as_str().unwrap_or("").to_lowercase();
    let old = m.contains("4-5") || m.contains("3-") || m.contains("-4-0") || m.contains("-4-1") || m.ends_with("-4") || m.contains("-4-2");
    if !old && !m.is_empty() {
        body["output_config"] = json!({ "effort": level });
    }
    body
}

fn sum_params(lang: &str, model: &str, d: &Doc) -> Value {
    let t = crate::aipack::texts(lang);
    with_effort(
        json!({
            "model": model, "max_tokens": SUM_MAX,
            "system": t["sum_sys"].as_str().unwrap_or("Summarise this document of a court case file objectively."),
            "messages": [{ "role": "user", "content": format!("{}\n\n{}", d.head, d.body) }],
        }),
        "low",
    )
}

#[derive(Default, Debug)]
pub struct DocPlan {
    pub n: usize,
    pub short: usize,
    pub cached: usize,
    pub todo: usize,
    pub chars_todo: usize,
    pub chars_short: usize,
    pub chars_cached: usize,
    pub chars_all: usize,
    pub pending: bool,
}

pub fn doc_plan(p: &Pack, model_doc: &str) -> DocPlan {
    let text = std::fs::read_to_string(&p.api_case).unwrap_or_default();
    let dir = sum_dir(p);
    let mut o = DocPlan {
        chars_all: text.chars().count(),
        ..Default::default()
    };
    for d in split_docs(&text) {
        o.n += 1;
        let c = d.body.chars().count() + d.head.chars().count();
        if short(&d) {
            o.short += 1;
            o.chars_short += c;
        } else if let Some(s) = cached(&dir, &sum_key(model_doc, &d)) {
            o.cached += 1;
            o.chars_cached += s.chars().count() + d.head.chars().count();
        } else {
            o.todo += 1;
            o.chars_todo += c;
        }
    }
    o.pending = std::fs::read_dir(&dir)
        .map(|r| r.flatten().any(|e| e.file_name().to_string_lossy().starts_with("_batch_")))
        .unwrap_or(false);
    o
}

pub struct Sums {
    pub texts: Vec<Option<String>>,
    pub docs: Vec<Doc>,
    pub new: usize,
    pub failed: usize,

    pub pending: Option<(String, u64, u64)>,
    pub tin: u64,
    pub tout: u64,
}

fn again(code: u16) -> bool {
    code == 429 || code == 529 || code >= 500
}

fn one(state: &Path, p: &Pack, model: &str, d: &Doc) -> Result<(String, Value), (u16, String)> {
    let body = sum_params(&p.lang, model, d);
    let mut last = (0u16, String::new());
    for k in 0..3 {
        if k > 0 {
            for _ in 0..(10 * k) {
                if stopped() {
                    return Err((0, "stopped".into()));
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        match call(state, &p.dir, "messages", &body, 600) {
            Ok((200, v)) => {
                let t = answer_text(&v);
                if t.trim().is_empty() {
                    return Err((200, v["stop_reason"].as_str().unwrap_or("empty").to_string()));
                }
                return Ok((t, v["usage"].clone()));
            }
            Ok((code, v)) => {
                last = (code, err_of(code, &v));
                if !again(code) {
                    break;
                }
            }
            Err(e) => last = (0, e),
        }
    }
    Err(last)
}

fn batch_take(state: &Path, p: &Pack, dir: &Path, b: &Value, ids: &serde_json::Map<String, Value>, model: &str) -> (u64, u64) {
    let (mut tin, mut tout) = (0, 0);
    let url = b["results_url"].as_str().unwrap_or("");
    if url.is_empty() {
        return (0, 0);
    }
    let Ok((200, raw)) = req(state, &p.dir, url, None, 600) else {
        return (0, 0);
    };
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(key) = v["custom_id"].as_str().and_then(|c| ids.get(c)).and_then(|k| k.as_str()) else {
            continue;
        };
        if v["result"]["type"] == "succeeded" {
            let m = &v["result"]["message"];
            let t = answer_text(m);
            if !t.trim().is_empty() {
                keep(dir, key, model, &t, &m["usage"]);
                tin += m["usage"]["input_tokens"].as_u64().unwrap_or(0);
                tout += m["usage"]["output_tokens"].as_u64().unwrap_or(0);
            }
        }
    }
    (tin, tout)
}

fn batches_open(state: &Path, p: &Pack, dir: &Path) -> Vec<(String, Vec<String>, u64, u64)> {
    let mut open = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return open };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if !n.starts_with("_batch_") || !n.ends_with(".json") {
            continue;
        }
        let Ok(note) = std::fs::read_to_string(e.path()).map(|s| serde_json::from_str::<Value>(&s).unwrap_or(Value::Null)) else {
            continue;
        };
        let id = note["id"].as_str().unwrap_or("").to_string();
        let model = note["model"].as_str().unwrap_or("").to_string();
        let ids = note["ids"].as_object().cloned().unwrap_or_default();
        match req(state, &p.dir, &format!("messages/batches/{id}"), None, 60) {
            Ok((200, raw)) => {
                let b: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                if b["processing_status"] == "ended" {
                    let (i, o) = batch_take(state, p, dir, &b, &ids, &model);
                    crate::focus::app_event(&format!(
                        "AI handover: batch {id} ended – summaries taken over ({i} input / {o} output tokens)"
                    ));
                    let _ = std::fs::remove_file(e.path());
                } else {
                    let rc = &b["request_counts"];
                    let done = ["succeeded", "errored", "canceled", "expired"]
                        .iter()
                        .map(|k| rc[*k].as_u64().unwrap_or(0))
                        .sum::<u64>();
                    let total = done + rc["processing"].as_u64().unwrap_or(0);
                    open.push((
                        id,
                        ids.values().filter_map(|v| v.as_str().map(|s| s.to_string())).collect(),
                        done,
                        total,
                    ));
                }
            }

            Ok((404, _)) => {
                let _ = std::fs::remove_file(e.path());
            }
            _ => {}
        }
    }
    open
}

pub fn summarize(
    state: &Path,
    p: &Pack,
    model: &str,
    batch: bool,
    wait_min: u32,
    progress: &(dyn Fn(Value) + Sync),
) -> Result<Sums, String> {
    let docs = split_docs(&std::fs::read_to_string(&p.api_case).unwrap_or_default());
    let dir = sum_dir(p);
    let _ = std::fs::create_dir_all(&dir);
    let mut out = Sums {
        texts: vec![None; docs.len()],
        docs: docs.clone(),
        new: 0,
        failed: 0,
        pending: None,
        tin: 0,
        tout: 0,
    };
    let open = batches_open(state, p, &dir);
    let keys: Vec<String> = docs.iter().map(|d| sum_key(model, d)).collect();
    let mut todo: Vec<usize> = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        if short(d) {
            continue;
        }
        match cached(&dir, &keys[i]) {
            Some(s) => out.texts[i] = Some(s),
            None => todo.push(i),
        }
    }
    let total = todo.len() as u64;
    if todo.is_empty() {
        return Ok(out);
    }
    if batch {
        let running: std::collections::HashSet<&String> = open.iter().flat_map(|o| o.1.iter()).collect();
        let fresh: Vec<usize> = todo.iter().copied().filter(|i| !running.contains(&keys[*i])).collect();
        let mut ids_running: Vec<String> = open.iter().map(|o| o.0.clone()).collect();
        if !fresh.is_empty() {
            let mut ids = serde_json::Map::new();
            let reqs: Vec<Value> = fresh
                .iter()
                .map(|i| {
                    let cid = format!("d{}_{}", i, &keys[*i][..16]);
                    ids.insert(cid.clone(), json!(keys[*i]));
                    json!({ "custom_id": cid, "params": sum_params(&p.lang, model, &docs[*i]) })
                })
                .collect();
            let (code, v) = call(state, &p.dir, "messages/batches", &json!({ "requests": reqs }), 600)?;
            if code != 200 {
                return Err(err_of(code, &v));
            }
            let id = v["id"].as_str().unwrap_or("").to_string();
            let note = json!({ "id": id, "model": model, "at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(), "case": p.case, "ids": ids });
            let _ = std::fs::write(
                dir.join(format!("_batch_{id}.json")),
                serde_json::to_vec_pretty(&note).unwrap_or_default(),
            );
            crate::focus::app_event(&format!(
                "AI handover: batch {id} started – {} document(s) to summarise with {model}",
                fresh.len()
            ));
            ids_running.push(id);
        }
        let until = std::time::Instant::now() + std::time::Duration::from_secs(wait_min.max(1) as u64 * 60);
        loop {
            let left: Vec<usize> = todo.iter().copied().filter(|i| cached(&dir, &keys[*i]).is_none()).collect();
            let open = batches_open(state, p, &dir);
            let left_after: Vec<usize> = left.iter().copied().filter(|i| cached(&dir, &keys[*i]).is_none()).collect();
            let done = total - left_after.len() as u64;
            progress(json!({ "phase": "batch", "done": done, "total": total }));

            if left_after.is_empty() || open.is_empty() {
                break;
            }
            if stopped() || std::time::Instant::now() >= until {
                let id = open.first().map(|o| o.0.clone()).unwrap_or_default();
                out.pending = Some((id, done, total));
                break;
            }
            for _ in 0..POLL {
                if stopped() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        for i in &todo {
            if let Some(s) = cached(&dir, &keys[*i]) {
                out.texts[*i] = Some(s);
                out.new += 1;
            } else if out.pending.is_none() {
                out.failed += 1;
            }
        }
        return Ok(out);
    }

    let done = std::sync::atomic::AtomicU64::new(0);
    let fatal: Mutex<Option<String>> = Mutex::new(None);
    let res: Mutex<Vec<(usize, String, Value)>> = Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    progress(json!({ "phase": "sum", "done": 0, "total": total }));
    std::thread::scope(|sc| {
        for _ in 0..PAR.min(todo.len()) {
            sc.spawn(|| loop {
                let j = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if j >= todo.len() || stopped() || fatal.lock().unwrap().is_some() {
                    break;
                }
                let i = todo[j];
                match one(state, p, model, &docs[i]) {
                    Ok((t, u)) => {
                        keep(&dir, &keys[i], model, &t, &u);
                        res.lock().unwrap().push((i, t, u));
                    }

                    Err((code, e)) if code == 401 || code == 403 || (code == 400 && e.contains("credit")) => {
                        *fatal.lock().unwrap() = Some(e);
                    }
                    Err(_) => {}
                }
                let d = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                progress(json!({ "phase": "sum", "done": d, "total": total }));
            });
        }
    });
    if let Some(e) = fatal.into_inner().unwrap() {
        return Err(e);
    }
    for (i, t, u) in res.into_inner().unwrap() {
        out.tin += u["input_tokens"].as_u64().unwrap_or(0);
        out.tout += u["output_tokens"].as_u64().unwrap_or(0);
        out.texts[i] = Some(t);
        out.new += 1;
    }
    if stopped() {
        return Err("stopped".into());
    }
    out.failed = todo.len() - out.new;
    Ok(out)
}

fn sums_text(p: &Pack, s: &Sums, model: &str) -> String {
    let t = crate::aipack::texts(&p.lang);
    let mark_s = t["sum_mark"].as_str().unwrap_or("[summary]");
    let mark_f = t["sum_mark_full"].as_str().unwrap_or("[full text]");
    let mut o = t["sum_head"].as_str().unwrap_or("").replace("{m}", model);
    for (d, x) in s.docs.iter().zip(&s.texts) {
        o.push_str(&format!("\n\n===== {} =====\n", d.head));
        match x {
            Some(x) => {
                o.push_str(mark_s);
                o.push('\n');
                o.push_str(x.trim());
            }
            None => {
                o.push_str(mark_f);
                o.push('\n');
                o.push_str(&d.body);
            }
        }
    }
    o.push('\n');
    o
}

fn req(state: &Path, dir: &Path, url: &str, body: Option<&Value>, max_time: u32) -> Result<(u16, String), String> {
    let k = key(state)?;

    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let tmp = dir.join(format!(
        "_api_{}_{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let hdr = tmp.join("h.txt");
    let bf = tmp.join("b.json");
    let of = tmp.join("o.json");
    std::fs::write(
        &hdr,
        format!("x-api-key: {k}\nanthropic-version: 2023-06-01\ncontent-type: application/json\n"),
    )
    .map_err(|e| e.to_string())?;
    let mut c = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    c.args(["-sS", "--max-time", &max_time.to_string(), "-o"])
        .arg(&of)
        .args(["-w", "%{http_code}"])
        .arg("-H")
        .arg(format!("@{}", hdr.to_string_lossy()));
    if let Some(b) = body {
        std::fs::write(&bf, serde_json::to_vec(b).unwrap_or_default()).map_err(|e| e.to_string())?;
        c.args(["-X", "POST", "--data-binary"]).arg(format!("@{}", bf.to_string_lossy()));
    }
    c.arg(if url.starts_with("https://") || url.starts_with("http://") {
        url.to_string()
    } else {
        format!("{}/{url}", api_base())
    });
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let r = c.output();

    let _ = std::fs::remove_file(&hdr);
    let out = std::fs::read_to_string(&of).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&tmp);
    let o = r.map_err(|e| e.to_string())?;
    let code: u16 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0);
    if code == 0 {
        let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
        return Err(if e.is_empty() { "no answer".into() } else { e });
    }
    Ok((code, out))
}

fn call(state: &Path, dir: &Path, path: &str, body: &Value, max_time: u32) -> Result<(u16, Value), String> {
    let (code, out) = req(state, dir, path, Some(body), max_time)?;
    Ok((code, serde_json::from_str(&out).unwrap_or(Value::Null)))
}

fn err_of(code: u16, v: &Value) -> String {
    let m = v["error"]["message"].as_str().unwrap_or("").to_string();
    if m.is_empty() {
        format!("HTTP {code}")
    } else {
        format!("HTTP {code} – {m}")
    }
}

pub fn count(state: &Path, p: &Pack, keys: &[String], prompt: &str, model: &str, model_doc: &str) -> Result<Value, String> {
    let body = json!({ "model": model, "messages": [{ "role": "user", "content": content(p, keys, prompt, None, "off") }] });
    let (code, v) = call(state, &p.dir, "messages/count_tokens", &body, 120)?;
    if code != 200 {
        return Err(err_of(code, &v));
    }
    let has = |k: &str| keys.iter().any(|x| x == k);
    let chars = |f: &Path| std::fs::read_to_string(f).map(|s| s.chars().count()).unwrap_or(0);
    let case = if has("case") { chars(&p.api_case) } else { 0 };
    let rs = if has("research") { chars(&p.api_research) } else { 0 };
    let d = if has("case") { doc_plan(p, model_doc) } else { DocPlan::default() };
    Ok(json!({
        "tokens": v["input_tokens"], "model": model, "model_doc": model_doc,
        "chars": { "case": case, "research": rs, "prompt": prompt.chars().count() },
        "docs": { "n": d.n, "short": d.short, "cached": d.cached, "todo": d.todo, "chars_todo": d.chars_todo, "chars_short": d.chars_short, "chars_cached": d.chars_cached, "pending": d.pending },
    }))
}

pub fn prices(s: &str) -> Value {
    let mut m = serde_json::Map::new();

    let mut parts: Vec<String> = Vec::new();
    for x in s.split([',', ';']) {
        match parts.last_mut() {
            Some(l) if x.trim_start().starts_with(|c: char| c.is_ascii_digit()) => {
                l.push(',');
                l.push_str(x);
            }
            _ => parts.push(x.to_string()),
        }
    }
    for part in &parts {
        let Some((k, v)) = part.split_once('=') else { continue };
        let Some((i, o)) = v.split_once('/') else { continue };
        let num = |x: &str| x.trim().replace(',', ".").parse::<f64>().ok().filter(|n| *n >= 0.0);
        if let (Some(i), Some(o)) = (num(i), num(o)) {
            if !k.trim().is_empty() {
                m.insert(k.trim().to_string(), json!([i, o]));
            }
        }
    }
    Value::Object(m)
}

#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub per_doc: bool,
    pub model_doc: String,

    pub batch: bool,

    pub batch_wait_min: u32,

    pub cache: String,
}

fn meta(v: &Value, max_tokens: u32, when: &str) -> Value {
    json!({ "at": when, "model": v["model"], "stop_reason": v["stop_reason"], "usage": v["usage"], "max_tokens": max_tokens })
}

pub fn translate(state: &Path, dir: &Path, model: &str, text: &str, from: &str, to: &str) -> Result<(String, u64, u64), String> {
    let name = |c: &str| {
        match c {
            "es" => "Spanish",
            "de" => "German",
            "en" => "English",
            "pt" => "Portuguese",
            x => x,
        }
        .to_string()
    };
    let system = format!(
        "You translate court documents from {} into {}. Translate faithfully and completely, line by line: keep every line break,          every number, date, case number, name, address and abbreviation as it stands; legal terms in the usual {} legal language.          Output only the translation – no comments, no notes, no headings of your own.",
        name(from),
        name(to),
        name(to)
    );

    let body = with_effort(
        json!({ "model": model, "max_tokens": 24000, "system": system, "messages": [{ "role": "user", "content": text }] }),
        "low",
    );
    let (code, v) = call(state, dir, "messages", &body, 300)?;
    if code != 200 {
        return Err(err_of(code, &v));
    }
    let t = answer_text(&v);
    if t.trim().is_empty() {
        return Err("empty".into());
    }
    Ok((
        t,
        v["usage"]["input_tokens"].as_u64().unwrap_or(0),
        v["usage"]["output_tokens"].as_u64().unwrap_or(0),
    ))
}

fn answer_text(v: &Value) -> String {
    v["content"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|b| b["type"] == "text" || b["type"].is_null())
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
pub fn send(
    state: &Path,
    p: &Pack,
    keys: &[String],
    prompt: &str,
    model: &str,
    max_tokens: u32,
    o: &Opts,
    progress: &(dyn Fn(Value) + Sync),
) -> Result<Value, String> {
    STOP.store(false, std::sync::atomic::Ordering::SeqCst);
    let when = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let t = crate::aipack::texts(&p.lang);

    let mut sum_file: Option<PathBuf> = None;
    let mut sum_info = Value::Null;
    if o.per_doc && keys.iter().any(|k| k == "case") {
        let s = match summarize(state, p, &o.model_doc, o.batch, o.batch_wait_min, progress) {
            Ok(s) => s,
            Err(e) if e == "stopped" => return Ok(json!({ "stopped": true })),
            Err(e) => return Err(e),
        };
        if let Some((id, done, total)) = &s.pending {
            crate::focus::app_event(&format!(
                "AI handover: batch {id} not ended yet ({done} of {total}) – the answer is asked for later"
            ));
            return Ok(json!({ "pending": true, "batch": id, "done": done, "total": total }));
        }
        if stopped() {
            return Ok(json!({ "stopped": true }));
        }
        let text = sums_text(p, &s, &o.model_doc);
        let f = p.dir.join("_api_case_summaries.txt");
        std::fs::write(&f, &text).map_err(|e| e.to_string())?;

        let name = t["sum_file"].as_str().unwrap_or("Summaries").to_string();
        let md = p.dir.join(format!("{name}_{when}.md"));
        let _ = std::fs::write(&md, if p.anon { crate::aipack::back(p, &text) } else { text.clone() });
        crate::focus::app_event(&format!(
            "AI handover: {} document(s) – {} summarised now, {} from earlier, {} as they are (short or without a summary); {} input / {} output tokens for the summaries ({})",
            s.docs.len(), s.new, s.texts.iter().filter(|x| x.is_some()).count() - s.new, s.texts.iter().filter(|x| x.is_none()).count(), s.tin, s.tout, o.model_doc
        ));
        sum_info = json!({ "n": s.docs.len(), "new": s.new, "failed": s.failed, "tin": s.tin, "tout": s.tout, "model": o.model_doc, "path": crate::paths::plain_path(&md) });
        sum_file = Some(f);
    }
    progress(json!({ "phase": "main", "model": model }));
    let title = t["api_case_sum"]
        .as_str()
        .unwrap_or("Case file – summaries per document")
        .to_string();
    let case = sum_file.as_deref().map(|f| (f, title.as_str()));

    let body = with_effort(
        json!({ "model": model, "max_tokens": max_tokens, "messages": [{ "role": "user", "content": content(p, keys, prompt, case, &o.cache) }] }),
        "medium",
    );
    let (code, v) = call(state, &p.dir, "messages", &body, 1800)?;
    if code != 200 {
        return Err(err_of(code, &v));
    }
    let text = answer_text(&v);
    let m = meta(&v, max_tokens, &when);
    let stop = v["stop_reason"].as_str().unwrap_or("").to_string();
    let tin = v["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let tout = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
    if text.trim().is_empty() {
        let _ = std::fs::write(
            p.dir.join(format!("Claude_{when}_empty.json")),
            serde_json::to_vec_pretty(&m).unwrap_or_default(),
        );
        crate::focus::app_event(&format!(
            "AI handover: the Claude API answered without text (stop reason {stop}, {tin} input / {tout} output tokens, model {model})"
        ));
        return Ok(json!({ "empty": true, "stop": stop, "usage": v["usage"], "model": v["model"], "sums": sum_info }));
    }
    let f = p.dir.join(format!("Claude_{when}.md"));
    std::fs::write(&f, &text).map_err(|e| e.to_string())?;
    let _ = std::fs::write(
        p.dir.join(format!("Claude_{when}.json")),
        serde_json::to_vec_pretty(&m).unwrap_or_default(),
    );
    let back = crate::aipack::back(p, &text);
    let fb = if p.anon {
        let x = p.dir.join(format!("Claude_{when}_names.md"));
        std::fs::write(&x, &back).map_err(|e| e.to_string())?;
        Some(x)
    } else {
        None
    };
    crate::focus::app_event(&format!("AI handover: answer of the Claude API saved ({} characters, stop reason {stop}, {tin} input / {tout} output tokens, model {model})", text.chars().count()));
    Ok(json!({
        "text": text, "back": back, "path": crate::paths::plain_path(&f), "path_back": fb.map(|x| crate::paths::plain_path(&x)),
        "usage": v["usage"], "stop": stop, "model": v["model"], "sums": sum_info,
    }))
}

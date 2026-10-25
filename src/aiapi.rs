use crate::aipack::Pack;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const API: &str = "https://api.anthropic.com/v1";

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
    let enc = std::fs::read(key_file(state)).map_err(|_| "nokey".to_string())?;
    let k = crate::totp::unprotect(&enc).ok_or_else(|| "nokey".to_string())?;
    String::from_utf8(k).map_err(|_| "nokey".to_string())
}

fn content(p: &Pack, keys: &[String], prompt: &str) -> Vec<Value> {
    let t = crate::aipack::texts(&p.lang);
    let mut c = Vec::new();
    let doc = |path: &Path, title: &str| -> Option<Value> {
        let s = std::fs::read_to_string(path).ok().filter(|s| !s.trim().is_empty())?;
        Some(json!({ "type": "document", "source": { "type": "text", "media_type": "text/plain", "data": s }, "title": title }))
    };
    if keys.iter().any(|k| k == "case") {
        if let Some(d) = doc(&p.api_case, t["api_case"].as_str().unwrap_or("Case file")) {
            c.push(d);
        }
    }
    if keys.iter().any(|k| k == "research") {
        if let Some(d) = doc(&p.api_research, t["api_rs"].as_str().unwrap_or("Research")) {
            c.push(d);
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

fn call(state: &Path, dir: &Path, path: &str, body: &Value, max_time: u32) -> Result<(u16, Value), String> {
    let k = key(state)?;
    let tmp = dir.join(format!("_api_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let hdr = tmp.join("h.txt");
    let bf = tmp.join("b.json");
    let of = tmp.join("o.json");
    std::fs::write(
        &hdr,
        format!("x-api-key: {k}\nanthropic-version: 2023-06-01\ncontent-type: application/json\n"),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(&bf, serde_json::to_vec(body).unwrap_or_default()).map_err(|e| e.to_string())?;
    let mut c = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    c.args(["-sS", "--max-time", &max_time.to_string(), "-X", "POST", "-o"])
        .arg(&of)
        .args(["-w", "%{http_code}"])
        .arg("-H")
        .arg(format!("@{}", hdr.to_string_lossy()))
        .arg("--data-binary")
        .arg(format!("@{}", bf.to_string_lossy()))
        .arg(format!("{API}/{path}"));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let r = c.output();

    let _ = std::fs::remove_file(&hdr);
    let out = std::fs::read_to_string(&of).ok();
    let _ = std::fs::remove_dir_all(&tmp);
    let o = r.map_err(|e| e.to_string())?;
    let code: u16 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0);
    if code == 0 {
        let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
        return Err(if e.is_empty() { "no answer".into() } else { e });
    }
    let v: Value = out.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    Ok((code, v))
}

fn err_of(code: u16, v: &Value) -> String {
    let m = v["error"]["message"].as_str().unwrap_or("").to_string();
    if m.is_empty() {
        format!("HTTP {code}")
    } else {
        format!("HTTP {code} – {m}")
    }
}

pub fn count(state: &Path, p: &Pack, keys: &[String], prompt: &str, model: &str) -> Result<Value, String> {
    let body = json!({ "model": model, "messages": [{ "role": "user", "content": content(p, keys, prompt) }] });
    let (code, v) = call(state, &p.dir, "messages/count_tokens", &body, 120)?;
    if code != 200 {
        return Err(err_of(code, &v));
    }
    Ok(json!({ "tokens": v["input_tokens"], "model": model }))
}

fn meta(v: &Value, max_tokens: u32, when: &str) -> Value {
    json!({ "at": when, "model": v["model"], "stop_reason": v["stop_reason"], "usage": v["usage"], "max_tokens": max_tokens })
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

pub fn send(state: &Path, p: &Pack, keys: &[String], prompt: &str, model: &str, max_tokens: u32) -> Result<Value, String> {
    let body = json!({ "model": model, "max_tokens": max_tokens, "messages": [{ "role": "user", "content": content(p, keys, prompt) }] });
    let (code, v) = call(state, &p.dir, "messages", &body, 1800)?;
    if code != 200 {
        return Err(err_of(code, &v));
    }
    let text = answer_text(&v);
    let when = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
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
        return Ok(json!({ "empty": true, "stop": stop, "usage": v["usage"], "model": v["model"] }));
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
        "usage": v["usage"], "stop": stop, "model": v["model"],
    }))
}

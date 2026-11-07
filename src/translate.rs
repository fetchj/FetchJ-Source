use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const GOOGLE_PIECE: usize = 1800;

pub const CLAUDE_PIECE: usize = 12000;

pub fn pieces(text: &str, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        if cur.chars().count() + line.chars().count() > max && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if line.chars().count() > max {
            for w in line.split_inclusive(' ') {
                if cur.chars().count() + w.chars().count() > max && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                cur.push_str(w);
            }
        } else {
            cur.push_str(line);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn google_answer(v: &Value) -> String {
    v.get(0)
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.get(0).and_then(|t| t.as_str())).collect::<String>())
        .unwrap_or_default()
}

fn curl() -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

fn google_piece(tmp: &Path, text: &str, from: &str, to: &str) -> Result<String, String> {
    let qf = tmp.join("q.txt");
    std::fs::write(&qf, text).map_err(|e| e.to_string())?;
    let o = curl()
        .args(["-sS", "--max-time", "60", "-G", "-A", "Mozilla/5.0", "-w", "\n%{http_code}"])
        .arg("--data-urlencode")
        .arg("client=gtx")
        .arg("--data-urlencode")
        .arg(format!("sl={from}"))
        .arg("--data-urlencode")
        .arg(format!("tl={to}"))
        .arg("--data-urlencode")
        .arg("dt=t")
        .arg("--data-urlencode")
        .arg(format!("q@{}", qf.to_string_lossy()))
        .arg("https://translate.googleapis.com/translate_a/single")
        .output()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&qf);
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    let (body, code) = out.rsplit_once('\n').unwrap_or(("", "0"));
    let code: u16 = code.trim().parse().unwrap_or(0);
    if code != 200 {
        let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
        return Err(if code == 0 {
            if e.is_empty() {
                "no answer".into()
            } else {
                e
            }
        } else {
            format!("HTTP {code}")
        });
    }
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(google_answer(&v))
}

pub fn google(tmp: &Path, text: &str, from: &str, to: &str, progress: &dyn Fn(usize, usize)) -> Result<String, String> {
    let ps = pieces(text, GOOGLE_PIECE);
    let mut out = String::new();
    for (i, p) in ps.iter().enumerate() {
        progress(i, ps.len());
        if p.trim().is_empty() {
            out.push_str(p);
            continue;
        }
        let t = google_piece(tmp, p, from, to)?;
        out.push_str(&t);

        if p.ends_with('\n') && !t.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn claude(
    state: &Path,
    tmp: &Path,
    model: &str,
    text: &str,
    from: &str,
    to: &str,
    progress: &dyn Fn(usize, usize),
) -> Result<(String, u64, u64), String> {
    let ps = pieces(text, CLAUDE_PIECE);
    let (mut out, mut tin, mut tout) = (String::new(), 0, 0);
    for (i, p) in ps.iter().enumerate() {
        progress(i, ps.len());
        if p.trim().is_empty() {
            out.push_str(p);
            continue;
        }
        let (t, a, b) = crate::aiapi::translate(state, tmp, model, p, from, to)?;
        out.push_str(t.trim_end_matches('\n'));
        out.push('\n');
        tin += a;
        tout += b;
    }
    Ok((out, tin, tout))
}

pub fn model_url_ok(url: &str) -> bool {
    [
        "https://bergamot.s3.amazonaws.com/",
        "https://storage.googleapis.com/bergamot-models-sandbox/",
    ]
    .iter()
    .any(|p| url.starts_with(p))
        && !url.contains("..")
}

pub fn model_file(local: &Path, url: &str, sha256: Option<&str>) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    if !model_url_ok(url) {
        return Err("address not allowed".into());
    }
    let d = local.join("translations").join("models");
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let name: String = Sha256::digest(url.as_bytes()).iter().take(16).map(|b| format!("{b:02x}")).collect();
    let f = d.join(name);
    let ok1 = |b: &[u8]| {
        sha256
            .map(|h| {
                Sha256::digest(b)
                    .iter()
                    .map(|x| format!("{x:02x}"))
                    .collect::<String>()
                    .eq_ignore_ascii_case(h)
            })
            .unwrap_or(true)
    };

    let ok = |raw: &[u8], plain: &[u8]| ok1(plain) || ok1(raw);

    let fresh = std::fs::metadata(&f)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|e| sha256.is_some() || e.as_secs() < 7 * 86400)
        .unwrap_or(false);
    if fresh {
        if let Ok(b) = std::fs::read(&f) {
            let plain = gunzip_if_packed(&b);
            if ok(&b, &plain) && (sha256.is_some() || looks_like_json(&plain)) {
                return Ok(plain);
            }
        }
    }
    let part = f.with_extension("part");
    let o = curl()
        .args(["-sS", "-L", "--compressed", "--max-time", "600", "-o"])
        .arg(&part)
        .args(["-w", "%{http_code}", url])
        .output()
        .map_err(|e| e.to_string())?;
    let code: u16 = String::from_utf8_lossy(&o.stdout).trim().parse().unwrap_or(0);
    if code != 200 {
        let _ = std::fs::remove_file(&part);
        let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
        return Err(if code == 0 {
            if e.is_empty() {
                "no answer".into()
            } else {
                e
            }
        } else {
            format!("HTTP {code}")
        });
    }
    let raw = std::fs::read(&part).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&part);
    let b = gunzip_if_packed(&raw);
    if !ok(&raw, &b) {
        return Err("checksum".into());
    }

    if sha256.is_none() && !looks_like_json(&b) {
        let head: String = String::from_utf8_lossy(&b[..b.len().min(60)])
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        crate::focus::app_event(&format!(
            "translate: the list of language files is not readable ({} bytes, begins with „{head}“)",
            b.len()
        ));
        return Err("registry".into());
    }
    std::fs::write(&f, &b).map_err(|e| e.to_string())?;
    Ok(b)
}

fn gunzip_if_packed(b: &[u8]) -> Vec<u8> {
    use std::io::Read;
    if b.len() > 2 && b[0] == 0x1f && b[1] == 0x8b {
        let mut out = Vec::new();
        if flate2::read::MultiGzDecoder::new(b).read_to_end(&mut out).is_ok() {
            return out;
        }
    }
    b.to_vec()
}

fn looks_like_json(b: &[u8]) -> bool {
    matches!(b.iter().find(|c| !c.is_ascii_whitespace()), Some(b'{') | Some(b'['))
}

fn cache_dir(local: &Path) -> PathBuf {
    local.join("translations")
}

pub fn cache_key(doc: &Path, to: &str, way: &str) -> String {
    use sha2::{Digest, Sha256};
    let m = std::fs::metadata(doc).ok();
    let size = m.as_ref().map(|m| m.len()).unwrap_or(0);
    let time = m
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut h = Sha256::new();
    h.update(format!("{}|{size}|{time}|{to}|{way}", doc.to_string_lossy()).as_bytes());
    let x = h.finalize();
    x.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

pub fn cache_get(local: &Path, key: &str) -> Option<Value> {
    std::fs::read_to_string(cache_dir(local).join(format!("{key}.json")))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

pub fn cache_put(local: &Path, key: &str, v: &Value) -> Result<(), String> {
    let d = cache_dir(local);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    std::fs::write(d.join(format!("{key}.json")), serde_json::to_vec(v).unwrap_or_default()).map_err(|e| e.to_string())
}

pub const PAGE_MARK: &str = "\n⟦⟧\n";

pub fn join_pages(pages: &[String]) -> String {
    pages.join(PAGE_MARK)
}

pub fn split_pages(text: &str, n: usize) -> Vec<String> {
    let v: Vec<String> = text.split("⟦⟧").map(|p| p.trim_matches('\n').to_string()).collect();
    if v.len() == n {
        v
    } else {
        vec![text.replace("⟦⟧", "").trim().to_string()]
    }
}

pub fn result(pages: Vec<String>, way: &str, to: &str, extra: Value) -> Value {
    json!({ "pages": pages, "way": way, "to": to, "at": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(), "extra": extra })
}

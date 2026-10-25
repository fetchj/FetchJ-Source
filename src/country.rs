use once_cell::sync::Lazy;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

pub const EMBEDDED_CR: &str = include_str!("../data/countries/CR.json");
pub const INDEX: &str = include_str!("../data/countries/index.json");

pub const PUBLIC_BASE: &str = "https://raw.githubusercontent.com/fetchj/FetchJ-Public/main/countries";

pub const UPDATE_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDhENzg3NDM1QUExNUU4ODYKUldTRzZCV3FOWFI0amRyVkNuYnFmcmRkUExVN05WRk9RNjhlM1VORzVBSys3YVFMcVNMVTdpTDgK";

/* removed in the public source: validation / licence check · eliminado en el código público: validación / licencia */

pub static CATALOG: Lazy<Value> = Lazy::new(|| serde_json::from_str(INDEX).unwrap_or(Value::Null));

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub country: String,
    pub version: String,
    pub status: String,

    pub source: String,

    pub meta: BTreeMap<String, String>,

    pub values: BTreeMap<String, String>,

    pub own: BTreeMap<String, String>,

    pub problems: Vec<(String, String)>,
}

static ACTIVE: Lazy<RwLock<Module>> = Lazy::new(|| RwLock::new(parse(EMBEDDED_CR, "program").unwrap_or_default()));

fn parse(text: &str, source: &str) -> Option<Module> {
    let v: Value = serde_json::from_str(text).ok()?;
    let map = |k: &str| -> BTreeMap<String, String> {
        v.get(k)
            .and_then(|m| m.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(a, b)| b.as_str().map(|s| (a.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default()
    };
    Some(Module {
        country: v.get("country")?.as_str()?.trim().to_uppercase(),
        version: v.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        status: v.get("status").and_then(|x| x.as_str()).unwrap_or("scheduled").to_string(),
        source: source.to_string(),
        meta: map("meta"),
        values: map("values"),
        own: BTreeMap::new(),
        problems: Vec::new(),
    })
}

pub fn embedded(cc: &str) -> Option<Module> {
    match cc {
        "CR" => parse(EMBEDDED_CR, "program"),
        _ => None,
    }
}

pub fn catalog_status(cc: &str) -> String {
    CATALOG
        .get("countries")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|c| c.get("iso2").and_then(|x| x.as_str()) == Some(cc)))
        .and_then(|c| c.get("status").and_then(|s| s.as_str()))
        .unwrap_or("")
        .to_string()
}

pub fn selectable(cc: &str) -> bool {
    catalog_status(cc) == "available" && embedded(cc).is_some()
}

pub fn country_language(cc: &str) -> String {
    CATALOG
        .get("countries")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|c| c.get("iso2").and_then(|x| x.as_str()) == Some(cc)))
        .and_then(|c| c.get("lang").and_then(|s| s.as_str()))
        .unwrap_or("es")
        .to_string()
}

pub fn dir(app_data: &Path, cc: &str) -> PathBuf {
    app_data.join("countries").join(cc)
}

fn read_own(app_data: &Path, cc: &str) -> BTreeMap<String, String> {
    std::fs::read_to_string(dir(app_data, cc).join("module_own.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("values").and_then(|m| m.as_object()).cloned())
        .map(|m| m.into_iter().filter_map(|(a, b)| b.as_str().map(|s| (a, s.to_string()))).collect())
        .unwrap_or_default()
}

pub fn load(app_data: &Path, cc: &str) -> Module {
    let base = embedded(cc);
    let d = dir(app_data, cc);
    let dl = std::fs::read(d.join("module.json")).ok().and_then(|b| {
        let sig = std::fs::read_to_string(d.join("module.json.sig")).ok()?;
        if !verify(&b, &sig) {
            return None;
        }
        let m = parse(&String::from_utf8_lossy(&b), "download")?;
        (m.country == cc).then_some(m)
    });
    let mut m = match (base.clone(), dl) {
        (Some(b), Some(d)) if d.version >= b.version => {
            let mut x = d;
            for (k, v) in b.values {
                x.values.entry(k).or_insert(v);
            }
            for (k, v) in b.meta {
                x.meta.entry(k).or_insert(v);
            }
            x
        }
        (Some(b), _) => b,
        (None, Some(d)) => d,
        (None, None) => Module {
            country: cc.to_string(),
            status: catalog_status(cc),
            source: "none".into(),
            ..Default::default()
        },
    };
    m.own = read_own(app_data, cc);
    check(&mut m, base.as_ref());
    m
}

pub fn plausible(id: &str, v: &str) -> Result<(), &'static str> {
    let v = v.trim();
    let https = |s: &str| -> bool {
        let h = s.strip_prefix("https://").unwrap_or("");
        let host = h.split(['/', '?', '#']).next().unwrap_or("");
        !host.is_empty() && host.contains('.') && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':')
    };
    let hostname = |s: &str| !s.is_empty() && s.contains('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if v.contains(['\n', '\r']) {
        return Err("line");
    }
    match id {
        "portal.portal_url" | "portal.api_base_url" | "portal.download_api_url" | "nexus.nexus_url" | "country.player_url" => {
            if https(v) {
                Ok(())
            } else {
                Err("https")
            }
        }
        "nexus.nexus_url_fallback" => {
            if v.is_empty() || https(v) {
                Ok(())
            } else {
                Err("https")
            }
        }
        "portal.login_hosts" => {
            if v.split(',').map(|h| h.trim()).all(hostname) {
                Ok(())
            } else {
                Err("host")
            }
        }
        "country.court_domain" | "country.search_domain" => {
            if hostname(v) {
                Ok(())
            } else {
                Err("host")
            }
        }
        "portal.portal_timezone" => {
            let b = v.as_bytes();
            if b.len() == 6
                && (b[0] == b'+' || b[0] == b'-')
                && b[3] == b':'
                && v[1..3].parse::<u8>().map(|h| h <= 14).unwrap_or(false)
                && v[4..].parse::<u8>().map(|m| m < 60).unwrap_or(false)
            {
                Ok(())
            } else {
                Err("tz")
            }
        }
        "country.session_max_min" => match v.parse::<u32>() {
            Ok(n) if (1..=600).contains(&n) => Ok(()),
            _ => Err("number"),
        },
        "calendar.cal_holidays" => {
            if v.split([',', ';'])
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .all(|s| crate::calendar::rule_day(s, 2026).is_some())
            {
                Ok(())
            } else {
                Err("day")
            }
        }
        "calendar.cal_closed" => {
            let ok = v.split([',', ';']).map(|s| s.trim()).filter(|s| !s.is_empty()).all(|s| {
                s.split_once("..")
                    .map(|(a, b)| crate::calendar::rule_day(a, 2026).is_some() && crate::calendar::rule_day(b, 2026).is_some())
                    .unwrap_or(false)
            });
            if ok {
                Ok(())
            } else {
                Err("range")
            }
        }

        _ => {
            if v.is_empty() {
                Err("empty")
            } else {
                Ok(())
            }
        }
    }
}

fn check(m: &mut Module, base: Option<&Module>) {
    let mut problems = Vec::new();
    let fallback = |id: &str| -> Option<String> {
        let b = base?;
        match id.strip_prefix("country.") {
            Some(k) => b.meta.get(k).cloned(),
            None => b.values.get(id).cloned(),
        }
    };
    let ids: Vec<String> = m.values.keys().cloned().collect();
    for id in ids {
        let v = m.values[&id].clone();
        if let Err(why) = plausible(&id, &v) {
            problems.push((id.clone(), why.to_string()));
            match fallback(&id).filter(|f| plausible(&id, f).is_ok()) {
                Some(f) => {
                    m.values.insert(id, f);
                }
                None => {
                    m.values.remove(&id);
                }
            }
        }
    }
    let keys: Vec<String> = m.meta.keys().cloned().collect();
    for k in keys {
        let id = format!("country.{k}");
        if let Err(why) = plausible(&id, &m.meta[&k]) {
            problems.push((id.clone(), why.to_string()));
            match fallback(&id).filter(|f| plausible(&id, f).is_ok()) {
                Some(f) => {
                    m.meta.insert(k, f);
                }
                None => {
                    m.meta.remove(&k);
                }
            }
        }
    }
    let own: Vec<String> = m.own.keys().cloned().collect();
    for id in own {
        if let Err(why) = plausible(&id, &m.own[&id]) {
            problems.push((id.clone(), why.to_string()));
            m.own.remove(&id);
        }
    }
    m.problems = problems;
}

pub fn activate(m: Module) {
    *ACTIVE.write().unwrap() = m;
}

pub fn active() -> Module {
    ACTIVE.read().unwrap().clone()
}

pub fn value(id: &str) -> Option<String> {
    let a = ACTIVE.read().unwrap();
    a.own.get(id).or_else(|| a.values.get(id)).cloned()
}

pub fn is_module_key(id: &str) -> bool {
    let a = ACTIVE.read().unwrap();
    a.values.contains_key(id) || embedded("CR").map(|b| b.values.contains_key(id)).unwrap_or(false)
}

pub fn meta(key: &str) -> String {
    let a = ACTIVE.read().unwrap();
    a.own
        .get(&format!("country.{key}"))
        .or_else(|| a.meta.get(key))
        .cloned()
        .or_else(|| embedded("CR").and_then(|b| b.meta.get(key).cloned()))
        .unwrap_or_default()
}

pub fn subst(text: &str, lang: &str) -> String {
    let cc = ACTIVE.read().unwrap().country.clone();
    if cc.is_empty() || cc == "CR" {
        return text.to_string();
    }
    let country_name = CATALOG
        .get("countries")
        .and_then(|a| a.as_array())
        .and_then(|a| a.iter().find(|c| c.get("iso2").and_then(|x| x.as_str()) == Some(cc.as_str())))
        .and_then(|c| {
            c["name"]
                .get(lang)
                .or_else(|| c["name"].get("es"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| cc.clone());
    let mut s = text.to_string();
    for (a, k) in [
        ("Nexus.PJ", "search_name"),
        ("Gestión en Línea", "portal_name"),
        ("Poder Judicial", "court_name"),
        ("Nexus", "search_short"),
    ] {
        let b = meta(k);
        if !b.is_empty() && b != a {
            s = s.replace(a, &b);
        }
    }
    if country_name != "Costa Rica" {
        s = s.replace("Costa Rica", &country_name);
    }
    s
}

/* removed in the public source: validation / licence check · eliminado en el código público: validación / licencia */

pub fn verify_with(pubkey_b64: &str, data: &[u8], sig_b64: &str) -> bool {
    use base64::Engine;
    let dec = |s: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
    };
    let (Some(pk), Some(sig)) = (dec(pubkey_b64), dec(sig_b64)) else {
        return false;
    };
    let (Ok(pk), Ok(sig)) = (minisign_verify::PublicKey::decode(&pk), minisign_verify::Signature::decode(&sig)) else {
        return false;
    };
    pk.verify(data, &sig, true).is_ok()
}

pub fn verify(data: &[u8], sig_b64: &str) -> bool {
    verify_with(UPDATE_PUBKEY, data, sig_b64)
}

pub fn save_own(app_data: &Path, cc: &str, changes: &BTreeMap<String, String>) -> std::io::Result<Vec<String>> {
    let base = load(app_data, cc);
    let mut own = read_own(app_data, cc);
    let mut refused = Vec::new();
    for (id, v) in changes {
        let module_v = match id.strip_prefix("country.") {
            Some(k) => base.meta.get(k).cloned(),
            None => base.values.get(id).cloned(),
        };
        if v.trim().is_empty() || Some(v.trim().to_string()) == module_v {
            own.remove(id);
        } else if plausible(id, v).is_ok() {
            own.insert(id.clone(), v.trim().to_string());
        } else {
            refused.push(id.clone());
        }
    }
    let d = dir(app_data, cc);
    std::fs::create_dir_all(&d)?;
    let m: Map<String, Value> = own.into_iter().map(|(k, v)| (k, Value::from(v))).collect();
    crate::pyfmt::write_atomic(
        &d.join("module_own.json"),
        serde_json::to_string_pretty(&json!({"saved": crate::pyfmt::now_human(), "values": m}))
            .unwrap()
            .as_bytes(),
        ".part",
    )?;
    Ok(refused)
}

pub fn install(app_data: &Path, cc: &str, data: &[u8], sig_b64: &str) -> Result<Module, &'static str> {
    if !verify(data, sig_b64) {
        return Err("signature");
    }
    let m = parse(&String::from_utf8_lossy(data), "download").ok_or("format")?;
    if m.country != cc {
        return Err("country");
    }
    let d = dir(app_data, cc);
    std::fs::create_dir_all(&d).map_err(|_| "write")?;
    crate::pyfmt::write_atomic(&d.join("module.json"), data, ".part").map_err(|_| "write")?;
    crate::pyfmt::write_atomic(&d.join("module.json.sig"), sig_b64.trim().as_bytes(), ".part").map_err(|_| "write")?;
    Ok(load(app_data, cc))
}

pub fn public_url(cc: &str) -> String {
    format!("{PUBLIC_BASE}/{cc}.json")
}

pub fn fetch(url: &str, dest: &Path) -> Result<(), String> {
    if !url.starts_with(PUBLIC_BASE) {
        return Err("address".into());
    }
    let _ = std::fs::remove_file(dest);
    let mut c = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    c.args(["-L", "--fail", "-sS", "--max-time", "60", "-o"]).arg(dest).arg(url);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    match c.output() {
        Ok(o) if o.status.success() && dest.is_file() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn page_info(app_data: &Path, cc: &str) -> Value {
    let m = load(app_data, cc);
    let dl = dir(app_data, cc).join("module.json");
    let downloaded = std::fs::metadata(&dl)
        .ok()
        .and_then(|x| x.modified().ok())
        .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string());
    let countries: Vec<Value> = CATALOG
        .get("countries")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut c| {
            let iso = c.get("iso2").and_then(|x| x.as_str()).unwrap_or("").to_string();
            c["selectable"] = json!(selectable(&iso));
            c
        })
        .collect();
    json!({
        "country": cc,
        "countries": countries,
        "status": m.status,
        "version": m.version,
        "source": m.source,
        "downloaded": downloaded,
        "meta": m.meta,
        "values": m.values,
        "own": m.own,
        "problems": m.problems.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
        "url": public_url(cc),
    })
}

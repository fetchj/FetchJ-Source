use crate::config::Conf;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const ORIGIN: &str = "https://nexuspj.poder-judicial.go.cr";

static CUR: Mutex<String> = Mutex::new(String::new());

fn origin() -> String {
    let c = CUR.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if c.is_empty() {
        crate::country::value("nexus.nexus_url")
            .and_then(|u| clean_origin(&u))
            .unwrap_or_else(|| ORIGIN.to_string())
    } else {
        c
    }
}

fn set_origin(o: &str) {
    *CUR.lock().unwrap_or_else(|p| p.into_inner()) = o.to_string();
}

fn host_ok(h: &str) -> bool {
    let h = h.to_ascii_lowercase();
    let d = crate::country::meta("search_domain").to_ascii_lowercase();
    let d = if d.is_empty() { "poder-judicial.go.cr".to_string() } else { d };
    h == d || h.ends_with(&format!(".{d}"))
}

pub const MAX_Q: usize = 900;

pub fn q_len(url: &str) -> usize {
    url.split_once('?').map(|(_, q)| q.len()).unwrap_or(0)
}

pub fn clean_origin(s: &str) -> Option<String> {
    let mut t = s.trim().trim_matches(|c| c == '"' || c == '\'').replace('\\', "/");
    if t.is_empty() {
        return None;
    }
    if !t.contains("://") {
        t = format!("https://{t}");
    }
    let u: tauri::Url = t.parse().ok()?;
    if u.scheme() != "https" {
        return None;
    }
    let h = u.host_str()?;
    host_ok(h).then(|| format!("https://{}", h.to_ascii_lowercase()))
}

fn rebase(url: &str, to: &str) -> String {
    match url.parse::<tauri::Url>() {
        Ok(u) if u.host_str().map(host_ok).unwrap_or(false) => {
            format!("{to}{}{}", u.path(), u.query().map(|q| format!("?{q}")).unwrap_or_default())
        }
        _ => url.to_string(),
    }
}
const LABEL: &str = "nexus";

static PENDING: Mutex<Option<(PathBuf, mpsc::Sender<Result<PathBuf, String>>)>> = Mutex::new(None);

static CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn cancelled() -> bool {
    CANCEL.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn cancel() {
    CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some((_, tx)) = PENDING.lock().unwrap_or_else(|p| p.into_inner()).take() {
        let _ = tx.send(Err("cancelled".into()));
    }
}

fn js_lit(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

pub fn url_ok(u: &str) -> bool {
    match u.parse::<tauri::Url>() {
        Ok(x) => {
            x.scheme() == "https"
                && x.host_str().map(host_ok).unwrap_or(false)
                && (x.path() == "/search" || x.path().starts_with("/document/"))
        }
        Err(_) => false,
    }
}

fn nav_ok(s: &str) -> bool {
    s.starts_with("about:")
        || s.starts_with("blob:")
        || s.parse::<tauri::Url>()
            .map(|u| u.scheme() == "https" && u.host_str().map(host_ok).unwrap_or(false))
            .unwrap_or(false)
}

fn window<R: Runtime>(app: &AppHandle<R>, conf: &Conf, url: &str) -> Result<WebviewWindow<R>, String> {
    let parsed: tauri::Url = url.parse().map_err(|_| "bad url")?;
    if let Some(w) = app.get_webview_window(LABEL) {
        w.navigate(parsed).map_err(|e| e.to_string())?;
        return Ok(w);
    }
    let app2 = app.clone();
    let mut b = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(parsed))
        .title(crate::wintitle::title("research", &crate::wintitle::addr(url)))
        .inner_size(1100.0, 800.0)
        .min_inner_size(900.0, 650.0)
        .visible(false)
        .focused(false)
        .on_navigation(|u| nav_ok(u.as_str()))
        .on_download(move |_w, ev| match ev {
            tauri::webview::DownloadEvent::Requested { destination, .. } => {
                let p = PENDING.lock().unwrap_or_else(|p| p.into_inner());
                match p.as_ref() {
                    Some((path, _)) => {
                        *destination = path.clone();
                        true
                    }

                    None => true,
                }
            }
            tauri::webview::DownloadEvent::Finished { path, success, .. } => {
                if let Some((want, tx)) = PENDING.lock().unwrap_or_else(|p| p.into_inner()).take() {
                    let _ = tx.send(if success {
                        Ok(path.unwrap_or(want))
                    } else {
                        Err("download failed".into())
                    });
                }
                let _ = app2.emit("fj-nexus-dl", json!({ "ok": success }));
                true
            }
            _ => true,
        });
    b = b.data_directory(conf.state_dir.join("nexus_profile"));
    #[cfg(windows)]
    {
        b = b.additional_browser_args("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-background-timer-throttling --disable-backgrounding-occluded-windows --disable-renderer-backgrounding");
    }
    let w = b.build().map_err(|e| e.to_string())?;
    crate::wintitle::follow_url(app, LABEL, "research");

    w.on_window_event(|e| {
        if matches!(e, tauri::WindowEvent::Destroyed) {
            if let Some((_, tx)) = PENDING.lock().unwrap_or_else(|p| p.into_inner()).take() {
                let _ = tx.send(Err("closed".into()));
            }
        }
    });
    Ok(w)
}

fn export_pdf<R: Runtime>(w: &WebviewWindow<R>, path: &std::path::Path, timeout_s: u64) -> Result<PathBuf, String> {
    let start = r#"(function(){var b=document.querySelector('[ng-click="downloadDocument(\'pdf\')"]');var s=angular.element(b).scope();var api=angular.element(document.body).injector().get('searchAPI');window.__fjx={st:'run'};var p={docs:s.getHeaders(),download:true,export_type:'pdf',headers_only:false};if(localStorage.getItem('token'))p.authToken='Bearer '+localStorage.getItem('token');api.exportdocument(p).$promise.then(function(d){var r=d&&d.response;if(!r||!r.size){window.__fjx={st:'err',e:'empty answer'};return;}var fr=new FileReader();fr.onload=function(){var u=String(fr.result);window.__fjx={st:'ok',b64:u.slice(u.indexOf(',')+1)};};fr.onerror=function(){window.__fjx={st:'err',e:'read'};};fr.readAsDataURL(r);},function(e){window.__fjx={st:'err',e:'HTTP '+(e&&e.status),code:e&&e.status};});return true;})()"#;
    eval(w, start, Duration::from_secs(8))?;
    let t = Instant::now();
    let n = loop {
        std::thread::sleep(Duration::from_millis(250));
        if cancelled() {
            return Err("cancelled".into());
        }
        if gone(w) {
            return Err("closed".into());
        }
        let v = eval(
            w,
            "(function(){var x=window.__fjx||{};return {st:x.st||'',e:x.e||'',code:x.code||0,n:x.b64?x.b64.length:0};})()",
            Duration::from_secs(8),
        )
        .unwrap_or(Value::Null);
        match v["st"].as_str().unwrap_or("") {
            "ok" => break v["n"].as_u64().unwrap_or(0) as usize,
            "err" => {
                return Err(if v["code"].as_u64() == Some(405) {
                    "maxdownloads".into()
                } else {
                    v["e"].as_str().unwrap_or("error").to_string()
                });
            }
            _ => {}
        }
        if t.elapsed() > Duration::from_secs(timeout_s) {
            return Err("timeout".into());
        }
    };

    let mut b64 = String::with_capacity(n);
    let step = 2 * 1024 * 1024;
    let mut off = 0;
    while off < n {
        let v = eval(w, &format!("(window.__fjx.b64||'').substr({off},{step})"), Duration::from_secs(20))?;
        let part = v.as_str().unwrap_or("");
        if part.is_empty() {
            return Err("read".into());
        }
        b64.push_str(part);
        off += part.len();
    }
    let _ = eval(w, "(function(){window.__fjx=null;return true})()", Duration::from_secs(5));
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| e.to_string())?;
    if bytes.len() < 500 || !bytes.starts_with(b"%PDF") {
        return Err("not a PDF".into());
    }
    std::fs::write(path, &bytes).map_err(|e| e.to_string())?;
    Ok(path.to_path_buf())
}

fn origin_or(conf: &Conf) -> String {
    if CUR.lock().unwrap_or_else(|p| p.into_inner()).is_empty() {
        conf.nexus_url.clone()
    } else {
        origin()
    }
}

fn gone<R: Runtime>(w: &WebviewWindow<R>) -> bool {
    w.is_visible().is_err()
}

fn eval<R: Runtime>(w: &WebviewWindow<R>, expr: &str, timeout: Duration) -> Result<Value, String> {
    let here = w.url().map(|u| u.to_string()).unwrap_or_default();
    if !here.starts_with(&origin()) {
        return Err("notready".into());
    }
    let js = format!("(function(){{try{{if(location.origin!=={o})return JSON.stringify({{nofj:1}});return JSON.stringify({expr});}}catch(e){{return JSON.stringify({{err:String(e&&e.message||e)}})}}}})()", o = js_lit(&origin()));
    let (tx, rx) = mpsc::channel::<String>();
    w.eval_with_callback(&js, move |s: String| {
        let _ = tx.send(s);
    })
    .map_err(|e| e.to_string())?;
    let s = rx.recv_timeout(timeout).map_err(|_| "timeout".to_string())?;
    let v: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
    Ok(match v {
        Value::String(inner) => serde_json::from_str(&inner).unwrap_or(Value::String(inner)),
        x => x,
    })
}

fn snapshot_js(words: &[String]) -> String {
    let kw = serde_json::to_string(words).unwrap_or_else(|_| "[]".into());
    format!(
        r#"(function(){{
var fold=function(s){{return String(s||'').normalize('NFD').replace(/[̀-ͯ]/g,'').toLowerCase();}};
var body=document.body?document.body.innerText:'';
if(/captcha/i.test(body)&&!document.querySelector('.app-results'))return {{captcha:true}};
if(/The specified URL cannot be found/i.test(body))return {{blocked:true}};
var tot=document.querySelector('[ng-bind="resultsTotal"]');
var items=[].slice.call(document.querySelectorAll('.app-results .app-item[ng-repeat]'));
var loading=!!document.querySelector('.app-results [ng-show="onLoading"]:not(.ng-hide)');
var none=/No se encontraron|0 resultados/i.test(body)&&!items.length;
if(!window.angular)return {{ready:false,ng:false,res:!!document.querySelector('.app-results'),title:document.title}};
var kws={kw}.map(function(w){{return fold(w).replace(/["*]/g,'').trim();}}).filter(Boolean);
var out=items.map(function(it){{var r=(angular.element(it).scope()||{{}}).result||{{}};
 var text=fold((r.content||'')+' '+(r.descriptores||'')+' '+(r.restrictores||''));
 var hl=r.highlight&&r.highlight.contenido?String(r.highlight.contenido[0]||''):'';
 var snip=(hl||String(r.content||'').slice(0,320)).replace(/<[^>]+>/g,'');
 return {{id:r.idDocument||'',parent:r.numeroDocumentoPadre||'',type:r.tipoDocumento||'',despacho:r.despacho||'',nr:r.numeroDocumento||'',anno:r.anno||'',date:r.date||'',
   title:r.title||'',descriptor:r.descriptores||'',restrictor:r.restrictores||'',snippet:snip.slice(0,420),
   kw:kws.map(function(k){{var n=0,i=0;while(k&&(i=text.indexOf(k,i))>=0){{n++;i+=k.length;}}return n;}})}};}});
var pages=[].slice.call(document.querySelectorAll('.pagination li a')).map(function(a){{return a.textContent.trim();}});
var act=document.querySelector('.pagination li.active a');
var so=document.getElementById('sort-order');
var sorted=(body.match(/ordenados por\s+([^\n]*)/i)||['',''])[1].trim();
return {{ready:!!tot&&!loading&&(items.length>0||none||tot.textContent.trim()==='0'),ng:true,sorted:sorted,asc:!!so&&/Ascend/i.test(so.title||''),res:!!document.querySelector('.app-results'),hastot:!!tot,loading:loading,title:document.title,total:tot?tot.textContent.trim():'',page:act?act.textContent.trim():'1',pages:pages,items:out,none:none}};
}})()"#
    )
}

fn wait_snapshot<R: Runtime>(w: &WebviewWindow<R>, words: &[String], not_first: Option<&str>, limit: Duration) -> Result<Value, String> {
    let t0 = Instant::now();
    let js = snapshot_js(words);

    let mut last: String;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        if cancelled() {
            return Err("cancelled".into());
        }
        if gone(w) {
            crate::focus::app_event("research: the Nexus window was closed – the search is tried again in a new window");
            return Err("closed".into());
        }
        let r = eval(w, &js, Duration::from_secs(8));
        last = match &r {
            Ok(v) => format!(
                "angular={} results-box={} total-field={} total='{}' items={} loading={} none={} title='{}'",
                v["ng"].as_bool().unwrap_or(false),
                v["res"].as_bool().unwrap_or(false),
                v["hastot"].as_bool().unwrap_or(false),
                v["total"].as_str().unwrap_or(""),
                v["items"].as_array().map(|a| a.len()).unwrap_or(0),
                v["loading"].as_bool().unwrap_or(false),
                v["none"].as_bool().unwrap_or(false),
                v["title"].as_str().unwrap_or("").chars().take(60).collect::<String>()
            ),
            Err(e) => format!("page not readable ({e})"),
        };
        if let Ok(v) = r {
            if v["captcha"].as_bool().unwrap_or(false) {
                let _ = w.show();
                let _ = w.set_focus();
                return Err("captcha".into());
            }

            if v["blocked"].as_bool().unwrap_or(false) {
                crate::focus::app_event("research: Nexus answers \"The specified URL cannot be found\" – search address too long for the firewall, no further tries");
                return Err("blocked".into());
            }
            if v["ready"].as_bool().unwrap_or(false) {
                let first = v["items"][0]["id"].as_str().unwrap_or("");
                if not_first.map(|f| f != first || first.is_empty()).unwrap_or(true) {
                    return Ok(v);
                }
            }
        }
        if t0.elapsed() > limit {
            let at = w
                .url()
                .map(|u| format!("{}{}", u.path(), if u.query().is_some() { "?…" } else { "" }))
                .unwrap_or_default();
            crate::focus::app_event(&format!(
                "research: Nexus page not ready after {} s – {last} – page {at}",
                limit.as_secs()
            ));
            return Err("timeout".into());
        }
    }
}

fn total_num(snap: &Value) -> usize {
    snap["total"]
        .as_str()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

fn sort_top<R: Runtime>(w: &WebviewWindow<R>, words: &[String], snap: Value) -> Value {
    let click_desp = "(function(){var a=[].slice.call(document.querySelectorAll('a[ng-click=\"setSort(sort)\"]')).filter(function(a){return (a.title||a.textContent).trim()==='Despacho';})[0];if(!a)return false;a.click();return true;})()";
    let js = snapshot_js(words);

    let wait = |want_asc: bool, before: String| -> Option<Value> {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(40) && !cancelled() {
            std::thread::sleep(Duration::from_millis(700));
            if let Ok(v) = eval(w, &js, Duration::from_secs(8)) {
                let first = v["items"][0]["id"].as_str().unwrap_or("").to_string();
                if v["ready"].as_bool().unwrap_or(false)
                    && v["sorted"].as_str().unwrap_or("").starts_with("Despacho")
                    && (!want_asc || v["asc"].as_bool().unwrap_or(false))
                    && (first != before || t0.elapsed() > Duration::from_secs(15))
                {
                    return Some(v);
                }
            }
        }
        None
    };

    let now = || {
        eval(w, &js, Duration::from_secs(8))
            .ok()
            .filter(|v| v["ready"].as_bool().unwrap_or(false))
    };
    let first_of = |v: &Value| v["items"][0]["id"].as_str().unwrap_or("").to_string();
    if !eval(w, click_desp, Duration::from_secs(8))
        .map(|v| v.as_bool().unwrap_or(false))
        .unwrap_or(false)
    {
        crate::focus::app_event(
            "research: \"highest court first\" – sort link \"Despacho\" not found on the Nexus page, hits as Nexus sorts them",
        );
        return snap;
    }
    let Some(v) = wait(false, first_of(&snap)) else {
        return now().unwrap_or(snap);
    };
    if v["asc"].as_bool().unwrap_or(false) {
        return v;
    }
    let before = first_of(&v);
    let _ = eval(
        w,
        "(function(){var s=document.getElementById('sort-order');if(!s)return false;s.click();return true;})()",
        Duration::from_secs(8),
    );
    match wait(true, before) {
        Some(v) => {
            crate::focus::app_event("research: hits sorted by court, highest court first");
            v
        }
        None => now().unwrap_or(v),
    }
}

pub fn search<R: Runtime>(
    app: &AppHandle<R>,
    conf: &Conf,
    url: &str,
    words: &[String],
    want: usize,
    top: bool,
    progress: &dyn Fn(usize, usize),
) -> Result<Value, String> {
    if !url_ok(url) {
        return Err("refused".into());
    }

    if q_len(url) > MAX_Q {
        return Err("toolong".into());
    }
    let want = want.clamp(5, 50);
    let t_start = Instant::now();
    CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);

    let mut tries: Vec<String> = vec![conf.nexus_url.clone(), conf.nexus_url.clone()];
    if !conf.nexus_url_fallback.is_empty() && conf.nexus_url_fallback != conf.nexus_url {
        tries.push(conf.nexus_url_fallback.clone());
    }
    let mut w: Option<WebviewWindow<R>> = None;
    let mut snap = Value::Null;
    for (k, o) in tries.iter().enumerate() {
        if k > 0 {
            if let Some(old) = w.take() {
                let shown = old.is_visible().unwrap_or(false);
                let _ = old.destroy();

                let t = Instant::now();
                while app.get_webview_window(LABEL).is_some() && t.elapsed() < Duration::from_secs(5) {
                    std::thread::sleep(Duration::from_millis(200));
                }
                if o != &tries[k - 1] {
                    crate::focus::app_event(&format!("research: Nexus search – now on the second address of the settings ({o})"));
                } else {
                    crate::focus::app_event("research: Nexus search – the Nexus window is opened again and the search tried once more");
                }
                set_origin(o);
                let nw = window(app, conf, &rebase(url, o))?;
                if shown {
                    let _ = nw.show();
                }
                w = Some(nw);
            }
        } else {
            set_origin(o);
            w = Some(window(app, conf, &rebase(url, o))?);
        }
        match wait_snapshot(w.as_ref().unwrap(), words, None, Duration::from_secs(conf.nexus_timeout)) {
            Ok(v) => {
                snap = v;
                break;
            }
            Err(e) if (e == "timeout" || e == "closed") && !cancelled() && k + 1 < tries.len() => continue,
            Err(e) => {
                set_origin(&conf.nexus_url);
                return Err(e);
            }
        }
    }
    let w = w.ok_or("timeout")?;
    let n_total = total_num(&snap);
    let n_page = snap["items"].as_array().map(|a| a.len()).unwrap_or(0);
    crate::focus::app_event(&format!(
        "research: Nexus answered after {:.1} s – {} hit(s) in all, {} on the first page",
        t_start.elapsed().as_secs_f64(),
        n_total,
        n_page
    ));

    if top && n_total > n_page.max(1) {
        let t = Instant::now();
        snap = sort_top(&w, words, snap);
        crate::focus::app_event(&format!("research: sorting by court took {:.1} s", t.elapsed().as_secs_f64()));
    }
    let total = snap["total"].clone();
    let mut hits: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut page = 1usize;
    loop {
        for it in snap["items"].as_array().cloned().unwrap_or_default() {
            let key = match it["type"].as_str() {
                Some("EXT") if !it["parent"].as_str().unwrap_or("").is_empty() => it["parent"].as_str().unwrap().to_string(),
                _ => it["id"].as_str().unwrap_or("").to_string(),
            };
            if key.is_empty() {
                continue;
            }
            if seen.insert(key.clone()) {
                let mut h = it.clone();
                h["doc"] = json!(key);
                hits.push(h);
            } else if let Some(h) = hits.iter_mut().find(|h| h["doc"].as_str() == Some(key.as_str())) {
                if let (Some(a), Some(b)) = (h["kw"].as_array().cloned(), it["kw"].as_array()) {
                    h["kw"] = json!(a
                        .iter()
                        .zip(b.iter())
                        .map(|(x, y)| x.as_u64().unwrap_or(0) + y.as_u64().unwrap_or(0))
                        .collect::<Vec<_>>());
                }
            }
            if hits.len() >= want {
                break;
            }
        }
        progress(hits.len(), want);
        if hits.len() >= want || page >= 12 {
            break;
        }
        let next = (page + 1).to_string();
        let has = snap["pages"]
            .as_array()
            .map(|p| p.iter().any(|x| x.as_str() == Some(next.as_str())))
            .unwrap_or(false);
        if !has {
            break;
        }
        let first = snap["items"][0]["id"].as_str().unwrap_or("").to_string();
        let click = format!("(function(){{var a=[].slice.call(document.querySelectorAll('.pagination li a')).filter(function(a){{return a.textContent.trim()==={}}})[0];if(!a)return false;a.click();return true;}})()", js_lit(&next));
        if !eval(&w, &click, Duration::from_secs(8))
            .map(|v| v.as_bool().unwrap_or(false))
            .unwrap_or(false)
        {
            break;
        }
        page += 1;
        snap = match wait_snapshot(&w, words, Some(&first), Duration::from_secs(40)) {
            Ok(s) => s,
            Err(_) => break,
        };
    }
    Ok(json!({ "total": total, "hits": hits, "pages": page }))
}

fn safe_part(s: &str) -> String {
    let f = crate::research::fold(s);
    let t: String = f.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    t.split('-')
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(60)
        .collect()
}

pub fn id_of(name: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?:^|_)([a-z]{2,6}-\d+-\d+-\d+)(?:_|\.pdf$)").unwrap());
    re.captures(name).map(|c| c[1].to_string()).unwrap_or_default()
}

fn existing(dir: &std::path::Path, id: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        !n.starts_with('_') && n.ends_with(".pdf") && id_of(&n) == id && std::fs::metadata(p).map(|m| m.len() > 500).unwrap_or(false)
    })
}

const MONTHS: [(&str, u32); 13] = [
    ("enero", 1),
    ("febrero", 2),
    ("marzo", 3),
    ("abril", 4),
    ("mayo", 5),
    ("junio", 6),
    ("julio", 7),
    ("agosto", 8),
    ("setiembre", 9),
    ("septiembre", 9),
    ("octubre", 10),
    ("noviembre", 11),
    ("diciembre", 12),
];

pub fn head_of(text: &str) -> Value {
    let t = text.replace('\u{a0}', " ");
    let cap = |re: &str| {
        regex::Regex::new(re).ok().and_then(|r| {
            r.captures(&t).map(|c| {
                (1..c.len())
                    .map(|i| c.get(i).map(|m| m.as_str().trim().to_string()).unwrap_or_default())
                    .collect::<Vec<_>>()
            })
        })
    };
    let mut v = json!({});
    if let Some(c) = cap(r"Resoluci[oó]n\s*N\s*[º°o.]?\s*(\d+)\s*-\s*(\d{4})") {
        v["nr"] = json!(c[0]);
        v["anno"] = json!(c[1]);
    }
    if let Some(c) = cap(r"(?i)Fecha de la Resoluci[oó]n:\s*(\d{1,2})\s+de\s+([a-zé]+)\s+del?\s+(\d{4})(?:\s+a\s+las\s+(\d{1,2}):(\d{2}))?")
    {
        let m = MONTHS.iter().find(|(n, _)| crate::research::fold(&c[1]) == *n).map(|(_, m)| *m);
        if let (Some(m), Ok(d), Ok(y)) = (m, c[0].parse::<u32>(), c[2].parse::<i32>()) {
            if chrono::NaiveDate::from_ymd_opt(y, m, d).is_some() {
                v["fecha"] = json!(format!("{y:04}-{m:02}-{d:02}"));
                if !c[3].is_empty() {
                    v["hora"] = json!(format!("{:02}:{}", c[3].parse::<u32>().unwrap_or(0), c[4]));
                }
            }
        }
    }
    if let Some(c) = cap(r"Expediente:\s*(\d{2}-\d{6}-\d{4}-[A-Z]{2})") {
        v["exp"] = json!(c[0]);
    }
    if let Some(c) = cap(r"Clase\s+de\s+asunto:\s*([^\n\r]+)") {
        v["clase"] = json!(c[0].chars().take(80).collect::<String>());
    }
    v
}

fn pdf_head(path: &std::path::Path) -> Value {
    let Ok(doc) = lopdf::Document::load(path) else { return json!({}) };
    head_of(&doc.extract_text(&[1]).unwrap_or_default())
}

fn nexus_date(d: &str) -> Option<String> {
    let p: Vec<&str> = d.trim().split('-').collect();
    if p.len() != 3 {
        return None;
    }
    let m = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "set", "oct", "nov", "dic"]
        .iter()
        .position(|x| crate::research::fold(p[1]) == *x || (crate::research::fold(p[1]) == "sep" && *x == "set"))? as u32
        + 1;
    let (d, y) = (p[0].parse::<u32>().ok()?, p[2].parse::<i32>().ok()?);
    chrono::NaiveDate::from_ymd_opt(y, m, d).map(|_| format!("{y:04}-{m:02}-{d:02}"))
}

pub fn court_short(despacho: &str) -> String {
    let f = crate::research::fold(despacho);
    for (k, v) in [
        ("sala primera", "Sala-I"),
        ("sala segunda", "Sala-II"),
        ("sala tercera", "Sala-III"),
        ("sala constitucional", "Sala-Const"),
        ("corte plena", "Corte-Plena"),
    ] {
        if f.starts_with(k) {
            return v.to_string();
        }
    }
    let f = f.replace("circuito judicial", "cj").replace("primer circuito", "i circuito");
    let ab = [
        ("tribunal", "Trib"),
        ("apelacion", "Apel"),
        ("sentencia", "Sent"),
        ("casacion", "Casacion"),
        ("juzgado", "Juzg"),
        ("contencioso", "Cont"),
        ("administrativo", "Adm"),
        ("familia", "Fam"),
        ("violencia", "Viol"),
        ("domestica", "Dom"),
        ("agrario", "Agrario"),
        ("laboral", "Laboral"),
        ("civil", "Civil"),
        ("penal", "Penal"),
        ("juvenil", "Juvenil"),
        ("cj", "CJ"),
        ("hacienda", "Hacienda"),
    ];
    let skip = ["de", "la", "del", "el", "los", "las", "y", "en", "e"];
    let words: Vec<String> = f
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty() && !skip.contains(w))
        .map(|w| {
            if let Some((_, v)) = ab.iter().find(|(k, _)| k == &w) {
                v.to_string()
            } else if w.chars().all(|c| "ivxl".contains(c)) {
                w.to_uppercase()
            } else {
                let mut c = w.chars();
                c.next()
                    .map(|x| x.to_uppercase().collect::<String>() + c.as_str())
                    .unwrap_or_default()
            }
        })
        .collect();
    let mut out = String::new();
    for w in words {
        if out.len() + w.len() + 1 > 50 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&w);
    }
    out
}

pub fn judgment_name(case: &str, id: &str, fecha: &str, hora: &str, exp: &str, nr: &str, anno: &str, despacho: &str) -> String {
    let mut p: Vec<String> = Vec::new();
    if !fecha.is_empty() {
        p.push(fecha.to_string());
        if !hora.is_empty() {
            p.push(hora.replace(':', "-"));
        }
    }
    if !exp.is_empty() {
        p.push(safe_part(exp).to_uppercase());
    }
    if !nr.is_empty() {
        p.push(format!("Res-{}-{}", safe_part(nr), safe_part(anno)));
    }
    let c = court_short(despacho);
    if !c.is_empty() {
        p.push(c);
    }
    p.push(safe_part(id));
    p.push(format!("research-{}", safe_part(case).to_uppercase()));
    format!("{}.pdf", p.join("_"))
}

pub fn root(conf: &Conf) -> PathBuf {
    conf.case.dir.join("_research")
}

pub fn target(conf: &Conf, case: &str, id: &str, despacho: &str, nr: &str, anno: &str) -> PathBuf {
    let mut name = format!("{}_{}", safe_part(case).to_uppercase(), safe_part(despacho));
    if !nr.is_empty() {
        name.push_str(&format!("_Res-{}-{}", safe_part(nr), safe_part(anno)));
    }
    name.push_str(&format!("_{}.pdf", safe_part(id)));
    root(conf).join(case.replace(['/', '\\', ':'], "_")).join(name)
}

static FETCH_ONE: Mutex<Option<Instant>> = Mutex::new(None);
const FETCH_GAP: Duration = Duration::from_secs(5);

#[allow(clippy::too_many_arguments)]
pub fn fetch_pdf<R: Runtime>(
    app: &AppHandle<R>,
    conf: &Conf,
    case: &str,
    id: &str,
    despacho: &str,
    nr: &str,
    anno: &str,
    date: &str,
    descriptor: &str,
) -> Result<Value, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("bad id".into());
    }
    let dir = root(conf).join(case.replace(['/', '\\', ':'], "_"));

    if let Some(p) = existing(&dir, id) {
        return Ok(json!({ "path": p, "new": false }));
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut last = FETCH_ONE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(t) = *last {
        while t.elapsed() < FETCH_GAP {
            if cancelled() {
                return Err("cancelled".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    *last = Some(Instant::now());

    let path = dir.join(format!("_dl_{}.pdf", safe_part(id)));
    let _ = std::fs::remove_file(&path);
    CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
    if CUR.lock().unwrap_or_else(|p| p.into_inner()).is_empty() {
        set_origin(&conf.nexus_url);
    }

    let t0 = Instant::now();
    let inapp = format!(
        "(function(){{try{{if(!window.angular)return false;var inj=angular.element(document.body).injector();var l=inj.get('$location'),r=inj.get('$rootScope');r.$apply(function(){{l.url('/document/'+{id})}});return true}}catch(e){{return false}}}})()",
        id = js_lit(id)
    );
    let w = match app.get_webview_window(LABEL) {
        Some(w)
            if !gone(&w)
                && eval(&w, &inapp, Duration::from_secs(8))
                    .map(|v| v.as_bool().unwrap_or(false))
                    .unwrap_or(false) =>
        {
            w
        }
        _ => window(app, conf, &format!("{}/document/{id}", origin()))?,
    };

    let ready = format!(
        "(function(){{if(location.pathname.indexOf({id})<0)return false;var b=document.querySelector('[ng-click=\"downloadDocument(\\'pdf\\')\"]');if(!b||!window.angular)return false;var s=angular.element(b).scope();var d=s&&s._display;return !!(d&&String(d.id)==={id});}})()",
        id = js_lit(id)
    );
    let ready = ready.as_str();
    loop {
        std::thread::sleep(Duration::from_millis(500));
        if cancelled() {
            return Err("cancelled".into());
        }
        if gone(&w) {
            return Err("closed".into());
        }
        if eval(&w, ready, Duration::from_secs(8))
            .map(|v| v.as_bool().unwrap_or(false))
            .unwrap_or(false)
        {
            break;
        }
        if let Ok(v) = eval(
            &w,
            "/captcha/i.test(document.body?document.body.innerText:'')",
            Duration::from_secs(5),
        ) {
            if v.as_bool().unwrap_or(false) {
                let _ = w.show();
                return Err("captcha".into());
            }
        }
        if let Ok(v) = eval(
            &w,
            "/The specified URL cannot be found/i.test(document.body?document.body.innerText:'')",
            Duration::from_secs(5),
        ) {
            if v.as_bool().unwrap_or(false) {
                crate::focus::app_event("research: Nexus answers \"The specified URL cannot be found\" – search address too long for the firewall, no further tries");
                return Err("blocked".into());
            }
        }
        if t0.elapsed() > Duration::from_secs(conf.nexus_timeout) {
            return Err("timeout".into());
        }
    }
    let t_page = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();

    let got = export_pdf(&w, &path, conf.nexus_timeout.max(30));
    crate::focus::app_event(&format!(
        "research: Nexus judgment – page ready after {:.1} s, export {:.1} s{}",
        t_page,
        t1.elapsed().as_secs_f64(),
        match &got {
            Ok(_) => String::new(),
            Err(e) => format!(" – {e}"),
        }
    ));
    let got: Result<Result<PathBuf, String>, ()> = match got {
        Ok(p) => Ok(Ok(p)),
        Err(e) if e == "timeout" => Err(()),
        Err(e) => Ok(Err(e)),
    };
    match got {
        Ok(Ok(p)) => {
            let h = pdf_head(&p);
            let s = |k: &str| h[k].as_str().unwrap_or("").to_string();
            let fecha = if s("fecha").is_empty() {
                nexus_date(date).unwrap_or_default()
            } else {
                s("fecha")
            };
            let (nr2, anno2) = if s("nr").is_empty() {
                (nr.to_string(), anno.to_string())
            } else {
                (s("nr"), s("anno"))
            };
            let name = judgment_name(case, id, &fecha, &s("hora"), &s("exp"), &nr2, &anno2, despacho);
            let fin = dir.join(&name);
            let fin = if std::fs::rename(&p, &fin).is_ok() { fin } else { p.clone() };
            if !fecha.is_empty() {
                if let Ok(d) = chrono::NaiveDate::parse_from_str(&fecha, "%Y-%m-%d") {
                    let (hh, mm) = s("hora")
                        .split_once(':')
                        .map(|(a, b)| (a.parse().unwrap_or(0), b.parse().unwrap_or(0)))
                        .unwrap_or((0, 0));
                    if let Some(t) = d.and_hms_opt(hh, mm, 0) {
                        crate::run::download::set_file_times(&fin, t.and_utc().timestamp() + 6 * 3600);
                    }
                }
            }

            remember(
                &fin,
                json!({ "id": id, "case": case, "despacho": despacho, "nr": nr2, "anno": anno2, "date": date, "fecha": fecha, "hora": s("hora"), "exp": s("exp"), "clase": s("clase"), "descriptor": descriptor, "at": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(), "pages": crate::state::count_pages(&fin) }),
            );
            Ok(json!({ "path": fin, "new": true }))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            PENDING.lock().unwrap_or_else(|p| p.into_inner()).take();
            Err("timeout".into())
        }
    }
}

pub fn show<R: Runtime>(app: &AppHandle<R>, conf: &Conf, url: Option<&str>) -> Result<(), String> {
    let w = match (app.get_webview_window(LABEL), url) {
        (Some(w), None) => w,
        (_, Some(u)) if url_ok(u) => window(app, conf, &rebase(u, &origin_or(conf)))?,
        (None, _) => window(app, conf, &format!("{}/search?q=&advanced=true", origin_or(conf)))?,
        _ => return Err("refused".into()),
    };
    let _ = w.show();
    let _ = w.unminimize();
    let _ = w.set_focus();
    Ok(())
}

const META: &str = "_nexus.json";

fn remember(pdf: &std::path::Path, v: Value) {
    let Some(dir) = pdf.parent() else { return };
    let name = pdf.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut m: serde_json::Map<String, Value> = std::fs::read_to_string(dir.join(META))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    m.insert(name, v);
    let _ = std::fs::write(dir.join(META), serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default());
}

fn from_name(name: &str) -> Value {
    let stem = name.trim_end_matches(".pdf");
    let parts: Vec<&str> = stem.split('_').collect();
    let id = id_of(name);
    let (mut nr, mut anno) = (String::new(), String::new());
    if let Some(r) = parts.iter().find(|p| p.starts_with("Res-")) {
        let x: Vec<&str> = r[4..].rsplitn(2, '-').collect();
        if x.len() == 2 {
            anno = x[0].to_string();
            nr = x[1].to_string();
        }
    }
    let despacho = parts.get(1).map(|d| {
        let t = d.replace('-', " ");
        let mut c = t.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
    });
    json!({ "id": id, "despacho": despacho.unwrap_or_default(), "nr": nr, "anno": anno })
}

pub fn list_all(conf: &Conf) -> Value {
    let root = root(conf);
    let mut out: Vec<Value> = Vec::new();
    for d in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let dir = d.path();
        let dn = d.file_name().to_string_lossy().to_string();
        if !dir.is_dir() || dn.starts_with('_') {
            continue;
        }
        let mut meta: serde_json::Map<String, Value> = std::fs::read_to_string(dir.join(META))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut changed = false;
        for f in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().map(|x| x != "pdf").unwrap_or(true) {
                continue;
            }
            let name = f.file_name().to_string_lossy().to_string();
            if name.starts_with('_') {
                continue;
            }
            let md = f.metadata().ok();
            let mut v = meta.get(&name).cloned().unwrap_or_else(|| from_name(&name));
            if v["pages"].is_null() {
                v["pages"] = json!(crate::state::count_pages(&p));
                meta.insert(name.clone(), v.clone());
                changed = true;
            }

            if v["exp"].is_null() {
                let h = pdf_head(&p);
                for k in ["exp", "fecha", "hora", "clase"] {
                    v[k] = json!(h[k].as_str().unwrap_or(""));
                }
                meta.insert(name.clone(), v.clone());
                changed = true;
            }
            if v["case"].as_str().unwrap_or("").is_empty() {
                v["case"] = json!(dn);
            }
            if v["at"].as_str().unwrap_or("").is_empty() {
                v["at"] = json!(md
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default());
            }
            v["path"] = json!(p);
            v["name"] = json!(name);
            v["size"] = json!(md.map(|m| m.len()).unwrap_or(0));
            v["url"] = json!(format!("{}/document/{}", origin(), v["id"].as_str().unwrap_or("")));
            out.push(v);
        }
        if changed {
            let _ = std::fs::write(
                dir.join(META),
                serde_json::to_string_pretty(&Value::Object(meta)).unwrap_or_default(),
            );
        }
    }
    out.sort_by(|a, b| b["at"].as_str().cmp(&a["at"].as_str()));
    Value::Array(out)
}

fn judgment(conf: &Conf, path: &str) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(root(conf)).map_err(|_| "refused")?;
    let p = std::fs::canonicalize(path).map_err(|_| "notfound")?;
    let ok = p.extension().map(|x| x == "pdf").unwrap_or(false)
        && p.parent().and_then(|d| d.parent()).map(|r| r == root).unwrap_or(false)
        && !p
            .parent()
            .unwrap()
            .file_name()
            .map(|n| n.to_string_lossy().starts_with('_'))
            .unwrap_or(true);
    if ok {
        Ok(p)
    } else {
        Err("refused".into())
    }
}

pub fn trash(conf: &Conf, paths: &[String]) -> Result<Value, String> {
    let files: Vec<PathBuf> = paths.iter().map(|p| judgment(conf, p)).collect::<Result<_, _>>()?;
    trash::delete_all(&files).map_err(|e| e.to_string())?;
    for f in &files {
        let Some(dir) = f.parent() else { continue };
        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if let Some(mut m) = std::fs::read_to_string(dir.join(META))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Map<String, Value>>(&s).ok())
        {
            if m.remove(&name).is_some() {
                let _ = std::fs::write(dir.join(META), serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default());
            }
        }
    }
    Ok(json!({ "n": files.len() }))
}

pub fn save_to<R: Runtime>(app: &AppHandle<R>, conf: &Conf, paths: &[String]) -> Result<Value, String> {
    use tauri_plugin_dialog::DialogExt;
    let files: Vec<PathBuf> = paths.iter().map(|p| judgment(conf, p)).collect::<Result<_, _>>()?;
    let Some(dest) = app.dialog().file().blocking_pick_folder() else {
        return Ok(Value::Null);
    };
    let dest = dest.into_path().map_err(|e| e.to_string())?;
    let mut n = 0;
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let mut t = dest.join(&name);
        let mut k = 2;
        while t.exists() {
            t = dest.join(format!("{} ({k}).pdf", name.trim_end_matches(".pdf")));
            k += 1;
        }
        std::fs::copy(f, &t).map_err(|e| e.to_string())?;
        n += 1;
    }
    Ok(json!({ "n": n, "dir": dest }))
}

pub fn zip_to<R: Runtime>(app: &AppHandle<R>, conf: &Conf, paths: &[String]) -> Result<Value, String> {
    use std::io::Write;
    use tauri_plugin_dialog::DialogExt;
    let files: Vec<PathBuf> = paths.iter().map(|p| judgment(conf, p)).collect::<Result<_, _>>()?;
    let Some(dest) = app.dialog().file().blocking_pick_folder() else {
        return Ok(Value::Null);
    };
    let dest = dest.into_path().map_err(|e| e.to_string())?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M").to_string();
    let mut t = dest.join(format!("research_{stamp}.zip"));
    let mut k = 2;
    while t.exists() {
        t = dest.join(format!("research_{stamp}_{k}.zip"));
        k += 1;
    }
    let f = std::fs::File::create(&t).map_err(|e| e.to_string())?;
    let mut z = zip::ZipWriter::new(f);
    let opt = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    for p in &files {
        let case = p
            .parent()
            .and_then(|d| d.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        z.start_file(format!("{case}/{name}"), opt).map_err(|e| e.to_string())?;
        z.write_all(&std::fs::read(p).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    }
    z.finish().map_err(|e| e.to_string())?;
    Ok(json!({ "n": files.len(), "path": crate::paths::plain_path(&t) }))
}

pub fn kept(conf: &Conf, case: &str) -> Value {
    let dir = root(conf).join(case.replace(['/', '\\', ':'], "_"));
    let mut v: Vec<Value> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "pdf").unwrap_or(false) && !e.file_name().to_string_lossy().starts_with('_'))
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let id = id_of(&name);
            let t = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            json!({ "path": e.path(), "name": name, "id": id, "at": t })
        })
        .collect();
    v.sort_by(|a, b| b["at"].as_str().cmp(&a["at"].as_str()));
    Value::Array(v)
}

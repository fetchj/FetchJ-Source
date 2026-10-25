use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

pub const PORTAL_JS: &str = include_str!("portal.js");

pub const LOGIN_JS: &str = include_str!("login.js");

#[derive(Debug, Clone, PartialEq)]
pub enum BridgeError {
    Closed,

    Timeout,

    NotReady,

    Js(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::Closed => write!(f, "the portal window was closed"),
            BridgeError::Timeout => write!(f, "the portal page did not answer"),
            BridgeError::NotReady => write!(f, "the portal page is still loading"),
            BridgeError::Js(e) => write!(f, "page script error: {e}"),
        }
    }
}

pub type BResult<T> = Result<T, BridgeError>;

pub fn init_script(api_part: &str, case_list: &str, legajo_list: &str, logged_in_text: &str) -> String {
    let cfg = json!({"apiPart": api_part, "caseList": case_list, "legajoList": legajo_list, "loggedInText": logged_in_text});
    format!("window.__fjCfg={cfg};\n{PORTAL_JS}")
}

pub fn portal_scope(portal_url: &str) -> (String, String) {
    match tauri::Url::parse(portal_url) {
        Ok(u) => {
            let origin = u.origin().ascii_serialization();
            let first = u.path_segments().and_then(|mut s| s.next().map(String::from)).unwrap_or_default();
            (origin, if first.is_empty() { "/".into() } else { format!("/{first}/") })
        }
        Err(_) => ("https://pjenlinea.poder-judicial.go.cr".into(), "/GestionEnLinea/".into()),
    }
}

static SCOPE: std::sync::RwLock<Option<(String, String)>> = std::sync::RwLock::new(None);

pub fn set_scope(portal_url: &str) {
    *SCOPE.write().unwrap() = Some(portal_scope(portal_url));
}

static LOGIN_HOSTS: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

pub fn set_login_hosts(h: &[String]) {
    *LOGIN_HOSTS.write().unwrap() = h.to_vec();
}

pub fn in_login(url: &str) -> bool {
    match tauri::Url::parse(url) {
        Ok(u) if u.scheme() == "https" => {
            let h = u.host_str().unwrap_or("").to_lowercase();
            LOGIN_HOSTS.read().unwrap().iter().any(|x| *x == h)
        }
        _ => false,
    }
}

pub fn in_scope(url: &str) -> bool {
    let (origin, path) = SCOPE.read().unwrap().clone().unwrap_or_else(|| portal_scope(""));
    match tauri::Url::parse(url) {
        Ok(u) => u.origin().ascii_serialization() == origin && format!("{}/", u.path().trim_end_matches('/')).starts_with(&path),
        Err(_) => false,
    }
}

static PORTAL_ZOOM: std::sync::Mutex<f64> = std::sync::Mutex::new(0.0);
static ZOOM_SET: once_cell::sync::Lazy<std::sync::Mutex<Instant>> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(Instant::now()));

pub fn portal_zoom() -> f64 {
    *PORTAL_ZOOM.lock().unwrap()
}
pub fn set_portal_zoom(z: f64) {
    *PORTAL_ZOOM.lock().unwrap() = z;
}

pub fn zoom_applied() {
    *ZOOM_SET.lock().unwrap() = Instant::now();
}

fn geometry_file() -> std::path::PathBuf {
    crate::paths::local_dir(&std::env::temp_dir()).join("portal_window.json")
}

#[derive(Debug, Clone, Default)]
pub struct Geometry {
    pub rect: Option<(f64, f64, f64, f64, bool)>,
    pub zoom: f64,
    pub setting: f64,
}

static GEOM_SETTING: std::sync::Mutex<f64> = std::sync::Mutex::new(0.0);

pub fn load_geometry() -> Geometry {
    let v: Value = std::fs::read_to_string(geometry_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let f = |k: &str| v.get(k).and_then(|x| x.as_f64());
    let rect = match (f("x"), f("y"), f("w"), f("h")) {
        (Some(x), Some(y), Some(w), Some(h)) if w >= 300.0 && h >= 200.0 => {
            Some((x, y, w, h, v.get("max").and_then(|m| m.as_bool()).unwrap_or(false)))
        }
        _ => None,
    };
    Geometry {
        rect,
        zoom: f("zoom").unwrap_or(0.0),
        setting: f("setting").unwrap_or(0.0),
    }
}

pub fn note_zoom_setting(z: f64) {
    *GEOM_SETTING.lock().unwrap() = z;
}

fn save_geometry(rect: Option<(f64, f64, f64, f64, bool)>, zoom: f64) {
    let old = load_geometry();
    let mut v = serde_json::Map::new();
    if let Some((x, y, w, h, m)) = rect.or(old.rect) {
        v.insert("x".into(), json!(x));
        v.insert("y".into(), json!(y));
        v.insert("w".into(), json!(w));
        v.insert("h".into(), json!(h));
        v.insert("max".into(), json!(m));
    }
    v.insert("zoom".into(), json!(zoom));
    v.insert("setting".into(), json!(*GEOM_SETTING.lock().unwrap()));
    let p = geometry_file();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, Value::Object(v).to_string());
}

#[cfg(desktop)]
pub fn remember_window_of<R: Runtime>(w: &WebviewWindow<R>) {
    if w.is_minimized().unwrap_or(false) || w.is_fullscreen().unwrap_or(false) {
        save_geometry(None, portal_zoom());
        return;
    }
    let sc = w.scale_factor().unwrap_or(1.0);
    let (Ok(pos), Ok(size)) = (w.outer_position(), w.inner_size()) else {
        return;
    };
    let g = (
        pos.x as f64 / sc,
        pos.y as f64 / sc,
        size.width as f64 / sc,
        size.height as f64 / sc,
        w.is_maximized().unwrap_or(false),
    );
    save_geometry(Some(g), portal_zoom());
}

pub struct Portal<R: Runtime> {
    pub app: AppHandle<R>,
    pub label: String,

    pub closed: Arc<AtomicBool>,

    pub app_url: Option<String>,

    pub zoom: f64,
}

static FOCUS_LOG: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);
static LAST_ACTION: std::sync::Mutex<Option<(Instant, String)>> = std::sync::Mutex::new(None);

pub fn set_focus_log(path: std::path::PathBuf) {
    *FOCUS_LOG.lock().unwrap_or_else(|p| p.into_inner()) = Some(path);
}

pub fn note_action(what: &str) {
    let w: String = what.chars().filter(|c| *c != '\n' && *c != '\r').take(110).collect();
    *LAST_ACTION.lock().unwrap_or_else(|p| p.into_inner()) = Some((Instant::now(), w));
}

pub fn focus_trace(event: &str) {
    let path = FOCUS_LOG.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let last = LAST_ACTION
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .map(|(t, w)| (t.elapsed().as_secs_f64(), w));

    crate::focus::run_log(event, last.clone());
    let tail = match last {
        Some((s, w)) => format!(" | last FetchJ action {s:.2} s before: {w}"),
        None => " | no FetchJ action yet".into(),
    };
    let Some(path) = path else { return };
    let line = format!(
        "{} {event} | program in front: {}{tail}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        crate::focus::foreground()
    );
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = std::io::Write::write_all(&mut f, line.as_bytes());
    }
}

pub fn focus_event(label: &str, focused: bool) {
    focus_trace(&format!(
        "window '{label}' {}",
        if focused { "GOT the focus" } else { "lost the focus" }
    ));
}

#[cfg(desktop)]
pub fn app_focused<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.webview_windows().values().any(|w| w.is_focused().unwrap_or(false))
}

#[cfg(desktop)]
pub fn raise<R: Runtime>(w: &WebviewWindow<R>, attention: bool, why: &str) {
    if app_focused(w.app_handle()) {
        focus_trace(&format!(
            "raise '{}' ({why}): FetchJ is in front -> window brought to the front",
            w.label()
        ));
        note_action(&format!("raise '{}' ({why})", w.label()));
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    if !attention {
        focus_trace(&format!(
            "raise '{}' ({why}): another program is in front -> nothing done",
            w.label()
        ));
        return;
    }
    focus_trace(&format!(
        "raise '{}' ({why}): another program is in front -> only the taskbar button flashes",
        w.label()
    ));
    let target = if w.is_visible().unwrap_or(false) {
        Some(w.clone())
    } else {
        w.app_handle().get_webview_window("main")
    };
    if let Some(t) = target {
        let _ = t.request_user_attention(Some(tauri::UserAttentionType::Critical));
    }
}

impl<R: Runtime> Clone for Portal<R> {
    fn clone(&self) -> Self {
        Portal {
            app: self.app.clone(),
            label: self.label.clone(),
            closed: self.closed.clone(),
            app_url: self.app_url.clone(),
            zoom: self.zoom,
        }
    }
}

impl<R: Runtime> Portal<R> {
    pub fn window(&self) -> BResult<WebviewWindow<R>> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(BridgeError::Closed);
        }
        self.app.get_webview_window(&self.label).ok_or(BridgeError::Closed)
    }

    pub fn eval_raw(&self, js: &str, timeout: Duration) -> BResult<Value> {
        let w = self.window()?;
        let here = w.url().map(|u| u.to_string()).unwrap_or_default();
        if !in_scope(&here) {
            return Err(BridgeError::NotReady);
        }
        let (origin, path) = SCOPE.read().unwrap().clone().unwrap_or_else(|| portal_scope(""));
        let js = format!(
            "((location.origin==={}&&location.pathname.indexOf({})===0)?({js}):JSON.stringify({{nofj:1}}))",
            js_lit(&origin),
            js_lit(&path)
        );
        let (tx, rx) = mpsc::channel::<String>();
        w.eval_with_callback(&js, move |s: String| {
            let _ = tx.send(s);
        })
        .map_err(|e| BridgeError::Js(e.to_string()))?;
        let t0 = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(s) => {
                    let v: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
                    let v = match v {
                        Value::String(inner) => serde_json::from_str(&inner).unwrap_or(Value::String(inner)),
                        x => x,
                    };
                    return Ok(v);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(BridgeError::Timeout),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.closed.load(Ordering::SeqCst) || self.app.get_webview_window(&self.label).is_none() {
                        return Err(BridgeError::Closed);
                    }
                    if t0.elapsed() > timeout {
                        return Err(BridgeError::Timeout);
                    }
                }
            }
        }
    }

    pub fn login_call(&self, expr: &str) -> BResult<Value> {
        let w = self.window()?;
        let here = w.url().map(|u| u.to_string()).unwrap_or_default();
        if !in_login(&here) {
            return Err(BridgeError::NotReady);
        }
        let hosts = serde_json::to_string(&*LOGIN_HOSTS.read().unwrap()).unwrap_or_else(|_| "[]".into());
        let js = format!(
            "(function(){{try{{if({hosts}.indexOf(location.hostname.toLowerCase())<0)return JSON.stringify({{nofj:1}});var L=window.__fjl;if(!L)return JSON.stringify({{nofj:1}});return JSON.stringify({{ok:({expr})}});}}catch(e){{return JSON.stringify({{err:String(e&&e.message||e)}});}}}})()"
        );
        let (tx, rx) = mpsc::channel::<String>();
        w.eval_with_callback(&js, move |s: String| {
            let _ = tx.send(s);
        })
        .map_err(|e| BridgeError::Js(e.to_string()))?;
        let v = match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(s) => {
                let v: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
                match v {
                    Value::String(inner) => serde_json::from_str(&inner).unwrap_or(Value::String(inner)),
                    x => x,
                }
            }
            Err(_) => return Err(BridgeError::Timeout),
        };
        if v.get("nofj").is_some() {
            return Err(BridgeError::NotReady);
        }
        if let Some(e) = v.get("err").and_then(|e| e.as_str()) {
            return Err(BridgeError::Js(e.chars().take(200).collect()));
        }
        Ok(v.get("ok").cloned().unwrap_or(Value::Null))
    }

    pub fn login_click(&self, sel_js: &str) -> BResult<bool> {
        #[cfg(windows)]
        {
            if let Some(r) = self.login_call(&format!("L.rect({sel_js})"))?.as_object().cloned() {
                let x = r.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let y = r.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);

                let dpr = r.get("dpr").and_then(|v| v.as_f64()).unwrap_or(1.0);
                let sc = self.window().ok().and_then(|w| w.scale_factor().ok()).unwrap_or(1.0);
                let z = dpr / sc;
                let mut cands = vec![(x, y)];
                if (z - 1.0).abs() > 0.02 {
                    cands.push((x * z, y * z));
                }
                for (cx, cy) in cands {
                    let _ = self.login_call(&format!("L.arm({sel_js})"));
                    if !self.cdp_mouse(cx, cy, false) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(150));
                    if self.login_call("L.hov()").ok().and_then(|v| v.as_bool()).unwrap_or(false) {
                        if self.cdp_mouse(cx, cy, true) {
                            return Ok(true);
                        }
                        break;
                    }
                }
            } else {
                return Ok(false);
            }
        }
        Ok(self.login_call(&format!("L.click({sel_js})"))?.as_bool().unwrap_or(false))
    }

    pub fn login_type(&self, sel_js: &str, text: &str) -> BResult<bool> {
        if !self
            .login_call(&format!("L.focus({sel_js}) && L.clear({sel_js})"))?
            .as_bool()
            .unwrap_or(false)
        {
            return Ok(false);
        }
        #[cfg(windows)]
        {
            if let Ok(w) = self.window() {
                let p = serde_json::json!({ "text": text }).to_string();
                let (tx, rx) = mpsc::channel::<bool>();
                let r = w.with_webview(move |wv| unsafe {
                    use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
                    use windows_core::HSTRING;
                    let ok = (|| -> windows_core::Result<()> {
                        let core = wv.controller().CoreWebView2()?;
                        let h = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
                        core.CallDevToolsProtocolMethod(&HSTRING::from("Input.insertText"), &HSTRING::from(p.as_str()), &h)?;
                        Ok(())
                    })()
                    .is_ok();
                    let _ = tx.send(ok);
                });
                if r.is_ok() && rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false) {
                    std::thread::sleep(Duration::from_millis(250));
                    if self.login_call(&format!("L.len({sel_js})"))?.as_u64().unwrap_or(0) == text.chars().count() as u64 {
                        return Ok(true);
                    }
                }
            }
        }
        let t = serde_json::to_string(text).unwrap_or_default();
        Ok(self.login_call(&format!("L.fill({sel_js}, {t})"))?.as_bool().unwrap_or(false))
    }

    #[cfg(windows)]
    fn cdp_mouse(&self, x: f64, y: f64, click: bool) -> bool {
        let Ok(w) = self.window() else { return false };
        let kinds: &[&str] = if click {
            &["mousePressed", "mouseReleased"]
        } else {
            &["mouseMoved"]
        };
        let evs: Vec<String> = kinds
            .iter()
            .map(|t| {
                let extra = if *t == "mouseMoved" {
                    ""
                } else {
                    r#","button":"left","clickCount":1"#
                };
                format!(r#"{{"type":"{t}","x":{x:.1},"y":{y:.1}{extra}}}"#)
            })
            .collect();
        let (tx, rx) = mpsc::channel::<bool>();
        let r = w.with_webview(move |wv| unsafe {
            use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
            use windows_core::HSTRING;
            let ok = (|| -> windows_core::Result<()> {
                let core = wv.controller().CoreWebView2()?;
                for p in &evs {
                    let h = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
                    core.CallDevToolsProtocolMethod(&HSTRING::from("Input.dispatchMouseEvent"), &HSTRING::from(p.as_str()), &h)?;
                }
                Ok(())
            })()
            .is_ok();
            let _ = tx.send(ok);
        });
        r.is_ok() && rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false)
    }

    pub fn call(&self, expr: &str) -> BResult<Value> {
        self.call_t(expr, Duration::from_secs(15))
    }

    pub fn call_t(&self, expr: &str, timeout: Duration) -> BResult<Value> {
        if [
            "click",
            "Click",
            "FJ.mouse",
            "FJ.focus",
            "history.back",
            "FJ.check",
            "FJ.dialogButton",
        ]
        .iter()
        .any(|k| expr.contains(k))
        {
            note_action(&format!("in the portal page: {expr}"));
        }
        let js = format!(
            "(function(){{try{{var FJ=window.__fj;if(!FJ)return JSON.stringify({{nofj:1}});return JSON.stringify({{ok:({expr})}});}}catch(e){{return JSON.stringify({{err:String(e&&e.message||e)}});}}}})()"
        );
        let v = self.eval_raw(&js, timeout)?;
        if v.get("nofj").is_some() {
            return Err(BridgeError::NotReady);
        }
        if let Some(e) = v.get("err").and_then(|e| e.as_str()) {
            return Err(BridgeError::Js(e.chars().take(200).collect()));
        }
        Ok(v.get("ok").cloned().unwrap_or(Value::Null))
    }

    pub fn call_bool(&self, expr: &str) -> bool {
        self.call(expr).ok().and_then(|v| v.as_bool()).unwrap_or(false)
    }

    pub fn url(&self) -> String {
        self.window()
            .ok()
            .and_then(|w| w.url().ok())
            .map(|u| u.to_string())
            .unwrap_or_default()
    }

    pub fn short_url(&self) -> String {
        short_url(&self.url())
    }

    pub fn host(&self) -> String {
        self.window()
            .ok()
            .and_then(|w| w.url().ok())
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_default()
    }

    pub fn alive(&self) -> bool {
        self.window().is_ok()
    }

    pub fn navigate(&self, url: &str) -> BResult<()> {
        note_action("navigate the portal window to a new address");
        let w = self.window()?;
        let u = url.parse().map_err(|_| BridgeError::Js("bad url".into()))?;
        w.navigate(u).map_err(|e| BridgeError::Js(e.to_string()))
    }

    pub fn go_back(&self) -> BResult<()> {
        self.eval_raw("(function(){history.back();return 1})()", Duration::from_secs(5))
            .map(|_| ())
    }

    pub fn show(&self) {
        if let Ok(w) = self.window() {
            #[cfg(desktop)]
            raise(&w, true, "portal needed: login / portal message");
            #[cfg(not(desktop))]
            {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        self.apply_zoom();
    }

    pub fn apply_zoom(&self) {
        #[cfg(desktop)]
        {
            let z = portal_zoom();
            if z > 0.0 {
                if let Ok(w) = self.window() {
                    let _ = w.set_zoom(z);
                }
            }
        }
    }

    pub fn zoom_follow(&self) -> bool {
        #[cfg(desktop)]
        {
            if portal_zoom() <= 0.0 || ZOOM_SET.lock().unwrap().elapsed() < Duration::from_millis(1500) {
                return false;
            }
            let Ok(w) = self.window() else { return false };
            let scale = w.scale_factor().unwrap_or(1.0);
            let Ok(v) = self.eval_raw("(function(){return window.devicePixelRatio||1})()", Duration::from_secs(2)) else {
                return false;
            };
            let Some(dpr) = v.as_f64() else { return false };
            let z = ((dpr / scale) * 100.0).round() / 100.0;
            if (z - portal_zoom()).abs() > 0.04 && (0.25..=3.0).contains(&z) {
                set_portal_zoom(z);
                return true;
            }
        }
        false
    }

    #[cfg(desktop)]
    pub fn remember_window(&self) {
        if let Ok(w) = self.window() {
            remember_window_of(&w);
        }
    }

    pub fn focus_main(&self) {
        #[cfg(desktop)]
        if let Some(w) = self.app.get_webview_window("main") {
            raise(&w, false, "program window after the login");
        }
    }

    pub fn hide(&self) {
        note_action("hide the portal window");
        #[cfg(desktop)]
        if let Ok(w) = self.window() {
            let _ = w.hide();
        }
    }

    pub fn minimize(&self) {
        note_action("minimize the portal window");
        #[cfg(desktop)]
        if let Ok(w) = self.window() {
            let _ = w.minimize();
        }
    }

    pub fn close(&self) {
        if let Ok(w) = self.window() {
            if cfg!(mobile) {
                if let Some(u) = &self.app_url {
                    let _ = self.navigate(u);
                }
            } else {
                #[cfg(desktop)]
                self.remember_window();
                let _ = w.close();
            }
        }
    }

    pub fn wait(&self, ms: u64) -> BResult<()> {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(ms.min(100)));
            if self.closed.load(Ordering::SeqCst) {
                return Err(BridgeError::Closed);
            }
        }
        Ok(())
    }
}

pub fn short_url(u: &str) -> String {
    u.split(['?', '#']).next().unwrap_or("").to_string()
}

fn js_lit(s: &str) -> String {
    js(s)
}

pub fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

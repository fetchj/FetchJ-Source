use super::{ApiResp, Fetcher, RResult, RunControl, RunErr, BROWSER_LABEL};
use crate::bridge::js;
use crate::state::{login_dirty, set_login_state};
use chrono::{Local, TimeZone};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

pub const HDR_MAX_AGE_RUN: u64 = 25 * 60;
pub const HDR_MAX_AGE_ONLINE: u64 = 20 * 60;
pub const LOGIN_WAIT_MAX: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Default, Clone)]
pub struct PageInfo {
    pub host: String,
    pub path: String,
    pub salir: bool,
    pub token_exp: i64,
    pub hdr_exp: i64,
    pub has_hdr: bool,
    pub back: bool,

    pub auth_time: i64,
}

pub fn now_s() -> i64 {
    chrono::Utc::now().timestamp()
}

fn hhmmss(epoch: i64) -> String {
    Local
        .timestamp_opt(epoch, 0)
        .single()
        .map(|d| d.format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

pub fn start_wake_watch(ctl: Arc<RunControl>) -> Arc<AtomicU64> {
    let gap = Arc::new(AtomicU64::new(0));
    let g2 = gap.clone();
    std::thread::spawn(move || {
        let mut last = SystemTime::now();
        loop {
            std::thread::sleep(Duration::from_secs(10));
            if !ctl.running.load(Ordering::SeqCst) {
                break;
            }
            let n = SystemTime::now();
            if let Ok(d) = n.duration_since(last) {
                let s = d.as_secs();
                if s > crate::defaults::timing::SLEEP_JUMP_SEC as u64 {
                    let v = s.saturating_sub(10);
                    if v > g2.load(Ordering::SeqCst) {
                        g2.store(v, Ordering::SeqCst);
                    }
                }
            }
            last = n;
        }
    });
    gap
}

impl Fetcher {
    pub fn page_info(&self) -> RResult<Option<PageInfo>> {
        let Some(v) = self.js_opt("FJ.info()")? else { return Ok(None) };
        for d in v.get("dialogs").and_then(|d| d.as_array()).cloned().unwrap_or_default() {
            self.warn(format!(
                "   message box of the portal ({}): {}",
                d["type"].as_str().unwrap_or(""),
                d["text"].as_str().unwrap_or("").chars().take(300).collect::<String>()
            ));
        }
        Ok(Some(PageInfo {
            host: v["host"].as_str().unwrap_or("").into(),
            path: v["path"].as_str().unwrap_or("").into(),
            salir: v["salir"].as_bool().unwrap_or(false),
            token_exp: v["tokenExp"].as_i64().unwrap_or(0),
            hdr_exp: v["hdrExp"].as_i64().unwrap_or(0),
            has_hdr: v["hasHdr"].as_bool().unwrap_or(false),
            back: v["back"].as_bool().unwrap_or(false),
            auth_time: v["authTime"].as_i64().unwrap_or(0),
        }))
    }

    fn unauthorized(&self) -> bool {
        self.portal
            .url()
            .to_lowercase()
            .contains(&self.cfg.sel["url_unauthorized_part"].to_lowercase())
    }

    pub fn is_logged_in(&self) -> RResult<bool> {
        if !self.portal.alive() {
            return Err(RunErr::BrowserClosed);
        }
        if self.portal.host() != self.portal_host || self.unauthorized() {
            return Ok(false);
        }
        let Some(i) = self.page_info()? else { return Ok(false) };
        if i.salir {
            return Ok(true);
        }
        Ok(i.token_exp - now_s() > 30 && i.has_hdr && !self.login_button_visible()?)
    }

    fn login_button_visible(&self) -> RResult<bool> {
        Ok(self
            .js_opt(&format!("FJ.visibleExact({}, 'ingresar')", js(&self.cfg.sel["login_button"])))?
            .and_then(|v| v.as_bool())
            .unwrap_or(false))
    }

    pub fn login_probe(&mut self) -> RResult<bool> {
        if !self.portal.alive() {
            self.probe_gone += 1;
            if self.probe_gone >= 2 {
                self.warn(format!(
                    "   the {BROWSER_LABEL} window of the program was closed - the run is stopped"
                ));
                return Err(RunErr::BrowserClosed);
            }
            return Ok(false);
        }
        self.probe_gone = 0;
        self.portal.zoom_follow();
        let mut diag = (false, false, false);
        if self.portal.host() == self.portal_host && !self.unauthorized() {
            if let Some(i) = self.page_info()? {
                diag = (i.salir, i.token_exp - now_s() > 30, i.has_hdr);
                if i.back {
                    self.portal.show();
                }
                let left = i.token_exp - now_s();

                if i.token_exp > 0 && left > 30 && i.has_hdr && !self.login_button_visible()? {
                    return Ok(true);
                }
                if i.salir {
                    if i.token_exp == 0 {
                        let since = *self.salir_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= Duration::from_secs(5) {
                            return Ok(true);
                        }
                    }
                    return Ok(false);
                }
            }
        }
        self.salir_since = None;

        if let Some(d) = self.js_opt("FJ.down()")?.filter(|v| v.is_object()) {
            let msg = d.get("msg").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let since = *self.down_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_secs(4) {
                let path = d.get("path").and_then(|v| v.as_str()).unwrap_or("");
                self.warn(format!("   the portal says it is not available: \"{msg}\" ({path})"));
                crate::focus::app_event(&format!("portal not available: \"{msg}\""));
                return Err(RunErr::PortalDown(if msg.is_empty() { path.to_string() } else { msg }));
            }
            return Ok(false);
        }
        self.down_since = None;

        let url = self.short_url().to_lowercase();
        let on_login = self.cfg.login_hosts.iter().any(|h| url.contains(h.as_str()));
        if !diag.0 && !on_login && !self.login_button_visible()? {
            let since = *self.blank_since.get_or_insert_with(Instant::now);
            let due = self.blank_nav.map(|t| t.elapsed() >= Duration::from_secs(90)).unwrap_or(true);
            if since.elapsed() >= Duration::from_secs(60) && due {
                self.blank_nav = Some(Instant::now());
                self.warn(format!(
                    "   the portal page shows neither 'Salir' nor 'Ingresar' ({}) - opening the start page of the portal for the login",
                    self.short_url()
                ));
                let _ = self.portal.navigate(&self.cfg.portal_url);
                self.assisted = false;
                self.wait_ms(self.cfg.ui_wait_long)?;
            }
        } else {
            self.blank_since = None;
        }
        if !self.assisted && self.assist_tried.elapsed() >= Duration::from_secs(5) {
            self.assist_login(0.0)?;
        }
        if self.probe_log.elapsed() >= Duration::from_secs(60) {
            self.probe_log = Instant::now();
            self.info(format!(
                "      waiting for the login: 1 portal tab(s) ({}) - not logged in yet",
                self.short_url()
            ));
            let yn = |b: bool| if b { "✔" } else { "✘" };

            let ing = if self.login_button_visible()? { "visible" } else { "not visible" };
            self.info(format!(
                "      login check: 'Salir' {}, access token {}, call of the page {}, 'Ingresar' {}",
                yn(diag.0),
                yn(diag.1),
                yn(diag.2),
                ing
            ));
        }
        Ok(false)
    }

    pub fn wait_login(&mut self, stable: f64, every: f64, relogin_old: Option<i64>) -> RResult<&'static str> {
        let mut since: Option<Instant> = None;

        let begun = Instant::now();
        loop {
            self.check_stop()?;
            if begun.elapsed() >= LOGIN_WAIT_MAX {
                self.warn("   nobody logged in within 30 minutes - the run is stopped");
                return Err(RunErr::LoginTimeout);
            }
            if self.ctl.web_retry.swap(false, Ordering::SeqCst) {
                self.portal.show();
                if self.probe_ok(relogin_old)? {
                    return Ok("web");
                }
                self.warn("   'I am logged in' was clicked, but no logged-in portal tab was found yet");
                self.set_live_alert(Some("session_still"));
            }
            if self.probe_ok(relogin_old)? {
                match since {
                    Some(t) if t.elapsed().as_secs_f64() >= stable => return Ok("auto"),
                    None => since = Some(Instant::now()),
                    _ => {}
                }
            } else {
                since = None;
            }
            self.sleep(every)?;
        }
    }

    fn probe_ok(&mut self, relogin_old: Option<i64>) -> RResult<bool> {
        let ok = self.login_probe()?;
        if !ok {
            return Ok(false);
        }
        match relogin_old {
            None => Ok(true),
            Some(old) => {
                self.js_opt("FJ.updateToken(true)")?;
                Ok(self.page_info()?.map(|i| i.hdr_exp != old || !i.has_hdr).unwrap_or(false))
            }
        }
    }

    pub fn login_noticed(&mut self) {
        self.login_ok_at = Some(Instant::now());
        self.set_live_alert(None);
        self.note_login(true);
    }

    pub fn note_login(&mut self, fresh: bool) {
        self.totp_tries = 0;
        let auth = self.page_info().ok().flatten().map(|i| i.auth_time).unwrap_or(0);
        let n = now_s();
        let tok = auth > 0 && auth <= n + 60;
        if fresh {
            if tok && n - auth < 1800 {
                crate::portalsess::login(auth, "from the access token");
            } else {
                crate::portalsess::login(n, "noticed by FetchJ");
            }
        } else if tok {
            crate::portalsess::known_or(auth, "from the access token");
        } else {
            crate::portalsess::known_or(0, "unknown");
        }
        let c = crate::portalsess::current();
        if c.login > 0 {
            self.info(format!("Portal login: {} ({})", crate::portalsess::hms(c.login), c.source));
            if let Some(l) = crate::portalsess::expectation(c.login) {
                self.info(l);
            }
        } else {
            self.info("Portal login: time unknown (the portal window was already logged in when FetchJ started)");
        }
    }

    pub fn session_lost(&mut self, hint: &str) {
        let c = crate::portalsess::current();
        if c.login == 0 && c.last_renew == 0 {
            return;
        }
        let n = now_s();
        let cause = if hint == "sleep" {
            "sleep"
        } else if hint == "max_time" || hint == "max_start" {
            "max_time"
        } else if c.token_exp > 0 && n >= c.token_exp {
            "no_renewal"
        } else {
            "refused"
        };
        let d = self.js_opt("FJ.diag()").ok().flatten().unwrap_or(Value::Null);
        let page = d["page"].as_str().map(String::from).unwrap_or_else(|| self.short_url());
        let title = d["title"].as_str().unwrap_or("");
        let page = if title.is_empty() { page } else { format!("{page} ({title})") };
        let notice = [d["notice"].as_str().unwrap_or(""), d["hit"].as_str().unwrap_or("")]
            .iter()
            .filter(|t| !t.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        let mut net = vec![];
        let mut hosts = vec![self.portal_host.clone()];
        if let Some(h) = self.cfg.login_hosts.first() {
            hosts.push(h.clone());
        }
        for (i, h) in hosts.iter().enumerate() {
            let ok = crate::portalsess::reachable(h);
            net.push(format!(
                "{} {} {}",
                if i == 0 { "portal" } else { "login server" },
                h,
                if ok { "✔" } else { "✘" }
            ));
        }
        let e = crate::portalsess::End {
            cause: cause.into(),
            idle_min: crate::portalsess::idle_min(),
            page,
            notice,
            internet: net.join(", "),
        };
        let lines = crate::portalsess::ended(&self.version, &e);
        for (i, l) in lines.into_iter().enumerate() {
            if i == 0 {
                self.warn(l);
            } else {
                self.info(l);
            }
        }

        if hint == "max_start" {
            return;
        }
        let t = crate::portalsess::end_text();
        let body = if hint == "max_time" {
            crate::i18n::trf(
                "FetchJ logged out of the portal after the maximum time of {} min - switch it online again when you need it.",
                &[&self.cfg.session_max_min],
            )
        } else if t.is_empty() {
            crate::i18n::tr("The portal ended the login - please log in again in the portal window.")
        } else {
            crate::i18n::tr(&format!(
                "The portal ended the login after {t} - please log in again in the portal window."
            ))
        };
        self.sink.notify(&body);
    }

    pub fn login_at_start(&mut self) -> RResult<()> {
        for _ in 0..40 {
            if self.js_opt("1")?.is_some() {
                break;
            }
            self.wait_ms(250)?;
        }
        let mut logged = self.is_logged_in()?;
        if logged && self.session_expired_at_start()? {
            logged = false;
        }
        if logged {
            self.note_login(false);

            if crate::portalsess::max_due(self.cfg.session_max_min, 120) {
                self.warn(crate::i18n::trf(
                    "The login in the portal window is older than the maximum time of the portal session ({} min) - FetchJ logs out (Salir) and asks for a new login; the update goes on afterwards.",
                    &[&self.cfg.session_max_min],
                ));
                self.session_lost("max_start");
                self.logout_portal_as(true);
                let _ = self.portal.navigate(&self.cfg.portal_url);
                self.wait_ms(self.cfg.ui_wait_long)?;
                logged = self.is_logged_in()?;
            }
        }
        if logged {
            self.info(format!(
                "The {BROWSER_LABEL} window is already logged in to the portal - starting right away."
            ));
        } else {
            self.portal.show();

            self.assisted = false;
            self.blank_since = None;
            self.assist_login(20.0)?;
            self.set_live_alert(Some("login"));
            let how = self.wait_login(0.5, 0.5, None)?;
            self.login_noticed();
            if how == "auto" {
                self.info(format!(
                    "   login detected in the {BROWSER_LABEL} window - continuing automatically"
                ));
            } else {
                self.info("   'OK - logged in' clicked on the program page / case overview - continuing");
            }
            self.handle_session_dialog()?;
            self.set_live_alert(None);
        }
        self.ensure_logged_in(true)
    }

    fn session_expired_at_start(&mut self) -> RResult<bool> {
        let Some(i) = self.page_info()? else { return Ok(false) };
        if i.token_exp == 0 || i.token_exp - now_s() > 5 {
            return Ok(false);
        }
        self.warn(format!(
            "The {BROWSER_LABEL} window still shows the portal, but its session has expired (access key valid until {}) - opening the start page of the portal for a new login",
            Local.timestamp_opt(i.token_exp, 0).single().map(|d| d.format("%H:%M").to_string()).unwrap_or_default()
        ));
        let _ = self.portal.navigate(&self.cfg.portal_url);
        self.wait_ms(self.cfg.ui_wait_long)?;
        Ok(!self.is_logged_in()?)
    }

    pub fn ensure_logged_in(&mut self, _first: bool) -> RResult<()> {
        while !self.is_logged_in()? {
            self.session_lost("");
            self.portal.show();
            self.assisted = false;
            self.blank_since = None;
            self.assist_login(20.0)?;
            self.set_live_alert(Some("session"));
            let how = self.wait_login(0.5, 0.5, None)?;
            if how == "auto" {
                self.info(format!(
                    "   login detected in the {BROWSER_LABEL} window - continuing automatically"
                ));
            }
            self.login_noticed();
            self.handle_session_dialog()?;
            self.set_live_alert(None);
            self.info("   logged in again - continuing");
            self.portal_minimize_after_login();
        }
        if login_dirty(&self.cfg.state_dir).is_some() {
            set_login_state(&self.cfg.state_dir, false, "");
            self.info("   login successful - the login assistant works automatically again next time");
        }
        Ok(())
    }

    pub fn portal_minimize_after_login(&self) {
        if cfg!(mobile) {
            return;
        }
        if self.cfg.portal_window != "normal" {
            self.portal.focus_main();
        }
        match self.cfg.portal_window.as_str() {
            "hidden" => {
                self.portal.hide();
                self.info(format!(
                    "   {BROWSER_LABEL} window hidden (it keeps working and comes back when a login is needed)"
                ));
            }
            "normal" => {}
            _ => {
                self.portal.minimize();
                self.info(format!("   {BROWSER_LABEL} window minimised to the task bar (it keeps working)"));
            }
        }
    }

    fn sel(&self, k: &str) -> String {
        js(&self.cfg.sel[k])
    }

    pub fn assist_login(&mut self, wait: f64) -> RResult<()> {
        if !self.cfg.login_assist || self.assisted {
            return Ok(());
        }
        self.assisted = true;
        self.assist_tried = Instant::now();
        self.login_stage.clear();
        if let Some(d) = login_dirty(&self.cfg.state_dir) {
            self.set_login_stage("dirty");
            self.warn(format!(
                "   login assistant: the last automatic login failed ({}) - this time FetchJ clicks nothing; please log in yourself completely (Ingresar, Ciudadanía, user, password, code)",
                d.get("since").and_then(|v| v.as_str()).unwrap_or("")
            ));
            self.portal.show();
            return Ok(());
        }
        let t_end = Instant::now() + Duration::from_secs_f64(wait);
        let mut t_end = t_end;
        let mut found = false;
        let mut last_goto: Option<Instant> = None;
        loop {
            if self.is_logged_in()? {
                return Ok(());
            }
            if self.unauthorized() && last_goto.map(|t| t.elapsed() > Duration::from_secs(30)).unwrap_or(true) {
                self.warn(
                    "   login assistant: the portal shows its page 'unauthorized' (session ended) - opening the start page of the portal",
                );
                let _ = self.portal.navigate(&self.cfg.portal_url);
                last_goto = Some(Instant::now());
                t_end = t_end.max(Instant::now() + Duration::from_secs(20));
                self.wait_ms(500)?;
                continue;
            }
            if self
                .js_opt(&format!("FJ.anyVisible({})", self.sel("login_button")))?
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                found = true;
                break;
            }
            if Instant::now() >= t_end {
                break;
            }
            self.wait_ms(500)?;
        }
        if !found {
            if !self.login_stage.starts_with('-') {
                self.warn(
                    "   login assistant: 'Ingresar' not found on the portal page yet - it keeps looking while waiting (or log in yourself)",
                );
                self.login_stage = "-".into();
            }
            self.assisted = false;
            return Ok(());
        }
        self.login_stage.clear();
        let r: RResult<()> = (|| {
            self.portal.show();
            self.info("   login assistant: clicking 'Ingresar' ...");
            self.js(&format!("FJ.click({},0,true)", self.sel("login_button")))?;
            self.wait_ms(self.cfg.ui_wait_medium)?;
            if self.cfg.login_user_type != "user_and_password" {
                self.warn(format!(
                    "   login assistant: login type '{}' is done by you - please choose it and log in",
                    self.cfg.login_user_type
                ));
                return Ok(());
            }

            let ciu = self.cfg.sel["login_ciudadania"].clone();
            let mut on_login = false;
            let mut vis = false;
            for _ in 0..40 {
                if self.login_visible(&ciu)? {
                    on_login = true;
                    vis = true;
                    break;
                }
                if crate::bridge::in_scope(&self.portal.url())
                    && self
                        .js_opt(&format!("FJ.anyVisible({})", js(&ciu)))?
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                {
                    vis = true;
                    break;
                }
                self.wait_ms(300)?;
            }
            if !vis {
                self.set_login_stage("mask");
                self.warn("   login assistant: 'Ciudadanía' not found - please choose the login type yourself");
                return Ok(());
            }
            self.info("   login assistant: clicking 'Ciudadanía' ...");
            if on_login {
                self.portal.login_click(&js(&ciu))?;
            } else {
                self.js(&format!("FJ.click({},0,true)", js(&ciu)))?;
            }
            self.login_forward()?;
            Ok(())
        })();
        match r {
            Err(e) if e.is_fatal() => Err(e),
            Err(e) => {
                self.warn(format!("   login assistant stopped (please log in yourself): {}", e.short()));
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }

    fn login_visible(&self, sel: &str) -> RResult<bool> {
        self.check_stop()?;
        match self.portal.login_call(&format!("L.anyVisible({})", js(sel))) {
            Ok(v) => Ok(v.as_bool().unwrap_or(false)),
            Err(crate::bridge::BridgeError::Closed) => Err(RunErr::BrowserClosed),
            Err(_) => Ok(false),
        }
    }

    fn login_wait_for(&self, sel: &str, secs: f64) -> RResult<bool> {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        loop {
            if self.login_visible(sel)? {
                return Ok(true);
            }
            if Instant::now() >= end {
                return Ok(false);
            }
            self.wait_ms(400)?;
        }
    }

    fn login_js(&self, expr: &str) -> RResult<Option<Value>> {
        self.check_stop()?;
        match self.portal.login_call(expr) {
            Ok(v) => Ok(Some(v)),
            Err(crate::bridge::BridgeError::Closed) => Err(RunErr::BrowserClosed),
            Err(_) => Ok(None),
        }
    }

    fn login_forward(&mut self) -> RResult<()> {
        let user = self.cfg.sel["b2c_user"].clone();
        if !self.login_wait_for(&user, 20.0)? {
            self.set_login_stage("mask");
            self.warn("   login assistant: login mask not found - please log in yourself");
            return Ok(());
        }
        let mut filled = false;
        for _ in 0..16 {
            if let Some(a) = self
                .login_js(&format!("L.filled({},{})", self.sel("b2c_user"), self.sel("b2c_password")))?
                .and_then(|v| v.as_array().cloned())
            {
                if a.len() == 2 && a.iter().all(|x| x.as_bool().unwrap_or(false)) {
                    filled = true;
                    break;
                }
            }
            self.wait_ms(500)?;
        }
        if !filled {
            self.set_login_stage("mask");
            self.info("   login assistant: user name / password are not filled in by the browser - please log in yourself (user, password, then the security code)");
            let _ = self.login_js(&format!("L.focus({})", self.sel("b2c_user")));
            self.portal.show();
            return Ok(());
        }
        self.info("   login assistant: user name and password were filled in by the browser - clicking 'Iniciar sesión'");
        self.portal.login_click(&self.sel("b2c_next"))?;
        let m = if self.cfg.validation_method == "email" {
            "email".to_string()
        } else {
            "app".to_string()
        };
        let radio = self.cfg.sel["mfa_radio"].replace("{m}", &m);
        let otp = self.cfg.sel["otp_code"].clone();
        let end = Instant::now() + Duration::from_secs(30);
        let mut found = false;
        while Instant::now() < end {
            if self.login_wait_for(&radio, 1.0)? || self.login_visible(&otp)? {
                found = true;
                break;
            }
            if let Some(t) = self
                .login_js(&format!("L.error({})", self.sel("b2c_error")))?
                .and_then(|v| v.as_str().map(String::from))
            {
                self.info(format!("   login page says: '{t}'"));
                let low = t.to_lowercase();
                let rx_later = regex::Regex::new(r"m[aá]s tarde|problemas para iniciar|bloque|demasiad|intentos").unwrap();
                let rx_pw = regex::Regex::new(r"expir|caduc|venc|cambi|renov|actualiz").unwrap();
                if rx_later.is_match(&low) {
                    self.mark_dirty("later");
                    self.warn(
                        "   login assistant: the portal does not let anyone log in right now - wait a few minutes, then log in yourself",
                    );
                } else if rx_pw.is_match(&low) {
                    self.mark_dirty("pwchange");
                    self.warn("   login assistant: the portal asks for a new password - please change it yourself");
                } else {
                    self.mark_dirty("badpw");
                    self.warn("   login assistant: the login page reports an error (user name or password?) - please log in yourself");
                }
                self.portal.show();
                return Ok(());
            }
            if self.login_visible(&self.cfg.sel["b2c_pwchange"].clone())? {
                self.mark_dirty("pwchange");
                self.warn("   login assistant: the portal asks for a new password - please change it yourself");
                self.portal.show();
                return Ok(());
            }
        }
        if !found {
            self.warn("   login assistant: page for the verification method not found - please continue yourself");
            self.portal.show();
            return Ok(());
        }
        if self.login_visible(&radio)? {
            let how = if m == "email" { "e-mail" } else { "authenticator app" };
            self.info(format!(
                "   login assistant: verification by {how} (validation_method = {m}) - 'Continuar'"
            ));
            if !self
                .login_js(&format!("L.checked({})", js(&radio)))?
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                self.portal.login_click(&js(&radio))?;
                self.wait_ms(300)?;
            }
            self.portal.login_click(&self.sel("mfa_continue"))?;
        }
        if m == "email" {
            if self.login_wait_for(&self.cfg.sel["email_send"].clone(), 20.0)? {
                self.portal.login_click(&self.sel("email_send"))?;
                self.set_login_stage("code_email");
                self.info("   login assistant: code sent to the e-mail address stored in the portal - please type it in, then 'Comprobar código' and 'Continuar'");
                if self.login_wait_for(&self.cfg.sel["email_code"].clone(), 20.0)? {
                    let _ = self.login_js(&format!("L.focus({})", self.sel("email_code")));
                }
                self.portal.show();
            } else {
                self.warn("   login assistant: button 'Enviar código' not found - please continue yourself");
                self.portal.show();
            }
        } else if self.login_wait_for(&otp, 20.0)? {
            self.portal.show();
            let _ = self.login_js(&format!("L.focus({})", js(&otp)));
            self.set_login_stage("code_app");

            if self.totp_login(&otp)? {
                return Ok(());
            }
            self.info("   login assistant: please type the security code of your authenticator app and click 'Verificar'");
        } else {
            self.warn("   login assistant: field for the security code not found - please continue yourself");
            self.portal.show();
        }
        Ok(())
    }

    fn totp_login(&mut self, otp: &str) -> RResult<bool> {
        let dir = self.cfg.state_dir.clone();
        if !crate::totp::stored(&dir) || self.cfg.validation_method != "app" {
            return Ok(false);
        }
        if self.totp_tries >= 2 {
            self.warn("   login assistant: the security code from the TOTP key was not accepted twice - please type it yourself (check the key in the settings)");
            return Ok(false);
        }
        let Some((mut code, valid)) = crate::totp::current(&dir) else {
            self.warn("   login assistant: a TOTP key is stored but cannot be read (other Windows user?) - please type the code yourself");
            return Ok(false);
        };

        if valid < 4 {
            self.sleep(valid as f64 + 0.5)?;
            if let Some((c, _)) = crate::totp::current(&dir) {
                code = c;
            }
        }
        self.totp_tries += 1;
        if !self.portal.login_type(&js(otp), &code)? {
            self.warn("   login assistant: the security code could not be typed into the code field - please type it yourself");
            return Ok(false);
        }
        self.wait_ms(400)?;
        let verify = self.sel("otp_verify");
        let ok = self.portal.login_click(&verify)?;
        self.info(if ok {
            "   login assistant: security code from the stored TOTP key typed in - 'Verificar' clicked"
        } else {
            "   login assistant: security code from the stored TOTP key typed in - button 'Verificar' not found, please click it yourself"
        });
        Ok(ok)
    }

    fn mark_dirty(&mut self, why: &str) {
        self.set_login_stage(why);
        crate::state::set_login_state(&self.cfg.state_dir, true, why);
        self.info("   login assistant: marked - next time FetchJ clicks nothing at the login until you have logged in yourself once");
    }

    pub fn handle_session_dialog(&mut self) -> RResult<()> {
        let Some(d) = self.js_opt("FJ.dialog()")? else { return Ok(()) };
        let t = d.get("text").and_then(|t| t.as_str()).unwrap_or("").to_lowercase();
        if t.is_empty() || !t.contains(&self.cfg.sel["session_dialog_text"].to_lowercase()) {
            return Ok(());
        }

        crate::focus::app_event("portal dialog found: open session on another device - clicking 'Continuar'");
        let label = self.cfg.sel["dialog_continue"].clone();
        let mut hit = self.js(&format!("FJ.dialogButton({})", js(&label)))?.as_bool().unwrap_or(false);
        if !hit {
            hit = self.js("FJ.dialogButton('*last')")?.as_bool().unwrap_or(false);
        }

        let mut gone = false;
        for _ in 0..10 {
            let still = self
                .js_opt("FJ.dialog()")?
                .and_then(|d| d.get("text").and_then(|t| t.as_str()).map(|s| s.to_lowercase()))
                .map(|s| s.contains(&self.cfg.sel["session_dialog_text"].to_lowercase()))
                .unwrap_or(false);
            if !still {
                gone = true;
                break;
            }
            self.wait_ms(200)?;
        }
        if hit && gone {
            self.info("   portal reported an open session on another device - taken over with 'Continuar'");
        } else if !hit && gone {
            self.info("   portal reported an open session on another device - the message was already closed (by the user)");
        } else {
            self.warn("   portal reports an open session on another device - FetchJ could not click 'Continuar'; please click it in the portal window");
        }
        crate::focus::app_event(&format!(
            "portal dialog: FetchJ click hit: {}, dialog gone: {}",
            if hit { "yes" } else { "no" },
            if gone { "yes" } else { "no" }
        ));
        self.wait_ms(self.cfg.ui_wait_short)
    }

    pub fn wait_ms_dialogs(&mut self, ms: u64) -> RResult<()> {
        self.handle_session_dialog()?;
        self.wait_ms(ms)
    }

    pub fn dismiss_page_dialog(&mut self) -> RResult<bool> {
        let Some(d) = self.js_opt("FJ.dialog()")? else { return Ok(false) };
        let t = d.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if t.is_empty() || !t.contains(&self.cfg.sel["conn_problem_text"]) {
            return Ok(false);
        }
        let label = self.cfg.sel["dialog_accept"].clone();
        if !self.js(&format!("FJ.dialogButton({})", js(&label)))?.as_bool().unwrap_or(false) {
            self.js("FJ.dialogButton('*last')")?;
        }
        self.info("   closed the portal message 'Problemas de conexión'");
        self.wait_ms(self.cfg.ui_wait_short)?;
        Ok(true)
    }

    pub fn clear_portal_dialogs(&mut self) -> RResult<()> {
        self.handle_session_dialog()?;
        if self.dismiss_page_dialog()? {
            return Ok(());
        }
        let Some(d) = self.js_opt("FJ.dialog()")? else { return Ok(()) };
        let Some(t) = d.get("text").and_then(|t| t.as_str()) else {
            return Ok(());
        };
        self.warn(format!(
            "   portal message over the page: '{}' - closing it",
            t.chars().take(200).collect::<String>()
        ));
        let mut ok = self.js("FJ.dialogButton('*close')")?.as_bool().unwrap_or(false);
        for l in ["Aceptar", "Cerrar", "OK", "Continuar"] {
            if ok {
                break;
            }
            ok = self.js(&format!("FJ.dialogButton({})", js(l)))?.as_bool().unwrap_or(false);
        }
        self.wait_ms(self.cfg.ui_wait_short)
    }

    pub fn keepalive(&mut self) -> RResult<()> {
        self.check_wake()?;

        self.keep_alive(false)?;

        if self.cfg.entry_fetch_mode == "api" {
            self.fresh_headers(HDR_MAX_AGE_RUN)?;
        }
        Ok(())
    }

    pub fn activity_tick(&mut self) -> RResult<()> {
        let iv = self.cfg.activity_click_interval;
        if iv == 0 || self.last_activity_click.elapsed() < Duration::from_secs(iv) {
            return Ok(());
        }
        self.last_activity_click = Instant::now();
        let next = format!("{}:not(.p-disabled):not([disabled])", self.cfg.sel["paginator_next"]);
        let first = format!("{}:not(.p-disabled):not([disabled])", self.cfg.sel["paginator_first"]);
        let tab = js(&self.cfg.sel["accordion_documents"]);
        let expr = format!(
            "(function(){{var tabs=Array.prototype.slice.call(document.querySelectorAll('p-accordiontab, .p-accordion-tab')).filter(function(t){{return (t.innerText||'').indexOf({tab})>=0}});if(!tabs.length)return false;var t=tabs[0];var b=t.querySelector({n})||t.querySelector({f});if(!b)return false;b.click();return true}})()",
            n = js(&next),
            f = js(&first)
        );
        if self.js_opt(&expr)?.and_then(|v| v.as_bool()).unwrap_or(false) {
            self.wait_ms(self.cfg.ui_wait_short)?;
            self.info("      activity click in the web page (keeps the portal session active)");
        }
        Ok(())
    }

    fn check_wake(&mut self) -> RResult<()> {
        let gap = self.wake_gap.swap(0, Ordering::SeqCst);
        if gap == 0 {
            return Ok(());
        }
        let mins = ((gap as f64) / 60.0).round().max(1.0) as i64;
        crate::portalsess::slept(gap as i64);
        self.warn(format!(
            "The computer was in sleep mode for about {mins} minute(s) - checking the browser and the portal session first"
        ));
        if !self.portal.alive() {
            return Err(RunErr::BrowserClosed);
        }
        if self.is_logged_in()? {
            self.js_opt("FJ.updateToken(true)")?;
            if self.portal_answers()? {
                self.info("   browser and portal session are still valid - the update continues");
                return Ok(());
            }
        }
        self.warn("   the portal session ended during the sleep mode - please log in again in the browser window");
        self.session_lost("sleep");
        self.handle_session_problem(&format!("after the sleep mode of the computer ({mins} min)"), false)
    }

    pub fn ensure_session_fresh(&mut self) -> RResult<()> {
        self.check_wake()?;
        self.update_token(false)?;
        let Some(i) = self.page_info()? else { return Ok(()) };
        let exp = i.hdr_exp;
        if exp > 0 && exp - now_s() < crate::defaults::timing::TOKEN_WAIT_SEC_SHORT && Instant::now() > self.token_wait_skip {
            self.wait_token_renewal(exp)?;
        }
        Ok(())
    }

    pub fn update_token(&mut self, force: bool) -> RResult<()> {
        let v = self.js_opt(&format!("FJ.updateToken({force},{})", self.cfg.token_renew_before_sec))?;
        if let Some(e) = v.and_then(|v| v.as_i64()).filter(|e| *e > 0) {
            self.info(format!(
                "      access token taken over from the browser (valid until {})",
                hhmmss(e)
            ));
            if let Some(l) = crate::portalsess::renewed(e) {
                self.info(l);
            }
        }
        Ok(())
    }

    fn wait_token_renewal(&mut self, exp: i64) -> RResult<()> {
        let left = exp - now_s();
        self.info(format!(
            "      access token runs out in {left} s - waiting for the web page to renew it (no navigation)"
        ));
        while now_s() <= exp + 30 {
            self.wait_ms(2000)?;
            self.update_token(true)?;
            if let Some(i) = self.page_info()? {
                if i.hdr_exp - now_s() > crate::defaults::timing::TOKEN_WAIT_SEC_SHORT {
                    self.info("      the web page has renewed the access token - continuing");
                    return Ok(());
                }
            }
        }
        self.warn("      the web page did not renew the access token - checking the session");
        self.refresh_session()
    }

    pub fn refresh_session(&mut self) -> RResult<()> {
        let old = self.page_info()?.map(|i| i.hdr_exp).unwrap_or(0);
        self.update_token(true)?;
        if self.page_info()?.map(|i| i.hdr_exp).unwrap_or(0) != old {
            return Ok(());
        }
        if !self.is_logged_in()? {
            return self.prompt_relogin("HTTP 401 - the portal does not accept the session any more");
        }
        if self.portal_answers()? {
            self.info("      the portal accepts the session - continuing (the list is not loaded again)");
            self.token_wait_skip = Instant::now() + Duration::from_secs(300);
            return Ok(());
        }
        self.prompt_relogin("the portal does not accept the session any more")
    }

    pub fn prompt_relogin(&mut self, reason: &str) -> RResult<()> {
        if !self.portal.alive() {
            return Err(RunErr::BrowserClosed);
        }
        self.warn(format!("Session problem: {reason}"));
        self.session_lost("");
        if self.cfg.on_session_lost == "abort" {
            return Err(RunErr::SessionLost(reason.into()));
        }

        self.sink.portal_phase("offline");
        let old = self.page_info()?.map(|i| i.hdr_exp).unwrap_or(0);
        loop {
            self.assisted = false;
            self.portal.show();
            self.assist_login(20.0)?;
            self.set_live_alert(Some("session"));
            self.wait_login(5.0, 2.5, Some(old))?;
            self.login_noticed();
            self.handle_session_dialog()?;
            let mut changed = false;
            for _ in 0..10 {
                self.update_token(true)?;
                if self.page_info()?.map(|i| i.hdr_exp != old).unwrap_or(false) {
                    changed = true;
                    break;
                }
                self.wait_ms(1000)?;
            }
            if self.is_logged_in()? && changed {
                break;
            }
            self.warn("   still not logged in (no new session found)");
        }
        self.set_live_alert(None);
        self.sink.portal_phase("online");
        self.info("   logged in again - continuing");
        self.consecutive_errors = 0;
        self.portal_minimize_after_login();
        Ok(())
    }

    pub fn api_call(&mut self, method: &str, url: &str, extra: &Value, body: &str, timeout_s: u64, drop_p: bool) -> RResult<ApiResp> {
        let a = json!({"method": method, "url": url, "headers": extra, "body": body, "timeout": timeout_s * 1000, "dropP": drop_p});
        let id = self.js(&format!("FJ.api({a})"))?;
        let id = id.as_str().unwrap_or("").to_string();
        let end = Instant::now() + Duration::from_secs(timeout_s + 15);
        loop {
            self.wait_ms(100)?;
            let r = match self.js_opt(&format!("FJ.job({})", js(&id)))? {
                Some(r) => r,
                None => {
                    if Instant::now() > end {
                        return Err(RunErr::Other("TimeoutError: the portal page did not answer".into()));
                    }
                    continue;
                }
            };
            if r.get("gone").is_some() {
                return Err(RunErr::Other("the request disappeared in the browser (page reloaded?)".into()));
            }
            if !r["end"].as_bool().unwrap_or(false) {
                if Instant::now() > end {
                    return Err(RunErr::Other(format!("TimeoutError: no answer within {timeout_s} s")));
                }
                continue;
            }
            let headers = r["headers"]
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
                .unwrap_or_default();
            return Ok(ApiResp {
                status: r["status"].as_u64().unwrap_or(0) as u16,
                text: r["text"].as_str().unwrap_or("").to_string(),
                headers,
                dur: r["dur"].as_f64().unwrap_or(0.0),
                err: r["err"].as_str().unwrap_or("").to_string(),
            });
        }
    }

    pub fn portal_answers(&mut self) -> RResult<bool> {
        if self.page_info()?.map(|i| !i.has_hdr).unwrap_or(true) {
            return Ok(false);
        }
        self.update_token(false)?;
        let url = format!("{}/{}", self.cfg.api_base_url, self.cfg.api["api_configuration"]);
        let extra = json!({"p_codigoconfiguracion": "U_TelefonoContactoGestionLinea"});
        match self.api_call("GET", &url, &extra, "", self.cfg.ui_element_timeout, false) {
            Ok(r) if r.status == 200 => {
                let j: Value = serde_json::from_str(&r.text).unwrap_or(Value::Null);
                Ok(j.is_object() && !crate::pyfmt::truthy(j.get("huboError")))
            }
            Ok(_) => Ok(false),
            Err(e) if e.is_fatal() => Err(e),
            Err(_) => Ok(false),
        }
    }

    fn portal_health_check(&mut self) -> RResult<bool> {
        self.info("   check: back to the start page and 'Mis expedientes', then a test call to the portal ...");
        self.dismiss_page_dialog()?;
        self.browser_back()?;
        if let Err(e) = self.go_to_list(true, "check after errors in a row") {
            if e.is_fatal() && !matches!(e, RunErr::SessionLost(_)) {
                return Err(e);
            }
            self.warn(format!("   check: list could not be loaded ({e})"));
            return Ok(false);
        }
        let ok = self.is_logged_in()? && self.portal_answers()?;
        if ok {
            self.info("   check: the portal answers normally and the session is valid - the errors concern single documents, the run continues by itself");
        } else {
            self.warn("   check: the portal does not answer normally");
        }
        Ok(ok)
    }

    pub fn handle_session_problem(&mut self, reason: &str, navigate: bool) -> RResult<()> {
        if !self.portal.alive() {
            return Err(RunErr::BrowserClosed);
        }
        self.warn(format!("Session/connection problem: {reason}"));
        if self.cfg.on_session_lost == "abort" {
            return Err(RunErr::SessionLost(reason.into()));
        }
        if self.is_logged_in()? && self.portal_health_check()? {
            self.consecutive_errors = 0;
            self.set_live_alert(None);
            return Ok(());
        }
        if self.is_logged_in()? {
            self.portal.show();
            self.set_live_alert(Some("errors"));
            loop {
                self.check_stop()?;
                if self.ctl.web_retry.swap(false, Ordering::SeqCst) || true {
                    if self.is_logged_in()? && self.portal_answers()? {
                        self.info("   the portal answers normally again - continuing automatically");
                        break;
                    }
                }
                for _ in 0..60 {
                    if self.ctl.web_retry.load(Ordering::SeqCst) {
                        break;
                    }
                    self.sleep(0.5)?;
                }
            }
        }
        self.ensure_logged_in(false)?;
        self.set_live_alert(None);
        self.consecutive_errors = 0;
        if navigate {
            self.ensure_list(true, "after a new login")?;
        }
        Ok(())
    }

    pub fn logout_portal(&mut self) {
        self.logout_portal_as(false);
    }

    pub fn logout_portal_as(&mut self, force: bool) {
        if !(force || self.cfg.logout_at_end || self.cfg.close_browser_at_end) || self.portal.host() != self.portal_host {
            return;
        }
        self.sink.portal_phase("logout");
        self.ctl.stop.store(false, std::sync::atomic::Ordering::SeqCst);
        let name = js(&self.cfg.sel["logout_text"]);
        let ok = self
            .js(&format!("FJ.clickLink({name}) || FJ.click(\"a:has-text('Salir')\")"))
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if ok {
            let _ = self.wait_ms(self.cfg.ui_wait_medium);
            self.info("Logged out of the portal ('Salir') - no session left open.");
        }
    }
}

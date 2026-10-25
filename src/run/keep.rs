use super::{Fetcher, RResult, RunErr};
use std::time::{Duration, Instant};

pub const MOUSE_GAP: u64 = 240;

#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    Ok,
    Refused(u16),
    Other(String),
}

pub const TECHNIQUE: [&str; 8] = [
    "off",
    "mouse/keyboard events",
    "light server call",
    "the page's own case-list call again",
    "paging in 'Mis expedientes'",
    "page change by clicks",
    "load 'Mis expedientes' again",
    "query a single case (the newest)",
];

pub fn list_page_url(portal_url: &str, list_part: &str) -> String {
    match portal_url.find("/inicio/") {
        Some(i) => format!("{}consultas/{}", &portal_url[..i + "/inicio/".len()], list_part),
        None => format!("https://pjenlinea.poder-judicial.go.cr/GestionEnLinea/inicio/consultas/{list_part}"),
    }
}

pub fn check_text(c: &Check) -> String {
    match c {
        Check::Ok => "the portal accepts the session (HTTP 200)".to_string(),
        Check::Refused(s) => format!("the portal refuses the session (HTTP {s})"),
        Check::Other(m) => format!("no normal answer ({m})"),
    }
}

impl Fetcher {
    pub fn keep_alive(&mut self, online: bool) -> RResult<()> {
        let tech = self.cfg.keep_alive_technique.min(7);
        let every = Duration::from_secs(self.cfg.keep_alive_timer_min.max(1) * 60);
        crate::portalsess::keep_setting(tech, self.cfg.keep_alive_timer_min.max(1));
        if tech == 0 {
            return Ok(());
        }

        self.mouse_guard()?;
        if self.last_ui_action.elapsed() < every {
            return Ok(());
        }
        self.last_ui_action = Instant::now();

        let t = if !online && (4..=6).contains(&tech) { 1 } else { tech };

        if matches!(t, 2 | 3 | 7) {
            if let Some(a) = self.hdr_age()?.filter(|a| *a < every.as_secs()) {
                self.ka_log(
                    tech,
                    &format!("skipped - the page itself called the portal {a} s ago (counts for the server)"),
                    false,
                );
                return Ok(());
            }
        } else {
            let idle = self.js_opt("FJ.userIdle()")?.and_then(|v| v.as_i64()).unwrap_or(-1);
            if idle >= 0 && (idle as u64) < every.as_secs() {
                self.ka_log(
                    tech,
                    &format!("skipped - the user worked in the portal window {idle} s ago (counts as activity)"),
                    false,
                );
                return Ok(());
            }
        }
        let t0 = Instant::now();
        let mut warn = false;
        let res = match t {
            1 => {
                if self.js_opt("FJ.mouse()")?.and_then(|v| v.as_bool()).unwrap_or(false) {
                    "mouse/keyboard events sent".to_string()
                } else {
                    "the portal page did not answer".to_string()
                }
            }
            2 => {
                let c = self.light_call()?;
                warn = c != Check::Ok;
                check_text(&c)
            }
            3 => {
                let (c, what) = self.real_call()?;
                warn = c != Check::Ok;
                format!("{} ({what})", check_text(&c))
            }
            4 => self.ka_page_list()?,
            5 => self.ka_page_change()?,
            6 => self.ka_reload_list()?,
            _ => self.ka_single_case()?,
        };
        let res = if t != tech {
            format!("{res} - technique {tech} is not used during an update, technique 1 instead")
        } else {
            res
        };
        self.ka_log(t, &format!("{res} ({:.1} s)", t0.elapsed().as_secs_f64()), warn);
        Ok(())
    }

    fn ka_log(&mut self, t: u8, res: &str, warn: bool) {
        let mins = crate::portalsess::minutes_logged_in()
            .map(|m| m.to_string())
            .unwrap_or_else(|| "?".into());
        let tok = self
            .page_info()
            .ok()
            .flatten()
            .map(|i| i.hdr_exp)
            .filter(|e| *e > 0)
            .map(crate::portalsess::hms)
            .unwrap_or_else(|| "-".into());
        crate::portalsess::keep_event(&format!("technique {t}: {res}"));
        let line = format!(
            "   keep-alive (technique {t} - {}): {res} - logged in for {mins} min, access token valid until {tok}",
            TECHNIQUE[t as usize]
        );
        if warn {
            self.warn(line);
        } else if t != 1 || self.ka_logged.map(|x| x.elapsed() >= Duration::from_secs(600)).unwrap_or(true) {
            self.ka_logged = Some(Instant::now());
            self.info(line);
        }
    }

    fn mouse_guard(&mut self) -> RResult<()> {
        if self
            .mouse_sent
            .map(|t| t.elapsed() < Duration::from_secs(MOUSE_GAP))
            .unwrap_or(false)
        {
            return Ok(());
        }
        let idle = self.js_opt("FJ.userIdle()")?.and_then(|v| v.as_i64()).unwrap_or(-1);
        if idle >= 0 && (idle as u64) < MOUSE_GAP {
            return Ok(());
        }
        let ok = self.js_opt("FJ.mouse()")?.and_then(|v| v.as_bool()).unwrap_or(false);
        self.mouse_sent = Some(Instant::now());
        if !ok {
            crate::portalsess::keep_event("mouse/keyboard events: the portal page did not answer");
            self.warn("   keep-alive: mouse/keyboard events - the portal page did not answer");
        } else if self.mouse_logged.map(|t| t.elapsed() >= Duration::from_secs(600)).unwrap_or(true) {
            self.mouse_logged = Some(Instant::now());
            crate::portalsess::keep_event("mouse/keyboard events sent");
            self.info(crate::i18n::trf(
                "   keep-alive: mouse/keyboard events in the portal window (at the latest {} s after the last activity, against the portal's inactivity logout)",
                &[&MOUSE_GAP],
            ));
        }
        Ok(())
    }

    pub fn hdr_age(&self) -> RResult<Option<u64>> {
        let t = self.js_opt("FJ.hdrT")?.and_then(|v| v.as_f64()).unwrap_or(0.0);
        if t <= 0.0 {
            return Ok(None);
        }
        Ok(Some(((chrono::Utc::now().timestamp_millis() as f64 - t) / 1000.0).max(0.0) as u64))
    }

    pub fn fresh_headers(&mut self, max_age: u64) -> RResult<bool> {
        let Some(age) = self.hdr_age()? else { return Ok(false) };
        if age < max_age || self.url_has("url_detail_part") || !self.is_logged_in()? {
            return Ok(false);
        }

        let keep = (!self.carpetas.is_empty()).then(|| self.carpetas.clone());
        let r = self.go_to_list(true, "fresh session headers");
        if let Some(k) = keep {
            self.carpetas = k;
        }
        r?;
        let new = self.hdr_age()?.unwrap_or(age);
        let fresh = new < age;
        let what = if fresh {
            "fresh headers taken over"
        } else {
            "no new call of the page seen"
        };
        crate::portalsess::keep_event(&format!(
            "page loaded 'Mis expedientes' itself (headers were {} min old): {what}",
            age / 60
        ));
        self.info(crate::i18n::trf(
            "   session headers of the page were {} min old - the page loaded 'Mis expedientes' itself again ({})",
            &[&(age / 60), &crate::i18n::tr(what)],
        ));
        Ok(fresh)
    }

    pub fn recheck_with_fresh_headers(&mut self) -> RResult<bool> {
        if !self.is_logged_in()? {
            return Ok(false);
        }
        match self.fresh_headers(0) {
            Ok(_) => {}

            Err(RunErr::SessionLost(_)) => return Ok(false),
            Err(e) if e.is_fatal() => return Err(e),
            Err(_) => return Ok(false),
        }
        let c = self.online_check(true)?;
        if c == Some(Check::Ok) {
            self.info("   with the fresh headers of the page the portal accepts the session - no new login needed");
            crate::portalsess::keep_event("server check with fresh headers: accepted - it was only the old headers");
            return Ok(true);
        }
        Ok(false)
    }

    pub fn end_portal_login(&mut self) -> RResult<()> {
        if !self.is_logged_in()? {
            return Ok(());
        }
        let name = crate::bridge::js(&self.cfg.sel["logout_text"]);
        let ok = self
            .js_opt(&format!("FJ.clickLink({name}) || FJ.click(\"a:has-text('Salir')\")"))?
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.wait_ms(self.cfg.ui_wait_long)?;
        if ok {
            self.info("   'Salir' clicked - the portal page shows the logged-out state for the new login");
        } else {
            self.warn("   'Salir' not found - opening the start page of the portal for the new login");
            let _ = self.portal.navigate(&self.cfg.portal_url);
            self.wait_ms(self.cfg.ui_wait_long)?;
        }
        Ok(())
    }

    pub fn light_call(&mut self) -> RResult<Check> {
        if self.page_info()?.map(|i| !i.has_hdr).unwrap_or(true) {
            return Ok(Check::Other("the page has not made its own call yet".into()));
        }
        self.update_token(false)?;
        let url = format!("{}/{}", self.cfg.api_base_url, self.cfg.api["api_configuration"]);
        let extra = serde_json::json!({"p_codigoconfiguracion": self.cfg.api["config_code_contact_phone"]});
        let r = self.api_call("GET", &url, &extra, "", self.cfg.ui_element_timeout, false);
        self.classify(r)
    }

    pub fn real_call(&mut self) -> RResult<(Check, String)> {
        self.update_token(false)?;
        let Some(ll) = self.js_opt("FJ.lastList")?.filter(|v| v.is_object()) else {
            return Ok((self.light_call()?, "no case-list call of the page yet - light call instead".into()));
        };
        let url = ll.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let body = ll.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if url.is_empty() {
            return Ok((self.light_call()?, "no case-list call of the page yet - light call instead".into()));
        }
        let r = self.api_call("POST", &url, &serde_json::json!({}), &body, self.cfg.case_open_timeout, true);
        Ok((self.classify(r)?, "case list".into()))
    }

    fn classify(&self, r: RResult<super::ApiResp>) -> RResult<Check> {
        Ok(match r {
            Ok(r) if r.status == 200 => {
                let j: serde_json::Value = serde_json::from_str(&r.text).unwrap_or(serde_json::Value::Null);
                if j.is_object() && !crate::pyfmt::truthy(j.get("huboError")) {
                    crate::portalsess::server_ok();
                    Check::Ok
                } else {
                    Check::Other("the answer is not data".into())
                }
            }
            Ok(r) if r.status == 401 || r.status == 403 => Check::Refused(r.status),
            Ok(r) => Check::Other(format!("HTTP {}", r.status)),
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => Check::Other(e.short()),
        })
    }

    fn ka_page_list(&mut self) -> RResult<String> {
        if !self.url_has("url_list_part") || self.url_has("url_detail_part") {
            return Ok("not on the list page - nothing paged".into());
        }
        Ok(
            match self
                .js_opt("FJ.pageList()")?
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default()
                .as_str()
            {
                "next" => "paged one page further in the list".into(),
                "first" => "paged back to the first page of the list".into(),
                "one_page" => "the list has only one page - nothing to page".into(),
                _ => "no paginator on the page".into(),
            },
        )
    }

    fn ka_page_change(&mut self) -> RResult<String> {
        self.go_to_list(true, "keep-alive: page change")?;
        Ok(format!("opened 'Mis expedientes' (page: {})", self.short_url()))
    }

    fn ka_reload_list(&mut self) -> RResult<String> {
        let url = list_page_url(&self.cfg.portal_url, &self.cfg.sel["url_list_part"]);
        self.portal.navigate(&url)?;
        self.wait_ms(self.cfg.ui_wait_long)?;
        if self.is_logged_in()? {
            let _ = self.go_to_list(false, "keep-alive: list reloaded");
            Ok(format!("loaded the page 'Mis expedientes' new (page: {})", self.short_url()))
        } else {
            Ok(format!(
                "loaded the page 'Mis expedientes' new - not logged in afterwards (page: {})",
                self.short_url()
            ))
        }
    }

    fn ka_single_case(&mut self) -> RResult<String> {
        if self.ka_case.is_none() {
            if self.carpetas.is_empty() {
                self.go_to_list(true, "keep-alive: single case - learning the cases once")?;
            }
            self.ka_case = self.newest_carpeta();
        }
        let Some(c) = self.ka_case.clone() else {
            return Ok("no case known yet - nothing asked".into());
        };
        let (num, _, _, _) = crate::run::list::carpeta_ident(&c);
        let exp = c.get("expediente").filter(|e| e.is_object()).cloned();
        let (key, body) = if let Some(e) = exp {
            let mut b = c.as_object().cloned().unwrap_or_default();
            let mut em = e.as_object().cloned().unwrap_or_default();
            em.remove("intervenciones");
            b.insert("expediente".into(), serde_json::Value::Object(em));
            ("api_q_general_legajo", serde_json::Value::Object(b))
        } else {
            let mut b = c.as_object().cloned().unwrap_or_default();
            b.remove("intervenciones");
            ("api_q_general_case", serde_json::Value::Object(b))
        };
        self.update_token(false)?;
        let url = format!("{}/{}", self.cfg.api_base_url, self.cfg.api[key]);
        let text = serde_json::to_string(&crate::run::entry::clean(&body)).unwrap_or_default();
        let r = self.api_call("POST", &url, &serde_json::json!({}), &text, self.cfg.case_open_timeout, true);
        let c = self.classify(r)?;
        Ok(format!("{} (case {num})", check_text(&c)))
    }

    fn newest_carpeta(&self) -> Option<serde_json::Value> {
        let rank = |c: &serde_json::Value| -> (u32, u32) {
            let (n, _, _, _) = crate::run::list::carpeta_ident(c);
            let mut it = n.split('-');
            let y = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            let num = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            (y, num)
        };
        self.carpetas
            .iter()
            .filter(|c| !crate::run::list::carpeta_ident(c).0.is_empty())
            .max_by_key(|c| rank(c))
            .cloned()
    }

    pub fn online_check(&mut self, forced: bool) -> RResult<Option<Check>> {
        let every = self.cfg.online_check_min;
        if !forced {
            if every == 0 || self.check_last.elapsed() < Duration::from_secs(every * 60) {
                return Ok(None);
            }
        }
        self.check_last = Instant::now();
        let (c, what) = self.real_call()?;
        let ok = c == Check::Ok;
        let text = format!("{} ({what})", check_text(&c));
        crate::portalsess::keep_event(&format!("server check: {text}"));
        self.info(format!("   portal check: {text}"));
        self.ctl
            .check_result
            .lock()
            .map(|mut m| *m = Some((crate::portalsess::now(), ok, text)))
            .ok();
        Ok(Some(c))
    }
}

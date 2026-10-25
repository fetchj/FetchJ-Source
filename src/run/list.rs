use super::{Card, Fetcher, RResult, RunErr};
use crate::bridge::js;
use crate::text::{fmt_case_number, norm_txt};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

pub fn carpeta_ident(c: &Value) -> (String, String, String, String) {
    let exp = c.get("expediente").filter(|e| e.is_object());
    let kind = if exp.is_some() { "legajo" } else { "expediente" };
    let src = exp.unwrap_or(c);
    let raw = {
        let a = crate::pyfmt::py_or_empty(src.get("numeroExpedienteConFormato"));
        if a.is_empty() {
            crate::pyfmt::py_or_empty(src.get("numero"))
        } else {
            a
        }
    };
    let number = fmt_case_number(&raw);
    let fe: String = crate::pyfmt::py_or_empty(c.get("fechaEntrada")).chars().take(10).collect();
    let date = if regex::Regex::new(r"^\d{4}-\d{2}-\d{2}").unwrap().is_match(&fe) {
        format!("{}/{}/{}", &fe[8..10], &fe[5..7], &fe[0..4])
    } else {
        String::new()
    };
    let ctx = c.get("contexto").cloned().unwrap_or(Value::Null);
    let o1 = crate::pyfmt::py_or_empty(ctx.get("descripcion"));
    let office = if o1.is_empty() {
        crate::pyfmt::py_or_empty(ctx.get("oficina").and_then(|o| o.get("descripcion")))
    } else {
        o1
    };
    (number, kind.into(), date, office)
}

pub fn dup_key(c: &Value) -> String {
    let (n, k, d, o) = carpeta_ident(c);
    let s = |v: Option<&Value>| crate::pyfmt::py_str(v);
    format!(
        "{n}|{k}|{d}|{o}|{}|{}|{}",
        s(c.get("carpeta")),
        s(c.get("codigo")),
        s(c.get("contexto").and_then(|x| x.get("codigo")))
    )
}

pub fn parse_card(text: &str, link_text: &str) -> Card {
    let lines: Vec<String> = text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    let mut c = Card {
        kind: if text.contains("Datos del legajo") {
            "legajo".into()
        } else {
            "expediente".into()
        },
        raw: lines.join(" | "),
        ..Default::default()
    };
    let mut number = String::new();
    for (i, ln) in lines.iter().enumerate() {
        let Some(next) = lines.get(i + 1) else { continue };
        match ln.as_str() {
            "Fecha entrada:" => c.date = next.clone(),
            "Oficina judicial:" => c.office = next.clone(),
            "Clase:" => c.clase = next.clone(),
            "Asunto:" => c.asunto = next.clone(),
            "Clase asunto:" => c.clase_asunto = next.clone(),
            "Expediente:" => number = next.clone(),
            _ => {}
        }
    }
    c.number = fmt_case_number(if number.is_empty() { link_text } else { &number });
    c.class_text = if c.kind == "legajo" {
        [c.asunto.as_str(), c.clase_asunto.as_str()]
            .iter()
            .filter(|x| !x.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" / ")
    } else {
        c.clase.clone()
    };
    c
}

fn sort_canon(v: &Value) -> String {
    fn s(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut k: Vec<&String> = m.keys().collect();
                k.sort();
                Value::Object(k.into_iter().map(|k| (k.clone(), s(&m[k]))).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(s).collect()),
            x => x.clone(),
        }
    }
    s(v).to_string()
}

impl Fetcher {
    fn sjs(&self, k: &str) -> String {
        js(&self.cfg.sel[k])
    }

    pub fn case_link_count(&self) -> RResult<usize> {
        Ok(self.js(&format!("FJ.count({})", self.sjs("case_link")))?.as_u64().unwrap_or(0) as usize)
    }

    pub fn url_has(&self, k: &str) -> bool {
        self.portal.url().contains(&self.cfg.sel[k])
    }

    pub fn go_to_list(&mut self, force_reload: bool, reason: &str) -> RResult<()> {
        let tmo = self.cfg.case_open_timeout;
        if self.portal.host() != self.portal_host || !self.is_logged_in()? {
            return Err(RunErr::SessionLost(format!("Not logged in (page: {})", self.short_url())));
        }
        self.clear_portal_dialogs()?;
        if !force_reload && self.url_has("url_list_part") && !self.url_has("url_detail_part") && self.case_link_count()? > 0 {
            return Ok(());
        }
        if self.url_has("url_detail_part") {
            let clicked = self
                .js(&format!("FJ.click({})", self.sjs("back_button")))?
                .as_bool()
                .unwrap_or(false);
            if clicked && self.wait_list(true)? {
                self.last_ui_action = Instant::now();
                if !force_reload && self.case_link_count()? > 0 {
                    return Ok(());
                }
            } else {
                self.browser_back()?;
                if !force_reload && !self.url_has("url_detail_part") && self.wait_list(true)? {
                    return Ok(());
                }
            }
        }
        let reason = if !reason.is_empty() {
            reason.to_string()
        } else if force_reload {
            "reload".into()
        } else {
            "list not shown".into()
        };
        self.info(format!(
            "   navigating to 'Mis expedientes' (from {}) - reason: {}",
            self.short_url(),
            crate::i18n::tr(&reason)
        ));
        let t_list0 = Instant::now();
        if self.url_has("url_list_part") {
            let mut ok = self.js(&format!("FJ.click({})", self.sjs("home_link")))?.as_bool().unwrap_or(false);
            if !ok {
                ok = self
                    .js(&format!("FJ.clickLink({})", self.sjs("home_text")))?
                    .as_bool()
                    .unwrap_or(false);
            }
            if !ok {
                return Err(RunErr::Other(format!("TimeoutError: '{}' not found", self.cfg.sel["home_text"])));
            }
            let end = Instant::now()
                + Duration::from_secs_f64((self.cfg.ui_wait_medium as f64 / 1000.0).max(0.5) + self.cfg.ui_element_timeout as f64);
            while Instant::now() < end {
                let v = self.js_opt(&format!(
                    "FJ.anyVisible({}) || FJ.anyVisible({})",
                    self.sjs("list_link"),
                    self.sjs("consultas_menu")
                ))?;
                if v.and_then(|v| v.as_bool()).unwrap_or(false) {
                    break;
                }
                self.wait_ms_dialogs(150)?;
            }
        }

        let ready = format!(
            "FJ.anyVisible({}) || FJ.textCount({})>0 || FJ.anyVisible({})",
            self.sjs("list_link"),
            self.sjs("list_text"),
            self.sjs("consultas_menu")
        );
        let end = Instant::now() + Duration::from_secs(20).max(Duration::from_secs(self.cfg.ui_element_timeout as u64));
        let mut waited = false;
        loop {
            if self.js_opt(&ready)?.and_then(|v| v.as_bool()).unwrap_or(false) {
                break;
            }
            if Instant::now() >= end {
                break;
            }
            waited = true;
            self.wait_ms_dialogs(300)?;
        }
        if waited {
            let secs = format!("{:.1}", t_list0.elapsed().as_secs_f64());
            self.info(format!(
                "   waited {} s for the portal start page (page: {})",
                secs,
                self.short_url()
            ));
        }
        self.js("(FJ.list={collect:true,resps:[],reqSeen:0,legReqSeen:0,failed:'',legRespT:0},true)")?;
        let mut tile = self
            .js(&format!("FJ.anyVisible({})", self.sjs("list_link")))?
            .as_bool()
            .unwrap_or(false);
        let mut by_text = false;
        if !tile {
            by_text = self
                .js(&format!("FJ.textCount({})>0", self.sjs("list_text")))?
                .as_bool()
                .unwrap_or(false);
            tile = by_text;
        }
        if !tile {
            self.info("   'Mis expedientes' not on the start page - going through 'Consulta privada'");
            if !self
                .js(&format!("FJ.click({},0,true)", self.sjs("consultas_menu")))?
                .as_bool()
                .unwrap_or(false)
            {
                return Err(RunErr::CaseOpenFailed {
                    msg: "neither 'Mis expedientes' nor 'Consulta privada' found on the page".into(),
                    broken: false,
                });
            }

            for _ in 0..50 {
                self.wait_ms_dialogs(300)?;
                if self
                    .js_opt(&format!("FJ.anyVisible({})", self.sjs("list_link")))?
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    tile = true;
                    break;
                }
            }
            if !tile {
                return Err(RunErr::CaseOpenFailed {
                    msg: "'Mis expedientes' not found on the page 'Consulta privada'".into(),
                    broken: false,
                });
            }
        }
        if by_text {
            self.js(&format!("FJ.clickText({},true,true)", self.sjs("list_text")))?;
        } else {
            self.js(&format!("FJ.click({},0,true)", self.sjs("list_link")))?;
        }
        let t_click = Instant::now();
        let (resp, t_answer) = self.wait_list_answer()?;
        if let Some(text) = resp {
            match serde_json::from_str::<Value>(&text) {
                Ok(j) => {
                    if let Some(a) = j.get("valorDevuelto").and_then(|v| v.as_array()).filter(|a| !a.is_empty()) {
                        self.portal_list = a.clone();
                    }
                    self.save_raw("00_list_expedientes", &j);
                }
                Err(_) => self.warn("   list data not captured (JSONDecodeError) - continuing"),
            }
        }
        let end = Instant::now() + Duration::from_millis(self.cfg.ui_wait_medium.max(500));
        while Instant::now() < end && self.case_link_count()? == 0 {
            self.wait_ms_dialogs(100)?;
        }
        if self.case_link_count()? == 0 {
            self.clear_portal_dialogs()?;
            if self
                .js(&format!("FJ.click({})", self.sjs("search_button")))?
                .as_bool()
                .unwrap_or(false)
            {
                self.wait_ms(self.cfg.ui_wait_long)?;
            }
        }
        let _ = tmo;
        if !self.wait_list(true)? {
            if self.portal.host() != self.portal_host || !self.is_logged_in()? {
                return Err(RunErr::SessionLost(format!(
                    "Logged out while opening the list (page: {})",
                    self.short_url()
                )));
            }
            return Err(RunErr::CaseOpenFailed {
                msg: format!("Case list did not load within {}s", self.cfg.list_timeout),
                broken: false,
            });
        }
        self.last_ui_action = Instant::now();
        let t_rows = t_click.elapsed().as_secs_f64();
        let leg = self.cfg.api["api_legajo_list"].clone();
        if self.carpetas.is_empty() {
            let mut t_end = t_click + Duration::from_secs_f64(t_answer.unwrap_or(0.0) + 4.0);
            loop {
                let st = self.js("({n:FJ.list.resps.filter(function(r){return r.legajo}).length, seen:FJ.list.legReqSeen?(Date.now()-FJ.list.legReqSeen)/1000:-1})")?;
                if st["n"].as_u64().unwrap_or(0) > 0 {
                    break;
                }
                let seen = st["seen"].as_f64().unwrap_or(-1.0);
                if seen >= 0.0 {
                    let since_req = Instant::now() - Duration::from_secs_f64(seen);
                    t_end = t_end.max(since_req + Duration::from_secs(self.cfg.list_timeout));
                }
                if Instant::now() >= t_end {
                    break;
                }
                self.wait_ms(250)?;
            }
        }
        let t_list1 = t_list0.elapsed().as_secs_f64();
        let resps = self.js("(function(){var r=FJ.list.resps.map(function(x){return {url:x.url,status:x.status,text:x.text,legajo:x.legajo,t:x.t}});FJ.list.collect=false;return {r:r,legT:FJ.list.legRespT}})()")?;
        let arr = resps["r"].as_array().cloned().unwrap_or_default();
        let leg_any = arr.iter().any(|r| r["url"].as_str().unwrap_or("").contains(&leg));
        if leg_any {
            let leg_t = resps["legT"].as_f64().unwrap_or(0.0);
            let click_ms = chrono::Utc::now().timestamp_millis() as f64 - t_click.elapsed().as_millis() as f64;
            let after = if leg_t > 0.0 { (leg_t - click_ms) / 1000.0 } else { t_list1 };
            self.info(format!("   list times: rows shown after {t_rows:.1} s, legajos after {after:.1} s"));
        } else {
            self.info(format!("   list times: rows shown after {t_rows:.1} s, legajos not received"));
        }
        self.build_carpetas(&arr);
        crate::state::save_case_list(&self.cfg.protocol_dir, &self.portal_list);
        crate::state::save_entry_list(&self.cfg.protocol_dir, &self.carpetas);
        self.note_list_load(&reason, t_list1, t_answer);
        Ok(())
    }

    fn wait_list_answer(&mut self) -> RResult<(Option<String>, Option<f64>)> {
        let t0 = Instant::now();
        let case = self.cfg.api["api_case_list"].clone();
        let mut n = 0u32;
        loop {
            let st = self.js_opt(&format!(
                "(function(){{var r=FJ.list.resps.filter(function(x){{return x.url.indexOf({c})>=0}})[0];return {{r:r?{{status:r.status,text:r.text}}:null,failed:FJ.list.failed,seen:FJ.list.reqSeen}}}})()",
                c = js(&case)
            ))?;
            if let Some(st) = st {
                if let Some(r) = st.get("r").filter(|r| !r.is_null()) {
                    let status = r["status"].as_u64().unwrap_or(0);
                    if status != 200 {
                        return Err(RunErr::CaseOpenFailed {
                            msg: format!("the portal answered the list with HTTP {status}"),
                            broken: false,
                        });
                    }
                    return Ok((r["text"].as_str().map(String::from), Some(t0.elapsed().as_secs_f64())));
                }
                if let Some(f) = st["failed"].as_str().filter(|f| !f.is_empty()) {
                    return Err(RunErr::CaseOpenFailed {
                        msg: format!("the list request got no answer ({f})"),
                        broken: false,
                    });
                }
                let waited = t0.elapsed().as_secs_f64();
                if st["seen"].as_i64().unwrap_or(0) == 0 && waited > self.cfg.ui_element_timeout as f64 {
                    return Ok((None, None));
                }
                if waited > self.cfg.list_timeout as f64 {
                    return Err(RunErr::CaseOpenFailed {
                        msg: format!("the portal did not answer the list within {}s", self.cfg.list_timeout),
                        broken: false,
                    });
                }
            }
            n += 1;
            if n % 10 == 0 && !self.is_logged_in()? {
                return Err(RunErr::SessionLost(format!(
                    "Logged out while opening the list (page: {})",
                    self.short_url()
                )));
            }
            if n % 40 == 0 {
                self.info(format!(
                    "   still waiting for the list of the portal ({:.0} s, at most {} s)",
                    t0.elapsed().as_secs_f64(),
                    self.cfg.list_timeout
                ));
            }

            self.wait_ms_dialogs(500)?;
        }
    }

    pub fn wait_list(&mut self, long: bool) -> RResult<bool> {
        let end = Instant::now() + Duration::from_secs(self.cfg.ui_element_timeout);
        while Instant::now() < end {
            if self
                .js_opt(&format!("FJ.anyVisible({})", self.sjs("case_link")))?
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return Ok(true);
            }
            self.wait_ms(200)?;
        }
        if !long {
            return Ok(false);
        }
        let end = Instant::now() + Duration::from_secs(self.cfg.list_timeout.saturating_sub(self.cfg.ui_element_timeout));
        let mut n = 0;
        while Instant::now() < end {
            if self.case_link_count().unwrap_or(0) > 0 {
                return Ok(true);
            }
            let collecting = self.js_opt("FJ.list.collect")?.and_then(|v| v.as_bool()).unwrap_or(false);
            if !self.url_has("url_list_part") && !collecting {
                return Ok(false);
            }
            n += 1;
            if n % 5 == 0 && !self.is_logged_in()? {
                return Ok(false);
            }
            self.wait_ms(1000)?;
        }
        Ok(false)
    }

    pub fn ensure_list(&mut self, force_reload: bool, reason: &str) -> RResult<()> {
        let mut last = String::new();
        for attempt in 1..=self.cfg.case_open_retries + 1 {
            let r = if attempt == 1 {
                reason.to_string()
            } else {
                "list did not load - next attempt".into()
            };
            match self.go_to_list(force_reload || attempt > 1, &r) {
                Ok(()) => return Ok(()),
                Err(RunErr::SessionLost(m)) => {
                    last = m.clone();
                    self.handle_session_problem(&m, false)?;
                }
                Err(RunErr::CaseOpenFailed { msg, .. }) => {
                    last = msg.clone();
                    self.warn(format!("   list attempt {attempt} failed: {msg}"));
                    self.note_error(&msg)?;
                }
                Err(e) if e.is_fatal() => return Err(e),
                Err(e) => {
                    let s = e.short();
                    last = s.clone();
                    self.warn(format!("   list attempt {attempt} failed: {s}"));
                    self.note_error(&s)?;
                    self.clear_portal_dialogs()?;
                }
            }
        }
        Err(RunErr::SessionLost(format!("Case list could not be opened: {last}")))
    }

    fn build_carpetas(&mut self, resps: &[Value]) {
        let leg = self.cfg.api["api_legajo_list"].clone();
        let mut found = Vec::new();
        let mut is_leg = Vec::new();
        let mut legajos = Vec::new();
        let mut seen = BTreeSet::new();
        for r in resps {
            let Ok(j) = serde_json::from_str::<Value>(r["text"].as_str().unwrap_or("")) else {
                continue;
            };
            let Some(vals) = j.get("valorDevuelto").and_then(|v| v.as_array()) else {
                continue;
            };
            if !seen.insert(sort_canon(&Value::Array(vals.clone()))) {
                continue;
            }
            let l = r["url"].as_str().unwrap_or("").contains(&leg);
            for v in vals.iter().filter(|v| v.is_object()) {
                found.push(v.clone());
                is_leg.push(l);
                if l {
                    legajos.push(v.clone());
                }
            }
        }
        if !legajos.is_empty() {
            self.save_raw("00_list_legajos", &json!({"valorDevuelto": legajos}));
        }
        if !found.is_empty() {
            let first = self.carpetas.is_empty();
            let n = found.len();
            self.carpetas = found;
            self.carpeta_legajo = is_leg;
            self.carpeta_used.clear();
            if first && self.cfg.entry_fetch_mode == "api" {
                self.info(format!(
                    "Direct queries active: {n} entries of the portal list ({} legajos).",
                    legajos.len()
                ));
            }
        }
    }

    fn note_list_load(&mut self, reason: &str, secs: f64, answer: Option<f64>) {
        self.list_loads.push((reason.to_string(), secs, answer));
        let n = if self.carpetas.is_empty() {
            self.portal_list.len()
        } else {
            self.carpetas.len()
        };
        match answer {
            Some(a) => self.info(format!(
                "   list 'Mis expedientes' loaded (reason: {}): {secs:.1} s, portal answer after {a:.1} s, {n} entries",
                crate::i18n::tr(reason)
            )),
            None => self.info(format!(
                "   list 'Mis expedientes' loaded (reason: {}): {secs:.1} s, {n} entries",
                crate::i18n::tr(reason)
            )),
        }
        crate::state::note_list_time(&self.cfg.state_dir, &self.run_id, reason, secs, answer, n);
    }

    pub fn total_pages(&mut self) -> RResult<usize> {
        let t = self.js(&format!("FJ.text({})", self.sjs("paginator_current")))?;
        let t = t.as_str().unwrap_or("");
        let rx1 = regex::Regex::new(r"de\s+(\d+)").unwrap();
        let rx2 = regex::Regex::new(r"(\d+)\s*$").unwrap();
        let m = rx1.captures(t).or_else(|| rx2.captures(t));
        Ok(m.and_then(|c| c[1].parse().ok()).unwrap_or(1))
    }

    fn current_page(&self) -> RResult<usize> {
        let t = self.js(&format!("FJ.text({})", self.sjs("paginator_active_page")))?;
        Ok(t.as_str().and_then(|s| s.trim().parse().ok()).unwrap_or(1))
    }

    fn wait_page(&self, n: usize) -> RResult<bool> {
        let end = Instant::now() + Duration::from_secs(self.cfg.ui_element_timeout);
        while Instant::now() < end {
            if self.current_page()? == n {
                return Ok(true);
            }
            self.wait_ms(100)?;
        }
        Ok(false)
    }

    fn rows_signature(&self) -> String {
        self.js_opt(&format!(
            "FJ.cards({}).map(function(c){{return c.card}}).join('\\n||\\n')",
            self.sjs("case_link")
        ))
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
    }

    fn wait_rows_changed(&self, old: &str) -> RResult<bool> {
        let end = Instant::now() + Duration::from_secs(self.cfg.ui_element_timeout);
        while Instant::now() < end {
            let t = self.rows_signature();
            if !t.is_empty() && t != old {
                return Ok(true);
            }
            self.wait_ms(100)?;
        }
        Ok(false)
    }

    fn click_page_number(&self, n: usize) -> RResult<bool> {
        let expr = format!(
            "(function(){{var b=FJ.$$({}).filter(function(e){{return /^\\s*{n}\\s*$/.test(e.innerText||'')}})[0];if(!b)return false;b.click();return true}})()",
            self.sjs("paginator_page")
        );
        Ok(self.js(&expr)?.as_bool().unwrap_or(false))
    }

    pub fn go_to_page(&mut self, page_no: usize) -> RResult<()> {
        let mut cur = self.current_page()?;
        if cur == page_no {
            return Ok(());
        }
        let mut old = self.rows_signature();
        if self.click_page_number(page_no)? && self.wait_page(page_no)? {
            self.wait_rows_changed(&old)?;
            self.last_ui_action = Instant::now();
            return Ok(());
        }
        cur = self.current_page()?;
        if cur > page_no {
            self.js(&format!("FJ.click({})", self.sjs("paginator_first")))?;
            self.wait_page(1)?;
            self.wait_rows_changed(&old)?;
            old = self.rows_signature();
            cur = 1;
        }
        while cur < page_no {
            let nums = self.js(&format!(
                "FJ.$$({}).map(function(e){{return (e.innerText||'').trim()}})",
                self.sjs("paginator_page")
            ))?;
            let best = nums
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|v| v.as_str().and_then(|s| s.parse::<usize>().ok()))
                .filter(|n| *n > cur && *n <= page_no)
                .max()
                .unwrap_or(0);
            let mut moved = false;
            if best > 0 && self.click_page_number(best)? {
                moved = self.wait_page(best)?;
            }
            if !moved {
                self.js(&format!("FJ.click({})", self.sjs("paginator_next")))?;
                self.wait_page(cur + 1)?;
            }
            let new = self.current_page()?;
            if new == cur {
                return Err(RunErr::CaseOpenFailed {
                    msg: format!("Could not move to list page {page_no}."),
                    broken: false,
                });
            }
            self.wait_rows_changed(&old)?;
            old = self.rows_signature();
            cur = new;
        }
        self.last_ui_action = Instant::now();
        Ok(())
    }

    pub fn read_card(&self, pos: usize) -> RResult<Option<Card>> {
        let v = self.js(&format!("FJ.cards({})[{pos}]||null", self.sjs("case_link")))?;
        if v.is_null() {
            return Ok(None);
        }
        Ok(Some(parse_card(v["card"].as_str().unwrap_or(""), v["link"].as_str().unwrap_or(""))))
    }

    pub fn browser_back(&mut self) -> RResult<()> {
        for attempt in 1..=self.cfg.browser_back_max {
            if !self.url_has("url_detail_part") {
                return Ok(());
            }
            self.warn(format!(
                "   page blocked (endless loading) - using the browser's Back function ({attempt})"
            ));
            let _ = self.portal.go_back();
            self.wait_ms(self.cfg.ui_wait_long)?;
            self.last_ui_action = Instant::now();
        }
        Ok(())
    }

    pub fn match_carpeta(&mut self, c: &Card) -> Option<usize> {
        let mut best: Option<usize> = None;
        let mut best_score = -1;
        for (i, item) in self.carpetas.iter().enumerate() {
            if self.carpeta_used.contains(&i) {
                continue;
            }
            let (n, k, d, o) = carpeta_ident(item);
            if n != c.number || k != c.kind || d != c.date {
                continue;
            }
            let score = if norm_txt(&o) == norm_txt(&c.office) { 2 } else { 0 };
            if score > best_score {
                best = Some(i);
                best_score = score;
            }
        }
        let b = best.filter(|_| best_score >= 2)?;
        self.carpeta_used.insert(b);
        Some(b)
    }

    pub fn dup_groups(&self) -> Vec<Value> {
        let mut order: Vec<String> = Vec::new();
        let mut groups: std::collections::HashMap<String, Vec<usize>> = Default::default();
        for (i, c) in self.carpetas.iter().enumerate() {
            let k = dup_key(c);
            if !groups.contains_key(&k) {
                order.push(k.clone());
            }
            groups.entry(k).or_default().push(i);
        }
        let mut out = Vec::new();
        for k in order {
            let items = &groups[&k];
            if items.len() < 2 {
                continue;
            }
            let sets: Vec<Option<&BTreeSet<String>>> = items.iter().map(|i| self.dup_docs.get(i)).collect();
            let (state, only): (&str, Vec<usize>) = if sets.iter().any(|s| s.is_none()) {
                ("unchecked", vec![])
            } else {
                let ss: Vec<&BTreeSet<String>> = sets.iter().map(|s| s.unwrap()).collect();
                let inter: BTreeSet<String> = ss[0].iter().filter(|x| ss.iter().all(|s| s.contains(*x))).cloned().collect();
                let only = ss.iter().map(|s| s.len() - s.intersection(&inter).count()).collect();
                (if ss.iter().all(|s| *s == ss[0]) { "same" } else { "diff" }, only)
            };
            let first = &self.carpetas[items[0]];
            let mut fields: BTreeSet<String> = BTreeSet::new();
            for i in &items[1..] {
                let c = &self.carpetas[*i];
                let (a, b) = (first.as_object(), c.as_object());
                if let (Some(a), Some(b)) = (a, b) {
                    for key in a.keys().chain(b.keys()) {
                        if sort_canon(a.get(key).unwrap_or(&Value::Null)) != sort_canon(b.get(key).unwrap_or(&Value::Null)) {
                            fields.insert(key.clone());
                        }
                    }
                }
            }
            let (n, kind, d, o) = carpeta_ident(first);
            out.push(json!({
                "key": format!("{n}|{kind}|{d}|{o}"), "case": n, "kind": kind, "date": d, "office": o,
                "rows": items.iter().map(|i| i + 1).collect::<Vec<_>>(), "n_list": self.carpetas.len(), "state": state,
                "docs": sets.iter().map(|s| s.map(|x| json!(x.len())).unwrap_or(Value::Null)).collect::<Vec<_>>(),
                "only": only, "fields": fields.into_iter().collect::<Vec<_>>(),
            }));
        }
        out
    }

    pub fn save_dups(&mut self) {
        let new = if self.carpetas.is_empty() { None } else { Some(self.dup_groups()) };
        let groups = crate::state::dups_merge(&self.cfg.protocol_dir, new);
        crate::state::save_dups(&self.cfg.protocol_dir, &groups);
        if groups.is_empty() {
            return;
        }
        let n_same: usize = groups
            .iter()
            .filter(|g| g["state"] == "same")
            .map(|g| g["rows"].as_array().map(|a| a.len()).unwrap_or(1).saturating_sub(1))
            .sum();
        let n_diff = groups.iter().filter(|g| g["state"] == "diff").count();
        let n_un = groups.iter().filter(|g| g["state"] != "same" && g["state"] != "diff").count();
        self.info(format!(
            "Lines the portal shows twice: {n_same} with the same documents, {n_diff} group(s) with OTHER documents, {n_un} group(s) not compared"
        ));
        for g in &groups {
            if g["state"] == "diff" {
                let rows = g["rows"].as_array().cloned().unwrap_or_default();
                let docs = g["docs"].as_array().cloned().unwrap_or_default();
                let parts: Vec<String> = rows
                    .iter()
                    .zip(docs.iter())
                    .map(|(r, n)| crate::i18n::tr(&format!("line {r}: {}", crate::pyfmt::py_str(Some(n)))))
                    .collect();
                self.warn(format!(
                    "   WARNING: {} is shown {} times with other documents: {}",
                    self.id_case(g["case"].as_str().unwrap_or("")),
                    rows.len(),
                    parts.join(", ")
                ));
            }
        }
    }

    pub fn save_raw(&self, name: &str, data: &Value) {
        if !self.cfg.save_raw_json || data.is_null() {
            return;
        }
        let dir = self.cfg.raw_dir.join(&self.run_id);
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(format!("{}.json", crate::text::sanitize(name, None, false)));
        let _ = std::fs::write(file, crate::pyfmt::dumps_file(data, crate::pyfmt::Indent::Spaces(1), false));
    }
}

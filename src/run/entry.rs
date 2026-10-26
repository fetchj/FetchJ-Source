use super::list::dup_key;
use super::{Card, EntryData, FailedDoc, Fetcher, RResult, RunErr, Summary};
use crate::bridge::js;
use crate::caseinfo::{self, api_general_text, api_partes_text, EntryInfo};
use crate::protocol::{row, Row};
use crate::pyfmt::{py_or_empty, py_str, truthy};
use crate::state::{parties_key, FailMemory};
use crate::text::parse_portal_date;
use serde_json::{json, Map, Value};
use std::time::{Duration, Instant};

pub fn clean(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().filter(|(_, x)| !x.is_null()).map(|(k, x)| (k.clone(), clean(x))).collect()),
        Value::Array(a) => Value::Array(a.iter().filter(|x| !x.is_null()).map(clean).collect()),
        x => x.clone(),
    }
}

pub fn sv(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        x => py_str(x),
    }
}

pub fn carpeta_key(v: Option<&Value>) -> String {
    if truthy(v) {
        py_str(v)
    } else {
        String::new()
    }
}

impl Fetcher {
    fn api_post(&mut self, key: &str, body: &Value) -> RResult<Value> {
        let url = format!("{}/{}", self.cfg.api_base_url, self.cfg.api[key]);
        let seg = self.cfg.api[key].rsplit('/').next().unwrap_or("").to_string();
        self.keepalive()?;
        self.activity_tick()?;
        let text = serde_json::to_string(&clean(body)).unwrap_or_default();
        let t0 = Instant::now();
        let r = self.api_post_1(&url, &seg, &text);

        self.api_times.push((key.to_string(), t0.elapsed().as_secs_f64()));
        r
    }

    fn api_post_1(&mut self, url: &str, seg: &str, text: &str) -> RResult<Value> {
        let (url, text) = (url.to_string(), text.to_string());
        for attempt in 1..=2 {
            self.ensure_session_fresh()?;
            let r = self.api_call("POST", &url, &json!({}), &text, self.cfg.case_open_timeout, true)?;
            if !r.err.is_empty() {
                return Err(RunErr::Other(if r.err == "timeout" {
                    format!("TimeoutError: {seg} did not answer within {} s", self.cfg.case_open_timeout)
                } else {
                    format!("Error: {seg}: {}", r.err)
                }));
            }
            if (r.status == 401 || r.status == 403) && attempt == 1 {
                self.refresh_session()?;
                continue;
            }
            if r.status != 200 {
                return Err(RunErr::Other(format!("{seg} HTTP {}", r.status)));
            }
            let j: Value = serde_json::from_str(&r.text).unwrap_or(Value::Null);
            if !j.is_object() || truthy(j.get("huboError")) {
                let m = py_or_empty(j.get("mensajeUsuario"));
                let m = if m.is_empty() { "error".to_string() } else { m };
                return Err(RunErr::PortalNoData(format!("{seg}: {}", m.chars().take(80).collect::<String>())));
            }
            return Ok(j);
        }
        Err(RunErr::Other("no answer".into()))
    }

    pub fn api_open_entry(&mut self, idx: usize) -> RResult<EntryData> {
        self.api_times.clear();
        if !self.js("FJ.hasUser()")?.as_bool().unwrap_or(false) {
            return Err(RunErr::Other("portal user account not known yet".into()));
        }
        let c = self.carpetas[idx].clone();
        let exp = c.get("expediente").filter(|e| e.is_object()).cloned();
        let principal = exp.is_none();
        let ctx = c.get("contexto").filter(|x| truthy(Some(x))).cloned().unwrap_or(json!({}));
        let siagpj_ctx = truthy(ctx.get("provieneSIAGPJ"));
        let g = |k: &str| c.get(k).cloned().unwrap_or(Value::Null);
        let datos = if principal {
            let mut b = c.as_object().cloned().unwrap_or_default();
            b.remove("intervenciones");
            self.api_post("api_q_general_case", &Value::Object(b))?
        } else {
            let mut b = c.as_object().cloned().unwrap_or_default();
            let mut e = exp.clone().unwrap().as_object().cloned().unwrap_or_default();
            e.remove("intervenciones");
            b.insert("expediente".into(), Value::Object(e));
            self.api_post("api_q_general_legajo", &Value::Object(b))?
        };
        if !truthy(datos.get("valorDevuelto")) {
            return Err(RunErr::PortalNoData("no general data".into()));
        }
        let mut dr = Map::new();
        dr.insert("carpeta".into(), g("carpeta"));
        dr.insert("provieneSIAGPJ".into(), g("provieneSIAGPJ"));
        dr.insert("contexto".into(), ctx.clone());
        dr.insert(
            "numeroExpediente".into(),
            if principal {
                g("numero")
            } else {
                exp.as_ref().and_then(|e| e.get("numero")).cloned().unwrap_or(Value::Null)
            },
        );
        dr.insert("codigoLegajo".into(), if principal { Value::Null } else { json!([g("codigo")]) });
        dr.insert("funcionario".into(), json!({"usuarioRed": "__FJ_USER__"}));
        let docs_req = clean(&Value::Object(dr));
        /* removed in the public source: validation / licence check · eliminado en el código público: validación / licencia */
        let docs = self.api_post("api_q_documents", &docs_req)?;

        let gt = api_general_text(datos.get("valorDevuelto").filter(|v| truthy(Some(v))).unwrap_or(&json!({})));
        let ck = super::list::dup_key(&c);
        let sig = crate::entry_check::signature(&gt, docs.get("valorDevuelto").unwrap_or(&Value::Null));
        if self.entry_check.unchanged(&ck, &sig) && !self.cfg.case_info_personal {
            self.light_entries += 1;
            let prev = self.entry_check.get(&ck);
            self.info("   parties and hearings: documents and general data as last time - not asked again (all of them once a day)");
            let pt = prev.get("partes_text").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let aud = json!({"valorDevuelto": prev.get("aud").cloned().filter(|v| !v.is_null()).unwrap_or(json!([]))});
            return Ok(EntryData {
                datos,
                partes: Value::Null,
                docs_req,
                docs,
                aud,
                general_text: gt,
                partes_text: pt,
                light: true,
            });
        }
        let mut partes = Value::Null;
        if self.cfg.update_parties {
            let pb = if principal {
                if truthy(c.get("provieneSIAGPJ")) {
                    json!({"numero": g("numero"), "provieneSIAGPJ": true})
                } else {
                    json!({"carpeta": g("carpeta"), "numero": g("numero"), "provieneSIAGPJ": g("provieneSIAGPJ"), "contexto": ctx})
                }
            } else if truthy(c.get("provieneSIAGPJ")) {
                json!({"codigo": g("codigo"), "provieneSIAGPJ": true})
            } else {
                json!({"carpeta": g("carpeta"), "codigo": g("codigo"), "expediente": exp, "provieneSIAGPJ": g("provieneSIAGPJ"), "contexto": ctx})
            };
            let key = if principal { "api_q_parties_case" } else { "api_q_parties_legajo" };
            match self.api_post(key, &pb) {
                Ok(p) => partes = p,
                Err(e) if e.is_fatal() => return Err(e),
                Err(e) => self.warn(format!("   'Partes' not loaded ({}) - continuing.", e.short())),
            }
        }
        let ab = if principal || !siagpj_ctx {
            json!({"expediente": {"numero": g("numero"), "carpeta": g("carpeta")}, "contexto": {"codigo": ctx.get("codigo").cloned().unwrap_or(Value::Null), "provieneSIAGPJ": siagpj_ctx}})
        } else {
            json!({"contexto": {"codigo": ctx.get("codigo").cloned().unwrap_or(Value::Null), "provieneSIAGPJ": siagpj_ctx}, "legajoDetalle": {"codigo": g("codigo")}})
        };
        /* removed in the public source: validation / licence check · eliminado en el código público: validación / licencia */
        let aud = match self.api_post("api_q_hearings", &ab) {
            Ok(a) => a,
            Err(e) if e.is_fatal() => return Err(e),
            Err(e) => {
                self.warn(format!("   'Audiencias' not loaded ({}) - continuing.", e.short()));
                Value::Null
            }
        };
        let pt = api_partes_text(partes.get("valorDevuelto").unwrap_or(&Value::Null));
        if !partes.is_null() && !aud.is_null() {
            let av = aud.get("valorDevuelto").cloned().filter(|v| !v.is_null()).unwrap_or(json!([]));
            self.entry_check.note(&ck, &sig, av, &pt);
        }
        Ok(EntryData {
            datos,
            partes,
            docs_req,
            docs,
            aud,
            general_text: gt,
            partes_text: pt,
            light: false,
        })
    }

    fn click_expect(&mut self, click_expr: &str, part: &str, timeout: u64) -> RResult<Option<(Value, Value)>> {
        self.js(&format!("FJ.watchStart([{}])", js(part)))?;
        if !self.js(click_expr)?.as_bool().unwrap_or(false) {
            self.js("FJ.watchStop()")?;
            return Ok(None);
        }
        let end = Instant::now() + Duration::from_secs(timeout);
        while Instant::now() < end {
            self.wait_ms(200)?;
            if let Some(a) = self.js_opt(&format!("FJ.watchTake({})", js(part)))? {
                if let Some(r) = a.as_array().and_then(|a| a.first()) {
                    self.js_opt("FJ.watchStop()")?;
                    let req: Value = serde_json::from_str(r["req"].as_str().unwrap_or("")).unwrap_or(Value::Null);
                    let resp: Value = serde_json::from_str(r["text"].as_str().unwrap_or("")).unwrap_or(Value::Null);
                    return Ok(Some((req, resp)));
                }
            }
        }
        self.js_opt("FJ.watchStop()")?;
        Ok(None)
    }

    fn expand(&mut self, title: &str, part: &str, timeout: u64, required: bool) -> RResult<Option<(Value, Value)>> {
        let r = self.click_expect(&format!("FJ.clickText({},true)", js(title)), part, timeout)?;
        if r.is_none() {
            if required {
                return Err(RunErr::CaseOpenFailed {
                    msg: format!("'{title}' did not load within {timeout}s (TimeoutError)"),
                    broken: false,
                });
            }
            self.warn(format!("   '{title}' did not load (TimeoutError) - continuing."));
        }
        Ok(r)
    }

    fn accordion_text(&self, title: &str) -> String {
        let expr = format!(
            "(function(){{var t={};var els=Array.prototype.slice.call(document.querySelectorAll('a, span, div, button, h2, h3, h4')).filter(function(e){{return (e.textContent||'').trim()===t}});for(var i=0;i<els.length;i++){{var tab=els[i].closest('p-accordiontab, .p-accordion-tab');if(tab)return tab.innerText}}return ''}})()",
            js(title)
        );
        self.js(&expr).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
    }

    fn open_entry(&mut self, pos: usize) -> RResult<EntryData> {
        let t = self.cfg.case_open_timeout;
        let click = format!("FJ.click({},{pos})", js(&self.cfg.sel["case_link"]));
        let part = self.cfg.api["api_general_data"].clone();
        let Some((_, datos)) = self.click_expect(&click, &part, t)? else {
            return Err(RunErr::CaseOpenFailed {
                msg: format!("Case did not open within {t}s (TimeoutError)"),
                broken: false,
            });
        };
        if !datos.is_object() || truthy(datos.get("huboError")) || !truthy(datos.get("valorDevuelto")) {
            let m = py_or_empty(datos.get("mensajeUsuario"));
            let m = if m.is_empty() { "no general data".to_string() } else { m };
            return Err(RunErr::CaseOpenFailed {
                msg: format!(
                    "portal delivered no general data for this case ({})",
                    m.chars().take(80).collect::<String>()
                ),
                broken: true,
            });
        }
        self.last_ui_action = Instant::now();
        let (sp, sd, sh) = (
            self.cfg.sel["accordion_parties"].clone(),
            self.cfg.sel["accordion_documents"].clone(),
            self.cfg.sel["accordion_hearings"].clone(),
        );
        let (ap, ad, ah) = (
            self.cfg.api["api_parties"].clone(),
            self.cfg.api["api_documents"].clone(),
            self.cfg.api["api_hearings"].clone(),
        );
        let partes = self.expand(&sp, &ap, t, false)?.map(|x| x.1);
        let dt = if partes.is_some() { t } else { self.cfg.ui_element_timeout };
        let (docs_req, docs) = match self.expand(&sd, &ad, dt, true) {
            Ok(Some(x)) => x,
            Ok(None) => unreachable!(),
            Err(RunErr::CaseOpenFailed { msg, broken }) => {
                if partes.is_none() {
                    return Err(RunErr::CaseOpenFailed {
                        msg: "case hangs on the portal side (neither 'Partes' nor 'Documentos' loaded)".into(),
                        broken: true,
                    });
                }
                return Err(RunErr::CaseOpenFailed { msg, broken });
            }
            Err(e) => return Err(e),
        };
        let aud = self.expand(&sh, &ah, t, false)?.map(|x| x.1).unwrap_or(Value::Null);
        let general_text = self.accordion_text(&self.cfg.sel["accordion_general"].clone());
        let partes_text = self.accordion_text(&sp);
        Ok(EntryData {
            datos,
            partes: partes.unwrap_or(Value::Null),
            docs_req,
            docs,
            aud,
            general_text,
            partes_text,
            light: false,
        })
    }

    pub fn doc_row(&self, c: &Card, carpeta: Option<&Value>, d: &Value) -> Row {
        let pdt = parse_portal_date(&py_or_empty(d.get("fechaCreacion")));
        let te = d
            .get("tipoEscrito")
            .filter(|t| t.is_object())
            .map(|t| sv(t.get("descripcion")))
            .unwrap_or_default();
        let est = {
            let a = d.get("descripcionEstado").filter(|v| truthy(Some(v)));
            match a {
                Some(v) => py_str(Some(v)),
                None => sv(d.get("estadoDocumento")),
            }
        };
        row(&[
            ("run_id", self.run_id.clone()),
            ("run_start", crate::pyfmt::ts_human(&self.run_start)),
            ("case_number", c.number.clone()),
            ("entry_kind", c.kind.clone()),
            ("entry_office", c.office.clone()),
            ("entry_class", c.class_text.clone()),
            ("entry_date", c.date.clone()),
            ("carpeta", sv(carpeta)),
            ("doc_id", sv(d.get("identificador"))),
            ("doc_description", sv(d.get("descripcion"))),
            ("doc_description_full", sv(d.get("descripcionCompleta"))),
            (
                "portal_date",
                pdt.map(|p| p.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_else(|| sv(d.get("fechaCreacion"))),
            ),
            ("estado", est),
            ("tipo_documento", sv(d.get("clasificacion"))),
            ("tipo_escrito", te),
        ])
    }

    pub fn is_done(&self, carpeta: Option<&Value>, d: &Value) -> bool {
        self.proto.is_done(&carpeta_key(carpeta), &sv(d.get("identificador")))
    }

    fn older_than_limit(&self, mode: &str, d: &Value) -> bool {
        let Some(lim) = self.cfg.docs_from_date else { return false };
        if mode == "full" {
            return false;
        }
        parse_portal_date(&py_or_empty(d.get("fechaCreacion")))
            .map(|p| p.date() < lim)
            .unwrap_or(false)
    }

    pub fn finish_summary(&self, s: &mut Summary, carpeta: Option<&Value>, docs: &[Value]) {
        let (mut ok, mut before) = (0, 0);
        let mut missing = Vec::new();
        let mode = self.cfg.entry_mode(&s.number, &s.kind, &s.date, &s.office);
        for d in docs {
            if self.is_done(carpeta, d) {
                ok += 1;
            } else if self.older_than_limit(&mode, d) {
                before += 1;
            } else {
                missing.push(d.clone());
            }
        }
        s.before_date = before;
        s.ok_total = ok;
        s.already = ok.saturating_sub(s.new);
        s.missing = missing;
        s.carpeta = Some(carpeta.cloned().unwrap_or(Value::Null));
        s.status = if s.missing.is_empty() {
            "COMPLETE"
        } else if self.cfg.dry_run {
            "DRY_RUN"
        } else {
            "INCOMPLETE"
        }
        .into();
    }

    pub fn process_entry(&mut self, page_no: usize, pos: usize) -> RResult<()> {
        let t_entry = Instant::now();
        self.ensure_list(false, "")?;
        self.go_to_page(page_no)?;
        let Some(info) = self.read_card(pos)? else { return Ok(()) };
        self.seq += 1;
        let seq = self.seq;
        self.info("");
        let eid = self.id_entry(&info.number, &info.kind, &info.date, &info.office);
        self.info(format!("[{seq}] {eid}  ({})", info.kind));
        if self.cfg.entry_mode(&info.number, &info.kind, &info.date, &info.office) == "skip" {
            self.info("   skipped (case set to 'do not load')");
            return Ok(());
        }
        self.prog_i += 1;
        self.live_progress = (info.number.clone(), 0, 0);
        self.write_live_status(false, true, false, "");
        let resume_key = self.resume_key(&info);

        let resume_gap = self.run_state.is_done(&resume_key) && self.proto.case_files_missing(&info.number) > 0;
        if resume_gap {
            self.info(format!(
                "   processed in the interrupted run, but {} file(s) of this case are missing in the case folder - checking it again",
                self.proto.case_files_missing(&info.number)
            ));
        }
        if self.run_state.is_done(&resume_key) && !resume_gap {
            self.resume_skipped += 1;
            let from = self.run_state.resumed_from.clone().unwrap_or_default();
            self.info(format!("   skipped (already processed in the interrupted run {from})"));
            return Ok(());
        }
        let ekey = FailMemory::entry_key(&info.number, &info.kind, &info.date, &info.office);
        if let Some(until) = self.failmem.skip("entries", &ekey) {
            self.info(format!(
                "   paused: this entry did not open in {} runs - next try after {}",
                self.failmem.count("entries", &ekey),
                until.format("%d/%m/%Y")
            ));
            return Ok(());
        }
        let mut summary = Summary {
            resume_key,
            seq,
            number: info.number.clone(),
            kind: info.kind.clone(),
            date: info.date.clone(),
            office: info.office.clone(),
            class_text: info.class_text.clone(),
            ..Default::default()
        };
        let mut data: Option<EntryData> = None;
        let mut last_err = String::new();
        let mut no_data = false;
        let t_open_start = Instant::now();
        let mut via_api = false;
        let mut c_idx: Option<usize> = None;
        let mut no_query = false;
        if self.cfg.entry_fetch_mode == "api" {
            c_idx = if self.carpetas.is_empty() {
                None
            } else {
                self.match_carpeta(&info)
            };
            match c_idx {
                None => {
                    let why = if self.carpetas.is_empty() {
                        "list data of the portal not available"
                    } else {
                        "no portal data for this entry in the list"
                    };
                    self.warn(format!("   direct query not possible ({why}) - opening the entry in the web page"));
                }
                Some(i) => {
                    let dk = dup_key(&self.carpetas[i]);
                    if let Some((first, d)) = self.dup_cache.get(&dk).cloned() {
                        no_query = true;
                        data = Some(d);
                        via_api = true;
                        self.dup_reused += 1;
                        self.info(format!(
                            "   same line as [{first}] (the portal lists this entry twice: same folder and codes) - its answer is used, not queried again"
                        ));
                    } else {
                        for q_try in 1..=2 {
                            match self.api_open_entry(i) {
                                Ok(d) => {
                                    self.dup_cache.insert(dk.clone(), (seq, d.clone()));
                                    data = Some(d);
                                    via_api = true;
                                    self.note_success();
                                    break;
                                }
                                Err(e) if e.is_fatal() => return Err(e),
                                Err(RunErr::PortalNoData(m)) => {
                                    last_err = format!("portal delivered no data for this entry: {m}");
                                    no_data = true;
                                    self.warn(format!(
                                        "   the portal answers, but delivers no data for this entry ({m}) - not opened in the web page (it shows the same), it will be tried again in the next update"
                                    ));
                                    break;
                                }
                                Err(e) => {
                                    last_err = e.short();
                                    if q_try == 1 {
                                        self.warn(format!(
                                            "   direct query failed ({last_err}) - once more in {:.0} s (without loading the list again)",
                                            self.cfg.retry_wait
                                        ));
                                        self.sleep(self.cfg.retry_wait)?;
                                    } else {
                                        self.warn(format!("   direct query failed ({last_err}) - opening the entry in the web page"));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut attempt = 0;
        while data.is_none() && !no_data && attempt < self.cfg.case_open_retries + 1 {
            attempt += 1;
            if attempt > 1 {
                self.ensure_list(false, "next attempt to open the entry")?;
                self.go_to_page(page_no)?;
            }
            match self.open_entry(pos) {
                Ok(d) => {
                    data = Some(d);
                    self.note_success();
                }
                Err(RunErr::CaseOpenFailed { msg, broken }) => {
                    last_err = msg.clone();
                    self.warn(format!("   Open attempt {attempt} failed: {msg} (page: {})", self.short_url()));
                    self.browser_back()?;
                    if !self.is_logged_in()? {
                        self.note_error(&last_err.clone())?;
                        self.handle_session_problem("logged out while opening a case", true)?;
                    } else if broken {
                        self.warn("   the portal delivers no data for this case - it will not be retried in this run, but it will be tried again in the next full run");
                        break;
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if data.is_some() {
            if via_api {
                self.open_api += 1;
            } else {
                self.open_click += 1;
            }
        }
        let Some(data) = data else {
            self.failmem
                .failed("entries", &ekey, &[("case", &info.number), ("office", &info.office)]);
            summary.status = "OPEN_FAILED".into();
            summary.error = last_err;
            self.entries.push(summary.clone());
            self.failed_entries.push(self.entries.len() - 1);
            self.write_entry_row(&summary);
            match self.ensure_list(false, "after an entry that did not open") {
                Err(RunErr::SessionLost(m)) => return Err(RunErr::SessionLost(m)),
                Err(e) if e.is_fatal() => return Err(e),
                _ => {}
            }
            return Ok(());
        };
        self.failmem.ok("entries", &ekey);
        let t_open_end = Instant::now();
        if self.cfg.update_parties && !data.light {
            let k = parties_key(&info.number, &info.kind, &info.date, &info.office);
            self.parties
                .update(&k, data.partes.get("valorDevuelto").unwrap_or(&Value::Null), self.cfg.ct_detect);
        }
        let docs_all: Vec<Value> = data
            .docs
            .get("valorDevuelto")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let aud_list: Vec<Value> = data
            .aud
            .get("valorDevuelto")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if via_api {
            if let Some(i) = c_idx {
                self.dup_docs.insert(
                    i,
                    docs_all
                        .iter()
                        .filter(|d| d.is_object())
                        .map(|d| py_str(d.get("identificador")))
                        .collect(),
                );
            }
        }
        let carpeta: Option<Value> = data.docs_req.get("carpeta").cloned();
        let contexto = data
            .docs_req
            .get("contexto")
            .filter(|v| truthy(Some(v)))
            .cloned()
            .unwrap_or(json!({}));
        summary.portal = docs_all.len();
        summary.aud = aud_list.len();
        if !aud_list.is_empty() {
            self.audiencias.push((info.clone(), aud_list.clone()));
        }
        let base_name = format!("{seq:03}_{}_{}", info.number, info.kind);
        self.save_raw(&format!("{base_name}_1_datos"), &data.datos);
        self.save_raw(&format!("{base_name}_2_partes"), &data.partes);
        self.save_raw(&format!("{base_name}_4_audiencias"), &data.aud);
        let case_dir = self.cfg.case.dir.join(&info.number);
        let _ = std::fs::create_dir_all(&case_dir);
        let carpeta_txt = match &carpeta {
            None | Some(Value::Null) => None,
            Some(v) => Some(py_str(Some(v))),
        };
        let ci_key = caseinfo::case_info_key(&info.kind, carpeta_txt.as_deref(), &info.date, &info.office);
        let ei = EntryInfo {
            kind: &info.kind,
            number: &info.number,
            date: &info.date,
            office: &info.office,
            asunto: &info.asunto,
            clase_asunto: &info.clase_asunto,
            clase: &info.clase,
            class_text: &info.class_text,
        };
        let block = caseinfo::case_info_block(
            &ei,
            &data.general_text,
            &data.partes_text,
            &data.partes,
            docs_all.len(),
            &aud_list,
            self.cfg.case_info_personal,
        );

        let ci_dir = caseinfo::info_dir(&self.cfg.case.dir, &info.number);
        let media_dir = ci_dir.clone();
        if caseinfo::write_case_info(&ci_dir, &ei, &block, &ci_key, &mut self.ci_state, &self.version, &self.run_id) {
            self.info("   case info file updated");
        }
        if !aud_list.is_empty() {
            self.media.update(
                &info.number,
                &info.kind,
                &info.office,
                &info.date,
                &info.class_text,
                &aud_list,
                &self.run_id,
            );
            self.media.save();
            let reg = &self.media;
            let num = info.number.clone();
            let have = move |code: &str| reg.recs.get(&crate::media::key(&num, code)).map(|r| r.present()).unwrap_or(false);
            let (m, d, a) = caseinfo::write_media_markers(
                &media_dir,
                &case_dir,
                &ei,
                &aud_list,
                &self.cfg.portal_tz,
                &self.version,
                &self.run_id,
                &have,
            );
            summary.media_missing = m;
            self.info(format!(
                "   recordings: {} (not downloaded {m}, downloaded {d}, only in the court archive {a})",
                aud_list.len()
            ));
        }
        let total_listed = docs_all.len();
        let media: Vec<Value> = docs_all
            .iter()
            .filter(|d| {
                self.cfg
                    .media_document_types
                    .contains(&py_or_empty(d.get("clasificacion")).trim().to_lowercase())
            })
            .cloned()
            .collect();
        let docs: Vec<Value> = docs_all.iter().filter(|d| !media.contains(d)).cloned().collect();
        summary.portal = docs.len();
        summary.media_docs = media.clone();

        self.media.update_doc_media(
            &info.number,
            &info.kind,
            &info.office,
            &info.date,
            &info.class_text,
            &media,
            !aud_list.is_empty(),
            &self.run_id,
        );
        self.media.save();
        self.info(format!(
            "   documents on portal: {total_listed} = {} files + {} recordings (recordings are stream-only and will be skipped)   audiencias: {}",
            docs.len(),
            media.len(),
            aud_list.len()
        ));
        for d in &docs {
            self.id_doc(&sv(d.get("identificador")));
        }
        if let Ok(mut m) = self.ids.lock() {
            m.save();
        }
        let mut todo: Vec<Value> = Vec::new();
        let (mut n_done, mut n_old, mut n_paused, mut n_before, mut n_disk) = (0, 0, 0, 0, 0);
        let ck = carpeta_key(carpeta.as_ref());
        let mode_e = self.cfg.entry_mode(&info.number, &info.kind, &info.date, &info.office);
        for d in &docs {
            let id = sv(d.get("identificador"));
            let pdt = parse_portal_date(&py_or_empty(d.get("fechaCreacion")));
            if !self.cfg.overrule_protocol && !self.cfg.dry_run && !self.proto.is_done(&ck, &id) {
                if let Some(p) = self.proto.found_on_disk(&info.number, &id) {
                    let r = self.doc_row(&info, carpeta.as_ref(), d);
                    self.proto.adopt_existing(r, &p);

                    let _ = crate::read_state::mark(&self.cfg.read_state_file, "doc", std::slice::from_ref(&id), false);
                    crate::read_state::add_present(&self.cfg.read_state_file, &id);
                    n_disk += 1;
                    continue;
                }
            }
            if self.older_than_limit(&mode_e, d) && !self.proto.is_done(&ck, &id) {
                n_before += 1;
                continue;
            }
            if self.cfg.overrule_protocol {
                if pdt.map(|p| p.date() < self.cfg.overrule_start_date).unwrap_or(false) {
                    n_old += 1;
                    continue;
                }
                todo.push(d.clone());
            } else if self.proto.is_done(&ck, &id) {
                n_done += 1;
            } else if self
                .failmem
                .skip("docs", &FailMemory::doc_key(&carpeta_txt.clone().unwrap_or_default(), &id))
                .is_some()
            {
                n_paused += 1;
            } else {
                todo.push(d.clone());
            }
        }
        let mut reasons: Vec<String> = Vec::new();
        if n_done > 0 {
            reasons.push(format!("{n_done} already downloaded (protocol)"));
        }
        if n_disk > 0 {
            reasons.push(format!("{n_disk} already in the case folder (taken over without downloading)"));
            self.adopted_from_disk += n_disk;
        }
        if n_old > 0 {
            reasons.push(format!("{n_old} older than overrule_start_date {}", self.cfg.overrule_start_date));
        }
        if n_before > 0 {
            reasons.push(format!(
                "{n_before} older than docs_from_date {} (not loaded)",
                self.cfg.docs_from_date.map(|d| d.to_string()).unwrap_or_default()
            ));
            self.skipped_by_date += n_before;
        }
        if n_paused > 0 {
            reasons.push(format!(
                "{n_paused} paused after failing in {} runs",
                self.cfg.skip_after_failed_runs
            ));
        }
        if self.cfg.overrule_protocol {
            reasons.push("overrule_protocol = 1: protocol ignored".into());
        }
        if !todo.is_empty() {
            self.save_raw(
                &format!("{base_name}_3_documentos"),
                &json!({"request": data.docs_req, "response": data.docs}),
            );
        }
        let tail = if reasons.is_empty() {
            String::new()
        } else {
            format!("  ({})", reasons.iter().map(|x| crate::i18n::tr(x)).collect::<Vec<_>>().join("; "))
        };
        self.info(format!("   to download: {} of {} files{tail}", todo.len(), docs.len()));
        self.current = Some((summary.clone(), carpeta.clone(), docs.clone()));
        self.entries.push(summary.clone());
        let eidx = self.entries.len() - 1;
        let n_todo = todo.len();
        for (i_doc, d) in todo.iter().enumerate() {
            self.live_progress = (info.number.clone(), i_doc + 1, n_todo);
            self.write_live_status(false, false, false, "");
            if self.cfg.max_documents_per_run > 0 && self.docs_downloaded as i64 >= self.cfg.max_documents_per_run {
                self.warn("   max_documents_per_run reached - stopping downloads.");
                break;
            }
            if self.cfg.dry_run {
                let dn = self.id_doc(&sv(d.get("identificador")));
                self.info(format!("      DRY RUN: would download {dn}"));
                continue;
            }
            let st = self.process_document(&info, carpeta.as_ref(), &contexto, d, &case_dir)?;
            if ["OK", "OK_SAME_FILE", "OK_EXISTING"].contains(&st.as_str()) {
                self.entries[eidx].new += 1;
            } else {
                self.entries[eidx].failed += 1;
                self.failed.push(FailedDoc {
                    info: info.clone(),
                    carpeta: carpeta.clone().unwrap_or(Value::Null),
                    contexto: contexto.clone(),
                    doc: d.clone(),
                    case_dir: case_dir.clone(),
                    entry: eidx,
                });
            }
            if let Some(cur) = self.current.as_mut() {
                cur.0.new = self.entries[eidx].new;
                cur.0.failed = self.entries[eidx].failed;
            }
            self.sleep(self.cfg.delay_between_downloads)?;
        }
        let t_dl_end = Instant::now();
        let mut s = self.entries[eidx].clone();
        self.finish_summary(&mut s, carpeta.as_ref(), &docs);
        self.entries[eidx] = s.clone();
        self.current = None;
        self.write_entry_row(&s);
        if !via_api {
            if self
                .js(&format!("FJ.click({})", js(&self.cfg.sel["back_button"])))
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                let _ = self.wait_list(false);
            }
        }
        self.entry_check.save();
        if self.cfg.delay_between_cases > 0.0 && !no_query {
            self.sleep(self.cfg.delay_between_cases)?;
        }
        let how = if via_api { "direct query" } else { "click" };
        let f = |a: Instant, b: Instant| b.duration_since(a).as_secs_f64();
        let now = Instant::now();
        self.info(format!(
            "   time ({how}): navigation {:.1} s · open {:.1} s · downloads {:.1} s · finish {:.1} s · total {:.1} s",
            f(t_entry, t_open_start),
            f(t_open_start, t_open_end),
            f(t_open_end, t_dl_end),
            f(t_dl_end, now),
            f(t_entry, now)
        ));
        if !self.api_times.is_empty() && f(t_open_start, t_open_end) >= 3.0 {
            let nm = |k: &str| match k {
                "api_q_general_case" | "api_q_general_legajo" => "general".to_string(),
                "api_q_parties_case" | "api_q_parties_legajo" => "parties".to_string(),
                "api_q_documents" => "documents".to_string(),
                "api_q_hearings" => "hearings".to_string(),
                x => x.to_string(),
            };
            let parts: Vec<String> = self.api_times.iter().map(|(k, d)| format!("{} {:.1} s", nm(k), d)).collect();
            self.info(format!("   time per portal call: {}", parts.join(" · ")));
        }
        self.api_times.clear();
        Ok(())
    }
}

use super::entry::{carpeta_key, sv};
use super::{ApiResp, Card, Fetcher, RResult, RunErr};
use crate::bridge::js;
use crate::caseinfo::sha256_hex;
use crate::protocol::Row;
use crate::pyfmt::{py_or_empty, truthy, ts_file, ts_human};
use crate::state::FailMemory;
use crate::text::{build_filename, detect_file_type, parse_portal_date, sanitize, splitext, NameInput};
use base64::Engine;
use chrono::NaiveDateTime;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

pub const NO_TITLE_FILE: &str = "SIN_TITULO_EN_PORTAL";

pub fn ext_from_response(ctype: &str, cdisp: &str) -> String {
    let rx = regex::Regex::new(r#"(?i)filename\*?=(?:UTF-8'')?"?([^";]+)"?"#).unwrap();
    if let Some(c) = rx.captures(cdisp) {
        let (_, e) = splitext(c[1].trim());
        let n = e.chars().count();
        if n > 1 && n <= 6 {
            return e.to_lowercase();
        }
    }
    let ct = ctype.split(';').next().unwrap_or("").trim().to_lowercase();
    let known: &[(&str, &str)] = &[
        ("application/pdf", ".pdf"),
        ("application/msword", ".doc"),
        ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", ".docx"),
        ("application/rtf", ".rtf"),
        ("text/rtf", ".rtf"),
        ("text/plain", ".txt"),
        ("image/jpeg", ".jpg"),
        ("image/png", ".png"),
        ("image/tiff", ".tif"),
        ("audio/mpeg", ".mp3"),
        ("video/mp4", ".mp4"),
        ("application/zip", ".zip"),
        ("application/vnd.ms-excel", ".xls"),
        ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", ".xlsx"),
        ("application/vnd.ms-powerpoint", ".ppt"),
        ("application/vnd.openxmlformats-officedocument.presentationml.presentation", ".pptx"),
        ("image/gif", ".gif"),
        ("image/bmp", ".bmp"),
        ("text/html", ".html"),
        ("application/xml", ".xml"),
        ("text/xml", ".xml"),
        ("application/json", ".json"),
        ("audio/ogg", ".ogg"),
        ("audio/wav", ".wav"),
        ("image/webp", ".webp"),
    ];
    known
        .iter()
        .find(|(k, _)| *k == ct)
        .map(|(_, e)| e.to_string())
        .unwrap_or_else(|| ".bin".into())
}

pub fn set_file_times(path: &Path, epoch: i64) {
    let ft = filetime::FileTime::from_unix_time(epoch, 0);
    let _ = filetime::set_file_times(path, ft, ft);
    #[cfg(windows)]
    unsafe {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{CreateFileW, SetFileTime, OPEN_EXISTING};
        let w: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let h = CreateFileW(w.as_ptr(), 0x100, 7, std::ptr::null(), OPEN_EXISTING, 0x80, std::ptr::null_mut());
        if h != INVALID_HANDLE_VALUE && !h.is_null() {
            let t = ((epoch + 11_644_473_600) as u64) * 10_000_000;
            let f = FILETIME {
                dwLowDateTime: t as u32,
                dwHighDateTime: (t >> 32) as u32,
            };
            SetFileTime(h, &f, &f, &f);
            CloseHandle(h);
        }
    }
}

const EVIDENCE: &[(&str, &[&str])] = &[
    ("server", &["server"]),
    ("via", &["via"]),
    ("powered_by", &["x-powered-by", "x-aspnet-version"]),
    (
        "gateway_ref",
        &["x-azure-ref", "x-ms-request-id", "x-request-id", "x-cache", "x-served-by"],
    ),
];

struct DlResp {
    status: u16,
    ctype: String,
    cdisp: String,
    data: Vec<u8>,
}

impl Fetcher {
    fn record_attempt(
        &mut self,
        call: &str,
        doc: &Value,
        status: Option<u16>,
        dur: f64,
        resp: Option<(&HashMap<String, String>, &[u8])>,
        error: &str,
    ) {
        let mut r: Row = Row::new();
        r.insert("run_id".into(), self.run_id.clone());
        r.insert("time".into(), crate::pyfmt::now_human());
        r.insert("case_number".into(), self.current_doc_ctx.0.clone());
        r.insert("carpeta".into(), self.current_doc_ctx.1.clone());
        r.insert("doc_id".into(), sv(doc.get("identificador")));
        r.insert("call".into(), call.into());
        r.insert("http_status".into(), status.map(|s| s.to_string()).unwrap_or_default());
        r.insert("duration_s".into(), format!("{dur:.1}"));
        r.insert("error".into(), error.into());
        if let Some((h, body)) = resp {
            for (field, names) in EVIDENCE {
                let v: Vec<&str> = names
                    .iter()
                    .filter_map(|n| h.get(*n).map(|s| s.as_str()).filter(|s| !s.is_empty()))
                    .collect();
                r.insert((*field).into(), v.join("; ").chars().take(200).collect());
            }
            r.insert(
                "content_type".into(),
                h.get("content-type").cloned().unwrap_or_default().chars().take(100).collect(),
            );
            let txt = String::from_utf8_lossy(&body[..body.len().min(4000)]).to_string();
            let t = regex::Regex::new(r"(?is)<title>(.*?)</title>")
                .unwrap()
                .captures(&txt)
                .map(|c| c[1].to_string());
            let t = t.unwrap_or_else(|| regex::Regex::new(r"(?s)<[^>]+>").unwrap().replace_all(&txt, " ").to_string());
            r.insert(
                "response_text".into(),
                t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect(),
            );
        }
        self.proto.append_attempt(&r);
    }

    fn call_recorded(&mut self, call: &str, url: &str, doc: &Value, extra: &Value, body: &str, timeout: u64) -> RResult<ApiResp> {
        let t0 = Instant::now();
        match self.api_call("POST", url, extra, body, timeout, false) {
            Ok(r) => {
                if !r.err.is_empty() {
                    let msg = if r.err == "timeout" {
                        format!("TimeoutError: {call} did not answer within {timeout} s")
                    } else {
                        format!("Error: {}", r.err)
                    };
                    self.record_attempt(call, doc, None, t0.elapsed().as_secs_f64(), None, &crate::log::short_error(&msg));
                    return Err(RunErr::Other(msg));
                }
                if r.status != 200 {
                    let h = r.headers.clone();
                    self.record_attempt(
                        call,
                        doc,
                        Some(r.status),
                        t0.elapsed().as_secs_f64(),
                        Some((&h, r.text.as_bytes())),
                        "",
                    );
                }
                Ok(r)
            }
            Err(e) => {
                if !e.is_fatal() {
                    self.record_attempt(call, doc, None, t0.elapsed().as_secs_f64(), None, &e.short());
                }
                Err(e)
            }
        }
    }

    fn dl_browser(&mut self, call: &str, doc: &Value, body: &str, p_archivo: &str) -> RResult<DlResp> {
        let a = json!({"url": self.cfg.download_api_url, "headers": {"p_archivo": p_archivo}, "body": body,
            "answer": self.cfg.doc_timeout, "silence": self.cfg.doc_silence});
        let t0 = Instant::now();
        let id = self.js(&format!("FJ.dlStart({a})"))?.as_str().unwrap_or("").to_string();
        let mut buf: Vec<u8> = Vec::new();
        let r = loop {
            let r = match self.js_opt(&format!("FJ.dlTake({},{})", js(&id), crate::defaults::timing::CHUNK_BYTES)) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    if t0.elapsed() > Duration::from_secs(self.cfg.doc_timeout + self.cfg.doc_silence + 600) {
                        let _ = self.js_opt(&format!("FJ.dlAbort({})", js(&id)));
                        return Err(RunErr::Other(format!("{call}: the page did not answer")));
                    }
                    self.wait_ms(250)?;
                    continue;
                }
                Err(e) => {
                    let _ = self.portal.call(&format!("FJ.dlAbort({})", js(&id)));
                    return Err(e);
                }
            };
            if r.get("gone").is_some() {
                return Err(RunErr::Other("the download disappeared in the browser (page reloaded?)".into()));
            }
            if let Some(b) = r["b64"].as_str().filter(|b| !b.is_empty()) {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b) {
                    buf.extend(bytes);
                }
            }
            if r["end"].as_bool().unwrap_or(false) {
                break r;
            }
            if let Err(e) = self.keepalive() {
                let _ = self.portal.call(&format!("FJ.dlAbort({})", js(&id)));
                return Err(e);
            }
            self.wait_ms(250)?;
        };
        let dur = t0.elapsed().as_secs_f64();
        let err = r["err"].as_str().unwrap_or("").to_string();
        let status = r["status"].as_u64().unwrap_or(0) as u16;
        let headers: HashMap<String, String> = r["headers"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").into())).collect())
            .unwrap_or_default();
        if !err.is_empty() {
            let (msg, gw) = if err == "answer" {
                (
                    format!(
                        "{call}: the portal did not begin to send the file within {} s",
                        self.cfg.doc_timeout
                    ),
                    true,
                )
            } else if err == "silence" {
                (
                    format!(
                        "{call}: no data for {} s while the file was coming ({:.1} MB received)",
                        self.cfg.doc_silence,
                        buf.len() as f64 / 1048576.0
                    ),
                    false,
                )
            } else if status == 0 && buf.is_empty() && dur >= 45.0 {
                (
                    format!("{call}: no answer - the portal's gateway ended the request after {dur:.0} s"),
                    true,
                )
            } else {
                (format!("{call}: {}", err.chars().take(150).collect::<String>()), false)
            };
            self.record_attempt(call, doc, if status > 0 { Some(status) } else { None }, dur, None, &msg);
            return Err(if gw { RunErr::Gateway(msg) } else { RunErr::Other(msg) });
        }
        let ctype = r["ctype"].as_str().unwrap_or("").to_string();
        let cdisp = r["cdisp"].as_str().unwrap_or("").to_string();
        if status != 200 {
            let mut h = headers.clone();
            h.insert("content-type".into(), ctype.clone());
            self.record_attempt(call, doc, Some(status), dur, Some((&h, &buf)), "");
        } else {
            let head = r["head"].as_f64().unwrap_or(0.0);
            if buf.len() > 5 * 1048576 || head > 10.0 {
                self.info(format!(
                    "      download: the portal began after {head:.1} s, {:.1} MB in {:.1} s, longest pause {:.1} s",
                    buf.len() as f64 / 1048576.0,
                    dur - head,
                    r["gap"].as_f64().unwrap_or(0.0)
                ));
            }
        }
        Ok(DlResp {
            status,
            ctype,
            cdisp,
            data: buf,
        })
    }

    fn download_document(&mut self, carpeta: Option<&Value>, contexto: &Value, doc: &Value) -> RResult<(Value, Vec<u8>)> {
        let siagpj = truthy(doc.get("provieneSIAGPJ")) || (truthy(contexto.get("provieneSIAGPJ")) && !truthy(carpeta));
        let codigo = contexto.get("codigo").cloned().unwrap_or(Value::Null);
        if siagpj {
            let body = format!(
                "{{\"contexto\":{{\"codigo\":{},\"provieneSIAGPJ\":true}}}}",
                crate::pyfmt::dumps(&codigo, crate::pyfmt::Indent::Compact, true)
            );
            let mut finfo = serde_json::Map::new();
            finfo.insert("identificador".into(), doc.get("identificador").cloned().unwrap_or(Value::Null));
            finfo.insert("nombreArchivo".into(), json!(py_or_empty(doc.get("descripcion"))));
            let p_archivo = crate::pyfmt::dumps(&Value::Object(finfo.clone()), crate::pyfmt::Indent::Compact, true);
            self.ensure_session_fresh()?;
            let r = self.dl_browser("DescargarArchivo", doc, &body, &p_archivo)?;
            if r.status == 401 || r.status == 403 {
                return Err(RunErr::SessionLost(format!("HTTP {} on DescargarArchivo (SIAGPJ)", r.status)));
            }
            if r.status == 504 {
                return Err(RunErr::Gateway(format!("DescargarArchivo (SIAGPJ) HTTP {}", r.status)));
            }
            if r.status != 200 {
                return Err(RunErr::Other(format!("DescargarArchivo (SIAGPJ) HTTP {}", r.status)));
            }
            let ct = r.ctype.to_lowercase();
            if ct.contains("json") || r.data.is_empty() {
                let msg = String::from_utf8_lossy(&r.data[..r.data.len().min(300)]).to_string();
                return Err(RunErr::DocNoData(format!(
                    "portal delivered no file ({ct}): {msg}; document of the SIAGPJ system"
                )));
            }
            finfo.insert("extension".into(), json!(ext_from_response(&ct, &r.cdisp)));
            finfo.insert("original_name".into(), json!(py_or_empty(doc.get("identificador"))));
            return Ok((Value::Object(finfo), r.data));
        }
        let prov = contexto.get("provieneSIAGPJ").cloned().unwrap_or(json!(false));
        let body = format!(
            "{{\"carpeta\":{},\"contexto\":{{\"codigo\":{},\"provieneSIAGPJ\":{}}}}}",
            crate::pyfmt::dumps(carpeta.unwrap_or(&Value::Null), crate::pyfmt::Indent::Compact, true),
            crate::pyfmt::dumps(&codigo, crate::pyfmt::Indent::Compact, true),
            crate::pyfmt::dumps(&prov, crate::pyfmt::Indent::Compact, true)
        );
        self.ensure_session_fresh()?;
        let url = format!("{}/{}", self.cfg.api_base_url, self.cfg.api["api_document_info"]);
        let extra = json!({"p_identificador": crate::pyfmt::py_str(doc.get("identificador"))});
        let r1 = self.call_recorded("ConsultarDocumento", &url, doc, &extra, &body, self.cfg.doc_timeout)?;
        if r1.status == 401 || r1.status == 403 {
            return Err(RunErr::SessionLost(format!("HTTP {} on ConsultarDocumento", r1.status)));
        }
        if r1.status == 504 {
            return Err(RunErr::Gateway(format!("ConsultarDocumento HTTP {}", r1.status)));
        }
        if r1.status != 200 {
            return Err(RunErr::Other(format!("ConsultarDocumento HTTP {}", r1.status)));
        }
        let j: Value = serde_json::from_str(&r1.text).unwrap_or(json!({}));
        if truthy(j.get("huboError")) || !truthy(j.get("valorDevuelto")) {
            let sys = if truthy(doc.get("provieneSIAGPJ")) { "SIAGPJ" } else { "standard" };
            let m = py_or_empty(j.get("mensajeUsuario"));
            let m = if m.is_empty() { "no data".to_string() } else { m };
            return Err(RunErr::DocNoData(format!(
                "portal delivered no file data ({m}; document of the {sys} system)"
            )));
        }
        let finfo = j["valorDevuelto"].clone();
        let p_archivo = crate::pyfmt::dumps(&finfo, crate::pyfmt::Indent::Compact, true);
        let r2 = self.dl_browser("DescargarArchivo", doc, &body, &p_archivo)?;
        if r2.status == 401 || r2.status == 403 {
            return Err(RunErr::SessionLost(format!("HTTP {} on DescargarArchivo", r2.status)));
        }
        if r2.status == 504 {
            return Err(RunErr::Gateway(format!("DescargarArchivo HTTP {}", r2.status)));
        }
        if r2.status != 200 {
            return Err(RunErr::Other(format!("DescargarArchivo HTTP {}", r2.status)));
        }
        let ct = r2.ctype.to_lowercase();
        if ct.contains("json") || r2.data.is_empty() {
            let msg = String::from_utf8_lossy(&r2.data[..r2.data.len().min(300)]).to_string();
            return Err(RunErr::Other(format!("DescargarArchivo returned no file ({ct}): {msg}")));
        }
        Ok((finfo, r2.data))
    }

    fn name_used_by_other_document(&self, target: &Path, doc_id: &str) -> bool {
        let t = crate::protocol::norm_path_key(target);
        self.proto.ok.values().any(|r| {
            let p = r.get("target_path").map(|s| s.as_str()).unwrap_or("");
            !p.is_empty()
                && crate::protocol::norm_path_key(Path::new(p)) == t
                && r.get("doc_id").map(|s| s.as_str()).unwrap_or("") != doc_id
        })
    }

    fn file_name(&self, case_dir: &Path, c: &Card, doc: &Value, pdt: Option<NaiveDateTime>, ext: &str, original: &str) -> String {
        let est = {
            let a = py_or_empty(doc.get("descripcionEstado"));
            if a.is_empty() {
                py_or_empty(doc.get("estadoDocumento"))
            } else {
                a
            }
        };
        let (clas, id, mut desc) = (
            py_or_empty(doc.get("clasificacion")),
            py_or_empty(doc.get("identificador")),
            py_or_empty(doc.get("descripcion")),
        );

        if desc.trim().is_empty() {
            desc = NO_TITLE_FILE.into();
        }
        let inp = NameInput {
            pattern: &self.cfg.filename_pattern,
            keep_underscores: self.cfg.keep_underscores,
            description_max_chars: self.cfg.description_max_chars,
            original_name_max_chars: self.cfg.original_name_max_chars,
            max_path_length: self.cfg.max_path_length,
            case_dir_len: case_dir.to_string_lossy().chars().count(),
            case_number: &c.number,
            kind: &c.kind,
            clasificacion: &clas,
            estado: &est,
            doc_id: &id,
            descripcion: &desc,
            portal_dt: pdt,
            original_name: original,
        };
        build_filename(&inp, ext)
    }

    pub fn process_document(
        &mut self,
        c: &Card,
        carpeta: Option<&Value>,
        contexto: &Value,
        doc: &Value,
        case_dir: &Path,
    ) -> RResult<String> {
        self.current_doc_ctx = (c.number.clone(), carpeta_key(carpeta));
        /* removed in the public source: validation / licence check · eliminado en el código público: validación / licencia */
        let pdt = parse_portal_date(&py_or_empty(doc.get("fechaCreacion")));
        let mut row = self.doc_row(c, carpeta, doc);
        let doc_id = sv(doc.get("identificador"));
        let carpeta_txt = match carpeta {
            None | Some(Value::Null) => String::new(),
            Some(v) => crate::pyfmt::py_str(Some(v)),
        };
        let mut last_err = String::new();
        let mut no_data = false;
        let mut portal_ok = false;

        self.space_ok(0)?;

        self.session_room(60 + self.dl_max_secs.min(300.0) as i64)?;
        for attempt in 1..=self.cfg.doc_retries + 1 {
            self.keepalive()?;
            self.activity_tick()?;
            let t_dl = std::time::Instant::now();
            let got = self.download_document(carpeta, contexto, doc);
            if let Ok((_, data)) = &got {
                let secs = t_dl.elapsed().as_secs_f64();
                self.dl_max_secs = self.dl_max_secs.max(secs);
                if secs > 0.5 && !data.is_empty() {
                    let r = data.len() as f64 / secs;
                    self.dl_rate = if self.dl_rate > 0.0 { 0.7 * self.dl_rate + 0.3 * r } else { r };
                }
            }
            match got {
                Ok((finfo, data)) => {
                    let original = {
                        let a = py_or_empty(finfo.get("original_name"));
                        if a.is_empty() {
                            py_or_empty(finfo.get("nombreArchivo"))
                        } else {
                            a
                        }
                    };
                    let portal_ext = {
                        let e = py_or_empty(finfo.get("extension"));
                        if !e.is_empty() {
                            e
                        } else {
                            let (_, x) = splitext(&original);
                            if x.is_empty() {
                                ".pdf".into()
                            } else {
                                x
                            }
                        }
                    };
                    let (ext, type_name) = detect_file_type(&data, &portal_ext);
                    if type_name != "unknown" && ext.to_lowercase() != format!(".{}", portal_ext.trim_start_matches('.')).to_lowercase() {
                        self.info(format!(
                            "      file type from content: {type_name} - the portal says '{portal_ext}', the file will be saved as '{ext}'"
                        ));
                    } else if type_name == "unknown" {
                        self.warn(format!(
                            "      file type not recognised from content - the portal's extension '{portal_ext}' is used"
                        ));
                    }
                    let mut name = self.file_name(case_dir, c, doc, pdt, &ext, &original);
                    let mut target = case_dir.join(&name);
                    let digest = sha256_hex(&data);
                    let mut status = "OK";
                    if target.exists() && self.name_used_by_other_document(&target, &doc_id) {
                        let (b, e) = splitext(&name);
                        let id = if doc_id.is_empty() { "NOID".to_string() } else { doc_id.clone() };
                        name = format!("{b}_{}{e}", sanitize(&id, Some(40), false));
                        target = case_dir.join(&name);
                        self.info(format!(
                            "      file name already used by another document - document id added ({})",
                            self.id_doc(&doc_id)
                        ));
                    }
                    if target.exists() {
                        if std::fs::read(&target).map(|b| sha256_hex(&b) == digest).unwrap_or(false) {
                            status = "OK_SAME_FILE";
                        } else {
                            let (b, e) = splitext(&name);
                            name = format!("{b}_redownload-{}{e}", ts_file(&chrono::Local::now()));
                            target = case_dir.join(&name);
                        }
                    }
                    if status == "OK" {
                        let mut part = target.as_os_str().to_owned();
                        part.push(".part");
                        let part = std::path::PathBuf::from(part);
                        self.space_ok(data.len() as u64)?;
                        if let Err(e) = std::fs::write(&part, &data).and_then(|_| std::fs::rename(&part, &target)) {
                            if crate::disk::is_full(&e) {
                                let _ = std::fs::remove_file(&part);
                                return Err(RunErr::DiskFull(format!("{e}")));
                            }
                            last_err = crate::log::short_error(&format!("OSError: {e}"));
                            self.warn(format!("      Attempt {attempt} failed: {last_err} (document {doc_id})"));
                            continue;
                        }
                        if self.cfg.set_file_time {
                            if let Some(p) = pdt {
                                if let Some(ep) = crate::text::portal_epoch(&p, &self.cfg.portal_tz) {
                                    set_file_times(&target, ep);
                                }
                            }
                        }
                    }
                    for (k, v) in [
                        ("original_filename", original.clone()),
                        ("detected_type", type_name.clone()),
                        ("new_filename", name.clone()),
                        ("target_path", target.to_string_lossy().to_string()),
                        ("download_time", ts_human(&chrono::Local::now())),
                        ("size_bytes", data.len().to_string()),
                        ("sha256", digest.clone()),
                        ("status", status.into()),
                        ("error", String::new()),
                    ] {
                        row.insert(k.into(), v);
                    }
                    self.proto.append(row);

                    if !doc_id.is_empty() {
                        if let Err(e) = crate::read_state::mark(&self.cfg.read_state_file, "doc", std::slice::from_ref(&doc_id), false) {
                            self.warn(format!("      could not mark the document as unread: {e}"));
                        }

                        crate::read_state::add_present(&self.cfg.read_state_file, &doc_id);
                    }
                    self.note_success();
                    self.docs_downloaded += 1;
                    self.failmem.ok("docs", &FailMemory::doc_key(&carpeta_txt, &doc_id));
                    if status == "OK" {
                        self.pages.count(&target);
                        self.new_docs += 1;
                    }
                    let (_, e) = splitext(&name);
                    let dn = format!("{}{}", self.id_doc(&doc_id), e.to_lowercase());
                    let st_w = crate::i18n::tr(status);
                    self.downloads.push(format!(
                        "{}  {:<12} {}  {dn}",
                        crate::pyfmt::now_human(),
                        st_w,
                        self.id_case(&c.number)
                    ));
                    self.info(format!("      {status}: {dn}"));
                    if status == "OK" {
                        self.live_tick();
                    }
                    return Ok(status.into());
                }
                Err(RunErr::DocNoData(m)) => {
                    last_err = crate::log::short_error(&format!("DocNoData: {m}"));
                    no_data = true;
                    self.warn(format!(
                        "      No file data for document {doc_id}: {m} - it will not be retried in this run, but it will be tried again in the next full run"
                    ));
                    break;
                }
                Err(RunErr::SessionLost(m)) => {
                    last_err = crate::log::short_error(&format!("SessionLost: {m}"));
                    self.warn(format!("      Attempt {attempt} - session problem: {last_err}"));
                    match self.refresh_session() {
                        Ok(()) => {}
                        Err(e) if e.is_fatal() => return Err(e),
                        Err(e) => self.warn(format!("      session renewal failed: {}", e.short())),
                    }
                }
                Err(e) if e.is_fatal() => return Err(e),
                Err(e) => {
                    last_err = e.short();
                    let gateway = matches!(e, RunErr::Gateway(_));
                    if gateway {
                        self.warn(format!(
                            "      Attempt {attempt} failed: {last_err} (document {doc_id}) - the portal does not answer this document; it will not be retried in this run, the next update tries it again"
                        ));
                    } else {
                        self.warn(format!("      Attempt {attempt} failed: {last_err} (document {doc_id})"));
                    }
                    portal_ok = self.portal_answers()?;
                    if portal_ok {
                        self.info("      portal check: the portal answers normally - the problem is this document");
                    } else {
                        self.warn("      portal check: the portal does NOT answer normally");
                    }
                    if gateway && portal_ok {
                        self.gw_failed.insert(doc_id.clone());
                        break;
                    }
                }
            }
            if attempt <= self.cfg.doc_retries {
                self.sleep(self.cfg.retry_wait)?;
            }
        }
        if !no_data && !portal_ok {
            self.note_error(&format!("doc {doc_id}: {last_err}"))?;
        }
        for (k, v) in [
            ("original_filename", String::new()),
            ("detected_type", String::new()),
            ("new_filename", String::new()),
            ("target_path", String::new()),
            ("download_time", crate::pyfmt::now_human()),
            ("size_bytes", String::new()),
            ("sha256", String::new()),
            ("status", "FAILED".into()),
            ("error", last_err.chars().take(500).collect()),
        ] {
            row.insert(k.into(), v);
        }
        self.proto.append(row);
        let desc = py_or_empty(doc.get("descripcion"));
        self.failmem.failed(
            "docs",
            &FailMemory::doc_key(&carpeta_txt, &doc_id),
            &[("case", &c.number), ("description", &desc)],
        );
        let line = format!(
            "{}  {:<12} {}  {}  ({})",
            crate::pyfmt::now_human(),
            crate::i18n::tr("FAILED"),
            self.id_case(&c.number),
            self.id_doc(&doc_id),
            last_err.chars().take(120).collect::<String>()
        );
        self.downloads.push(line);
        Ok("FAILED".into())
    }

    pub fn final_retry(&mut self) -> RResult<()> {
        if self.failed.is_empty() || !self.cfg.final_retry_pass || self.cfg.dry_run {
            return Ok(());
        }
        let (gw, todo): (Vec<_>, Vec<_>) = self
            .failed
            .clone()
            .into_iter()
            .partition(|f| self.gw_failed.contains(&sv(f.doc.get("identificador"))));
        self.info("");
        if !gw.is_empty() {
            self.info(format!(
                "Final retry pass: {} document(s) left out - the portal did not answer them in this run (the next update tries them again)",
                gw.len()
            ));
        }
        if todo.is_empty() {
            return Ok(());
        }
        self.info(format!("Final retry pass for {} failed document(s) ...", todo.len()));
        self.live_phase = "retry".into();
        self.live_progress = (todo[0].info.number.clone(), 0, todo.len());
        self.live_report(true);
        let mut still = gw.clone();
        let n = todo.len();
        for (i, f) in todo.into_iter().enumerate() {
            self.live_progress = (f.info.number.clone(), i + 1, n);
            self.write_live_status(false, true, false, "");
            let cp = if f.carpeta.is_null() { None } else { Some(f.carpeta.clone()) };
            if self.is_done(cp.as_ref(), &f.doc) {
                continue;
            }
            let st = self.process_document(&f.info, cp.as_ref(), &f.contexto, &f.doc, &f.case_dir)?;
            if ["OK", "OK_SAME_FILE", "OK_EXISTING"].contains(&st.as_str()) {
                let e = &mut self.entries[f.entry];
                e.new += 1;
                e.failed = e.failed.saturating_sub(1);
            } else {
                still.push(f);
            }
            self.sleep(self.cfg.delay_between_downloads)?;
        }
        for i in 0..self.entries.len() {
            let s = self.entries[i].clone();
            let Some(cp) = s.carpeta.clone().filter(|v| !v.is_null()) else {
                continue;
            };
            let cpo = Some(cp);
            let missing: Vec<Value> = s.missing.iter().filter(|d| !self.is_done(cpo.as_ref(), d)).cloned().collect();
            let e = &mut self.entries[i];
            e.ok_total = e.portal.saturating_sub(missing.len()).saturating_sub(e.before_date);
            e.already = e.ok_total.saturating_sub(e.new);
            if missing.is_empty() {
                e.status = "COMPLETE".into();
            }
            e.missing = missing;
        }
        self.failed = still;
        self.live_phase.clear();
        self.live_progress = (String::new(), 0, 0);
        Ok(())
    }
}

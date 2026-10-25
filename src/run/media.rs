use super::{Card, Fetcher, RResult, RunErr};
use crate::bridge::js;
use crate::media::{self, MediaRec};
use crate::protocol::row;
use crate::text::{build_filename, detect_file_type, NameInput};
use base64::Engine;
use serde_json::json;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const PLAYER_URL: &str = "https://aptpj.poder-judicial.go.cr/cpglco/ReproducirAudiencia";

const ANSWER_S: u64 = 120;
const SILENCE_S: u64 = 90;

const GAP_S: f64 = 5.0;

struct Got {
    status: u16,
    ctype: String,
    url: String,
    bytes: u64,

    data: Vec<u8>,
}

impl Fetcher {
    fn media_get(&mut self, url: &str, out: Option<&Path>) -> RResult<Got> {
        let a = json!({"url": url, "method": "GET", "answer": ANSWER_S, "silence": SILENCE_S});
        let t0 = Instant::now();
        let id = self.js(&format!("FJ.dlStart({a})"))?.as_str().unwrap_or("").to_string();
        let mut file = match out {
            Some(p) => Some(std::fs::File::create(p).map_err(|e| RunErr::Other(format!("cannot write {}: {e}", p.display())))?),
            None => None,
        };
        let mut data: Vec<u8> = Vec::new();
        let mut bytes = 0u64;
        let mut last_log = Instant::now();
        let mut checked = 0u64;

        let drop_out = |out: Option<&Path>| {
            if let Some(p) = out {
                let _ = std::fs::remove_file(p);
            }
        };
        let r = loop {
            let r = match self.js_opt(&format!("FJ.dlTake({},{})", js(&id), 4 * 1024 * 1024)) {
                Ok(Some(r)) => r,
                Ok(None) => {
                    if t0.elapsed() > Duration::from_secs(ANSWER_S + SILENCE_S + 900) {
                        let _ = self.js_opt(&format!("FJ.dlAbort({})", js(&id)));
                        return Err(RunErr::Other("the page did not answer".into()));
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
            let mut got = false;
            if let Some(b) = r["b64"].as_str().filter(|b| !b.is_empty()) {
                if let Ok(chunk) = base64::engine::general_purpose::STANDARD.decode(b) {
                    got = true;
                    bytes += chunk.len() as u64;
                    match file.as_mut() {
                        Some(f) => {
                            if data.len() < 64 {
                                data.extend(chunk.iter().take(64 - data.len()));
                            }
                            if let Err(e) = f.write_all(&chunk) {
                                let _ = self.portal.call(&format!("FJ.dlAbort({})", js(&id)));
                                if crate::disk::is_full(&e) {
                                    drop(file.take());
                                    drop_out(out);
                                    return Err(RunErr::DiskFull(format!("{e}")));
                                }
                                return Err(RunErr::Other(format!("cannot write the file: {e}")));
                            }

                            if bytes - checked >= 50 * 1048576 {
                                checked = bytes;
                                if let Err(e) = self.space_ok(bytes) {
                                    let _ = self.portal.call(&format!("FJ.dlAbort({})", js(&id)));
                                    drop(file.take());
                                    drop_out(out);
                                    return Err(e);
                                }
                            }
                        }
                        None => data.extend(chunk),
                    }
                }
            }
            if r["end"].as_bool().unwrap_or(false) {
                break r;
            }
            if last_log.elapsed() > Duration::from_secs(60) && out.is_some() {
                last_log = Instant::now();
                self.info(format!("      {:.0} MB received ...", bytes as f64 / 1048576.0));
            }
            if let Err(e) = self.keepalive() {
                let _ = self.portal.call(&format!("FJ.dlAbort({})", js(&id)));
                return Err(e);
            }
            if !got {
                if let Err(e) = self.wait_ms(250) {
                    let _ = self.portal.call(&format!("FJ.dlAbort({})", js(&id)));
                    return Err(e);
                }
            } else {
                self.check_stop().inspect_err(|_| {
                    let _ = self.portal.call(&format!("FJ.dlAbort({})", js(&id)));
                })?;
            }
        };
        if let Some(f) = file.as_mut() {
            let _ = f.flush();
        }
        let err = r["err"].as_str().unwrap_or("").to_string();
        if !err.is_empty() {
            let msg = match err.as_str() {
                "answer" => format!("the server did not begin to send within {ANSWER_S} s"),
                "silence" => format!("no data for {SILENCE_S} s ({:.1} MB received)", bytes as f64 / 1048576.0),
                e => e.chars().take(150).collect(),
            };
            return Err(RunErr::Other(msg));
        }
        Ok(Got {
            status: r["status"].as_u64().unwrap_or(0) as u16,
            ctype: r["ctype"].as_str().unwrap_or("").to_lowercase(),
            url: r["url"].as_str().unwrap_or("").to_string(),
            bytes,
            data,
        })
    }

    fn media_refused(g: &Got, body: &[u8]) -> Option<String> {
        if g.status == 200 && !g.ctype.contains("json") && !g.ctype.contains("html") && g.bytes > 0 {
            return None;
        }
        let txt = String::from_utf8_lossy(&body[..body.len().min(4000)]).to_string();
        let msg = serde_json::from_str::<serde_json::Value>(&txt)
            .ok()
            .and_then(|j| {
                j.get("MensajeUsuario")
                    .or_else(|| j.get("mensajeUsuario"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        Some(if msg.is_empty() {
            format!(
                "HTTP {} ({})",
                g.status,
                if g.ctype.is_empty() { "no content type" } else { &g.ctype }
            )
        } else {
            format!("HTTP {}: {msg}", g.status)
        })
    }

    fn media_name(&self, case_dir: &Path, r: &MediaRec, ext: &str) -> String {
        let doc_id = format!("Aud-{}", r.codigo);
        let original = if r.av() == "audio" { r.file.clone() } else { String::new() };
        let label = r.label();
        let inp = NameInput {
            pattern: &self.cfg.filename_pattern,
            keep_underscores: self.cfg.keep_underscores,
            description_max_chars: self.cfg.description_max_chars,
            original_name_max_chars: self.cfg.original_name_max_chars,
            max_path_length: self.cfg.max_path_length,
            case_dir_len: case_dir.to_string_lossy().chars().count(),
            case_number: &r.case,
            kind: &r.kind,
            clasificacion: "Audiencia",
            estado: &r.tipo,
            doc_id: &doc_id,
            descripcion: &label,
            portal_dt: r.date(),
            original_name: &original,
        };
        build_filename(&inp, ext)
    }

    fn fetch_media(&mut self, r: &MediaRec) -> RResult<(PathBuf, String)> {
        let case_dir = self.cfg.case.dir.join(&r.case);
        std::fs::create_dir_all(&case_dir).map_err(|e| RunErr::Other(format!("case folder: {e}")))?;
        let tmp = self.cfg.protocol_dir.join("_media_tmp");
        let _ = std::fs::create_dir_all(&tmp);

        let player = Some(crate::country::meta("player_url"))
            .filter(|u| u.starts_with("https://"))
            .unwrap_or_else(|| PLAYER_URL.to_string());
        let url = format!("{player}?P_EsM3u8=true&P_NombreArchivo={}", media::enc(&r.file));
        if r.av() != "av" {
            let part = tmp.join(format!("aud_{}.part", crate::text::sanitize(&r.codigo, Some(30), false)));
            let g = self.media_get(&url, Some(&part))?;
            if let Some(m) = Self::media_refused(&g, &std::fs::read(&part).unwrap_or_default()) {
                let _ = std::fs::remove_file(&part);
                return Err(RunErr::Other(m));
            }
            let (ext, _) = detect_file_type(&g.data, r.ext());
            let ext = if [".mp3", ".wav", ".m4a", ".ogg", ".mp4"].contains(&ext.as_str()) {
                ext
            } else {
                r.ext().to_string()
            };
            let target = move_file(&part, &case_dir.join(self.media_name(&case_dir, r, &ext)))?;
            self.info(format!("      {:.1} MB", g.bytes as f64 / 1048576.0));
            return Ok((target, String::new()));
        }

        let m = self.media_get(&url, None)?;
        if let Some(e) = Self::media_refused(&m, &m.data) {
            return Err(RunErr::Other(e));
        }
        let master = String::from_utf8_lossy(&m.data).to_string();
        let base = if m.url.is_empty() { url.clone() } else { m.url.clone() };
        let (vl, al) = media::parse_master(&master);
        let (Some(vl), Some(al)) = (vl, al) else {
            return Err(RunErr::Other("the portal's list has no picture and sound lists".into()));
        };
        let mut files: Vec<String> = Vec::new();
        for l in [&vl, &al] {
            let lu = media::resolve(&base, l);
            self.sleep(1.0)?;
            let g = self.media_get(&lu, None)?;
            if let Some(e) = Self::media_refused(&g, &g.data) {
                return Err(RunErr::Other(e));
            }
            let Some(f) = media::single_file(&String::from_utf8_lossy(&g.data)) else {
                return Err(RunErr::Other("the recording is split into many pieces - not supported yet".into()));
            };
            files.push(media::resolve(&lu, &f));
        }
        let vpart = tmp.join(format!("vid_{}.part", crate::text::sanitize(&r.codigo, Some(30), false)));
        let apart = tmp.join(format!("snd_{}.part", crate::text::sanitize(&r.codigo, Some(30), false)));
        self.sleep(GAP_S)?;
        self.info("      picture ...");
        let gv = self.media_get(&files[0], Some(&vpart))?;
        if let Some(e) = Self::media_refused(&gv, &[]) {
            let _ = std::fs::remove_file(&vpart);
            return Err(RunErr::Other(e));
        }
        self.sleep(GAP_S)?;
        self.info("      sound ...");
        let ga = self.media_get(&files[1], Some(&apart)).inspect_err(|_| {
            let _ = std::fs::remove_file(&vpart);
        })?;
        if let Some(e) = Self::media_refused(&ga, &[]) {
            let _ = std::fs::remove_file(&vpart);
            let _ = std::fs::remove_file(&apart);
            return Err(RunErr::Other(e));
        }
        self.info(format!(
            "      {:.1} MB picture + {:.1} MB sound",
            gv.bytes as f64 / 1048576.0,
            ga.bytes as f64 / 1048576.0
        ));
        let target = case_dir.join(self.media_name(&case_dir, r, ".mp4"));
        if let Some(ff) = media::ffmpeg(&media::app_data()) {
            let merged = tmp.join(format!("av_{}.mp4", crate::text::sanitize(&r.codigo, Some(30), false)));
            let _ = std::fs::remove_file(&merged);
            let mut cmd = std::process::Command::new(&ff);
            cmd.args(["-hide_banner", "-v", "error", "-y", "-i"])
                .arg(&vpart)
                .arg("-i")
                .arg(&apart);
            cmd.args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy", "-movflags", "+faststart"])
                .arg(&merged);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            match cmd.output() {
                Ok(o) if o.status.success() && merged.is_file() => {
                    let target = move_file(&merged, &target)?;
                    let _ = std::fs::remove_file(&vpart);
                    let _ = std::fs::remove_file(&apart);
                    self.info("      picture and sound put into one file with ffmpeg (-c copy, nothing changed)");
                    return Ok((target, String::new()));
                }
                Ok(o) => self.warn(format!(
                    "      ffmpeg could not put picture and sound together - both files are kept as they came ({})",
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(150)
                        .collect::<String>()
                )),
                Err(e) => self.warn(format!(
                    "      ffmpeg could not be started - both files are kept as they came ({e})"
                )),
            }
            let _ = std::fs::remove_file(&merged);
        }

        let (stem, _) = crate::text::splitext(&target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
        let atarget = case_dir.join(format!("{stem}_audio.m4a"));
        let target = move_file(&vpart, &target)?;
        let atarget = move_file(&apart, &atarget)?;
        Ok((target, atarget.to_string_lossy().to_string()))
    }

    pub fn media_pass(&mut self) -> RResult<()> {
        if !self.cfg.download_media || self.cfg.dry_run || self.cfg.list_only {
            return Ok(());
        }
        let today_lim = self.cfg.docs_from_date;
        let todo: Vec<MediaRec> = self
            .media
            .recs
            .values()
            .filter(|r| r.state != "archive" && r.state != "court" && !r.present() && !r.file.trim().is_empty())
            .filter(|r| {
                !(r.state == "missing" || r.state == "failed")
                    || self.cfg.retry_paused
                    || !r.paused(self.cfg.skip_after_failed_runs, self.cfg.retry_skipped_after_days)
            })
            .filter(|r| {
                let mode = self.cfg.entry_mode(&r.case, &r.kind, &r.entry_date, &r.office);

                if mode == "skip" || !self.cfg.media_on(&r.case, &r.kind, &r.entry_date, &r.office) {
                    return false;
                }
                match (today_lim, r.date()) {
                    (Some(l), Some(d)) if mode != "full" => d.date() >= l,
                    _ => true,
                }
            })
            .cloned()
            .collect();
        if todo.is_empty() {
            return Ok(());
        }
        let mut todo = todo;
        todo.sort_by(|a, b| a.created.cmp(&b.created));
        self.info("");
        self.info(format!(
            "Recordings (audiencias): {} to download - one after the other, after the documents",
            todo.len()
        ));
        self.live_phase = "media".into();
        let n = todo.len();

        let mut seen: HashMap<(String, String), (usize, Result<(PathBuf, String), String>)> = HashMap::new();
        for (i, mut r) in todo.into_iter().enumerate() {
            self.live_progress = (r.case.clone(), i + 1, n);
            self.write_live_status(false, true, false, "");

            if let Err(e) = self.space_ok(r.size.max(50 * 1048576)) {
                self.media.recs.insert(r.key(), r);
                self.media.save();
                return Err(e);
            }
            let fk = (r.case.trim().to_uppercase(), r.file.trim().to_string());
            let cached = seen.get(&fk).cloned();
            if cached.is_none() {
                if r.size > 0 {
                    let rate = if self.dl_rate > 0.0 { self.dl_rate } else { 1_048_576.0 };
                    let need = (60.0 + r.size as f64 / rate) as i64;
                    if let Err(e) = self.session_room(need.min(self.cfg.session_max_min as i64 * 30)) {
                        self.media.recs.insert(r.key(), r);
                        self.media.save();
                        return Err(e);
                    }
                }

                self.sleep(GAP_S)?;
                self.keepalive()?;
                self.ensure_session_fresh()?;
            }
            self.info(format!(
                "   recording {} of {n}: {} · {} · {}",
                i + 1,
                self.id_case(&r.case),
                r.label(),
                r.created.chars().take(16).collect::<String>().replace('T', " ")
            ));

            r.last_try = crate::pyfmt::now_human();
            let dup = cached.is_some();
            let res: RResult<(PathBuf, String)> = match cached {
                Some((k, v)) => {
                    self.info(format!(
                        "      same file as recording {} of this update - its result is taken over",
                        k + 1
                    ));
                    v.map_err(RunErr::Other)
                }
                None => {
                    let mut res = self.fetch_media(&r);

                    if let Err(e) = &res {
                        if !e.is_fatal() && !media::refused(&e.short()) {
                            self.info(format!(
                                "      no answer from the server ({}) - one more try in {:.0} s",
                                e.short(),
                                self.cfg.retry_wait
                            ));
                            self.sleep(self.cfg.retry_wait)?;
                            res = self.fetch_media(&r);
                        }
                    }
                    if !matches!(&res, Err(e) if e.is_fatal()) {
                        seen.insert(fk, (i, res.as_ref().map(|v| v.clone()).map_err(|e| e.short())));
                    }
                    res
                }
            };
            match res {
                Ok((path, audio)) => {
                    let ep = r.date().and_then(|d| crate::text::portal_epoch(&d, &self.cfg.portal_tz));
                    if self.cfg.set_file_time {
                        if let Some(ep) = ep {
                            super::download::set_file_times(&path, ep);
                            if !audio.is_empty() {
                                super::download::set_file_times(Path::new(&audio), ep);
                            }
                        }
                    }
                    let bytes = media::sha256_file(&path);
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    r.state = "ok".into();
                    r.path = path.to_string_lossy().to_string();
                    r.audio_path = audio;
                    r.size = size;
                    r.error.clear();
                    let doc_id = format!("Aud-{}", r.codigo);
                    let c = Card {
                        kind: r.kind.clone(),
                        number: r.case.clone(),
                        date: r.entry_date.clone(),
                        office: r.office.clone(),
                        class_text: r.entry_class.clone(),
                        ..Default::default()
                    };
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let pdt = r
                        .date()
                        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or_else(|| r.created.clone());
                    let row = row(&[
                        ("run_id", self.run_id.clone()),
                        ("run_start", crate::pyfmt::ts_human(&self.run_start)),
                        ("case_number", c.number.clone()),
                        ("entry_kind", c.kind.clone()),
                        ("entry_office", c.office.clone()),
                        ("entry_class", c.class_text.clone()),
                        ("entry_date", c.date.clone()),
                        ("carpeta", String::new()),
                        ("doc_id", doc_id.clone()),
                        ("doc_description", r.label()),
                        ("doc_description_full", r.title.clone()),
                        ("portal_date", pdt),
                        ("estado", r.tipo.clone()),
                        ("tipo_documento", media::type_label(r.av()).into()),
                        ("tipo_escrito", String::new()),
                        ("original_filename", r.file.clone()),
                        ("detected_type", if r.av() == "av" { "MP4 video".into() } else { "audio".into() }),
                        ("new_filename", name.clone()),
                        ("target_path", r.path.clone()),
                        ("download_time", crate::pyfmt::ts_human(&chrono::Local::now())),
                        ("size_bytes", size.to_string()),
                        ("sha256", bytes),
                        ("status", "OK".into()),
                        ("error", String::new()),
                    ]);
                    self.proto.append(row);
                    let _ = crate::read_state::mark(&self.cfg.read_state_file, "doc", std::slice::from_ref(&doc_id), false);
                    crate::read_state::add_present(&self.cfg.read_state_file, &doc_id);
                    crate::caseinfo::mark_media_done(&crate::caseinfo::info_dir(&self.cfg.case.dir, &r.case), &r.case, &r.codigo);
                    if !dup {
                        self.media_new += 1;
                        self.new_docs += 1;
                    }
                    self.downloads.push(format!(
                        "{}  {:<12} {}  {}",
                        crate::pyfmt::now_human(),
                        crate::i18n::tr("OK"),
                        self.id_case(&r.case),
                        name
                    ));
                    self.info(format!("      OK: {name}"));
                    self.live_tick();
                }
                Err(e) if e.is_fatal() => {
                    if matches!(e, RunErr::DiskFull(_)) {
                        self.warn(format!("      recording not downloaded - not enough free space: {e}"));
                    }
                    self.media.recs.insert(r.key(), r);
                    self.media.save();
                    return Err(e);
                }
                Err(e) => {
                    r.error = e.short();
                    r.state = if r.error.contains("No se encontraron archivos") {
                        "missing".into()
                    } else {
                        "failed".into()
                    };
                    let lim = self.cfg.skip_after_failed_runs;
                    if !media::refused(&r.error) {
                        self.warn(format!("      recording not downloaded: {} - no answer, does not count as a failed run; the next update tries it again", r.error));
                    } else if lim <= 0 {
                        r.count_try();
                        self.warn(format!(
                            "      recording not downloaded: {} - it will be tried again in the next update",
                            r.error
                        ));
                    } else {
                        r.count_try();
                        if r.paused(lim, self.cfg.retry_skipped_after_days) {
                            let days = if self.cfg.retry_skipped_after_days <= 0 {
                                90
                            } else {
                                self.cfg.retry_skipped_after_days
                            };
                            self.warn(format!("      recording not downloaded: {} - the court's server refuses it (failed run {} of {lim}); not tried again for {days} days (right-click on „Not downloadable“ → „Mark as never tried“ tries it again)", r.error, r.tries));
                        } else {
                            self.warn(format!("      recording not downloaded: {} - the court's server refuses it (failed run {} of {lim}); next try at the earliest tomorrow", r.error, r.tries));
                        }
                    }
                }
            }
            self.media.recs.insert(r.key(), r);
            self.media.save();
        }
        self.live_phase.clear();
        self.live_progress = (String::new(), 0, 0);
        Ok(())
    }
}

fn move_file(from: &Path, to: &Path) -> RResult<PathBuf> {
    if to.exists() {
        let (b, e) = crate::text::splitext(&to.to_string_lossy());
        let alt = PathBuf::from(format!("{b}_redownload-{}{e}", crate::pyfmt::ts_file(&chrono::Local::now())));
        if !alt.exists() {
            return move_file(from, &alt);
        }
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(to.to_path_buf());
    }
    if let Err(e) = std::fs::copy(from, to) {
        let _ = std::fs::remove_file(to);
        return Err(if crate::disk::is_full(&e) {
            RunErr::DiskFull(format!("{e}"))
        } else {
            RunErr::Other(format!("cannot write {}: {e}", to.display()))
        });
    }
    let _ = std::fs::remove_file(from);
    Ok(to.to_path_buf())
}

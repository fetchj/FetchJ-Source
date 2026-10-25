use crate::aipdf::{self, DocOut, PageOut, Tx};
use crate::anon::{Anon, Kind};
use crate::config::Conf;
use crate::overview::CaseDoc;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub static AIK: Lazy<HashMap<String, Value>> = Lazy::new(|| {
    let mut m = HashMap::new();
    for (l, src) in [
        ("de", include_str!("../../ui/i18n/app.de.json")),
        ("es", include_str!("../../ui/i18n/app.es.json")),
        ("en", include_str!("../../ui/i18n/app.en.json")),
        ("pt", include_str!("../../ui/i18n/app.pt.json")),
    ] {
        let a: Value = serde_json::from_str(src).expect("app texts");
        m.insert(l.to_string(), a["aik"].clone());
    }
    m
});

pub fn texts(lang: &str) -> &'static Value {
    AIK.get(lang).or_else(|| AIK.get("es")).unwrap()
}

const IMG: [&str; 8] = [".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".tif", ".tiff"];

fn safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect()
}

fn fname(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' }).collect();
    let mut o = String::new();
    for p in t.split('-').filter(|x| !x.is_empty()) {
        if !o.is_empty() {
            o.push('-');
        }
        o.push_str(p);
    }
    o.chars().take(50).collect()
}

pub fn root(conf: &Conf, case: &str) -> PathBuf {
    crate::research::case_dir(conf, case).join("_ai")
}

fn blur_path(conf: &Conf, case: &str, doc: &str, size: u64, page: usize) -> PathBuf {
    root(conf, case).join("_blur").join(format!("{}_{}_{}.jpg", safe(doc), size, page))
}

#[derive(Clone, Debug)]
pub struct Opts {
    pub case_numbers: bool,
    pub ocr_text: bool,
    pub ocr_min: i64,
    pub media_anon: bool,
    pub always: Vec<String>,
    pub never: Vec<String>,
}

impl Opts {
    pub fn from_store(st: &crate::config::Store) -> Opts {
        let list = |k: &str| {
            st.get("ai", k)
                .split([',', ';', '\n'])
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        };
        Opts {
            case_numbers: st.get("ai", "case_numbers") == "1",
            ocr_text: st.get("ai", "ocr_text") != "0",
            ocr_min: st.get("ai", "ocr_min").parse().unwrap_or(70),
            media_anon: st.get("ai", "media_anon") == "1",
            always: list("always"),
            never: list("never"),
        }
    }
}

fn local(d: &CaseDoc) -> bool {
    d.missing.is_empty() && !d.path.is_empty() && d.rec.is_empty()
}
fn is_img(d: &CaseDoc) -> bool {
    IMG.contains(&d.ext.to_lowercase().as_str())
}

fn pages_of(text: &str) -> Vec<String> {
    text.split('\u{c}').map(|p| p.trim_matches('\n').to_string()).collect()
}

fn scanned(c: &crate::research::Cached, img: bool) -> bool {
    img || c.ocr || c.scan
}

pub fn scan(conf: &Conf, case: &str, version: &str, anon: bool) -> Value {
    let docs: Vec<CaseDoc> = crate::overview::case_docs(conf, case, version).into_iter().filter(local).collect();
    let mut need = Vec::new();
    let mut blur = Vec::new();
    let mut n_blur = 0usize;
    for d in &docs {
        let c = crate::research::cached(conf, case, &d.doc_id).filter(|c| c.size == d.size || c.size == 0);
        let Some(c) = c else {
            need.push(d.doc_id.clone());
            continue;
        };
        if !anon || !scanned(&c, is_img(d)) {
            continue;
        }
        let n = if is_img(d) { 1 } else { pages_of(&c.text).len().max(1) };
        let todo: Vec<usize> = (1..=n)
            .filter(|p| !blur_path(conf, case, &d.doc_id, d.size, *p).is_file())
            .collect();
        n_blur += todo.len();
        if !todo.is_empty() {
            blur.push(json!({ "doc": d.doc_id, "path": d.path, "ext": d.ext, "size": d.size, "pages": todo }));
        }
    }
    json!({ "docs": docs.len(), "need_text": need.len(), "need": need, "blur": blur, "blur_n": n_blur })
}

pub fn put_blur(conf: &Conf, case: &str, doc: &str, size: u64, page: usize, jpeg: &[u8]) -> Result<(), String> {
    if case.is_empty() || doc.is_empty() || page == 0 || aipdf::jpeg_size(jpeg).is_none() {
        return Err("bad request".into());
    }
    let p = blur_path(conf, case, doc, size, page);
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(p, jpeg).map_err(|e| e.to_string())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Att {
    pub key: String,

    pub kind: String,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub pages: usize,
    pub checked: bool,
    pub sub: String,
}

#[derive(Clone)]
pub struct Pack {
    pub id: String,
    pub case: String,
    pub lang: String,
    pub anon: bool,
    pub dir: PathBuf,
    pub atts: Vec<Att>,
    pub info: crate::aiprompt::PackInfo,
    pub map: BTreeMap<String, BTreeSet<String>>,

    pub api_case: PathBuf,
    pub api_research: PathBuf,

    pub role_labels: HashMap<String, Vec<String>>,
}

static PACKS: Lazy<Mutex<HashMap<String, Pack>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn get(id: &str) -> Option<Pack> {
    PACKS.lock().unwrap().get(id).cloned()
}

fn label_words(t: &Value) -> HashMap<Kind, String> {
    let mut m = HashMap::new();
    for k in Kind::ALL {
        if let Some(w) = t["lbl"][k.key()].as_str() {
            m.insert(k, w.to_string());
        }
    }
    m
}

fn engine(conf: &Conf, case: &str, lang: &str, o: &Opts, all: &[&str]) -> (Anon, HashMap<String, Vec<String>>) {
    let t = texts(lang);
    let mut a = Anon::new(label_words(t), t["lbl_shared"].as_str().unwrap_or("Name"));
    a.case_numbers = o.case_numbers;
    for n in &o.never {
        a.never(n);
    }
    for n in &o.always {
        a.always(n);
    }

    let mut persons: Vec<(String, String)> = Vec::new();
    for e in crate::research::case_entries(conf, case) {
        for (r, n) in e.roles.iter().zip(e.names.iter()) {
            let f = crate::anon::fold(n.trim());
            if !n.trim().is_empty() && !persons.iter().any(|(_, x)| crate::anon::fold(x) == f) {
                persons.push((r.clone(), n.trim().to_string()));
            }
        }
    }
    let mut per_role: HashMap<String, usize> = HashMap::new();
    let mut role_labels: HashMap<String, Vec<String>> = HashMap::new();
    for (r, n) in &persons {
        let rl = crate::caseinfo::role_label(r, lang)
            .split('/')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        let k = per_role.entry(rl.clone()).or_insert(0);
        *k += 1;
        let label = format!("[{rl} {k}]");
        role_labels.entry(r.to_uppercase()).or_default().push(label.clone());
        a.person(n, &label, Kind::Party);
    }

    let ct = crate::contacts::load(conf);
    let mut nc = 0usize;
    for c in ct["contacts"].as_array().cloned().unwrap_or_default() {
        if c["kind"] == "group" {
            continue;
        }
        let name: Vec<String> = ["first", "middle", "last"]
            .iter()
            .filter_map(|k| c[*k].as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if name.len() < 2 {
            continue;
        }
        let mine = c["cases"]
            .as_array()
            .map(|l| l.iter().any(|x| x.as_str().map(|s| s.eq_ignore_ascii_case(case)).unwrap_or(false)))
            .unwrap_or(false);
        nc += 1;
        let label = format!("[{} {nc}]", t["lbl"]["contact"].as_str().unwrap_or("Contact"));
        if mine {
            a.person(&name.join(" "), &label, Kind::Contact);
        } else {
            a.person_full(&name.join(" "), &label, Kind::Contact);
        }
    }
    for x in all {
        a.discover(x);
    }
    (a, role_labels)
}

fn recordings(conf: &Conf, case: &str) -> Vec<crate::media::MediaRec> {
    let reg = crate::media::Register::load(&conf.protocol_dir);
    let mut seen = std::collections::HashSet::new();
    let mut v: Vec<crate::media::MediaRec> = reg
        .recs
        .values()
        .filter(|r| r.case.eq_ignore_ascii_case(case) && !r.is_court() && r.present())
        .filter(|r| seen.insert((r.label().to_lowercase(), r.created.clone())))
        .cloned()
        .collect();
    v.sort_by(|a, b| a.created.cmp(&b.created));
    v
}

fn fsize(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn pdf_pages(p: &Path) -> usize {
    lopdf::Document::load(p).map(|d| d.get_pages().len()).unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
pub fn build(
    conf: &Conf,
    app_data: &Path,
    case: &str,
    lang: &str,
    party: &str,
    anon_on: bool,
    o: &Opts,
    version: &str,
) -> Result<Value, String> {
    let l = if AIK.contains_key(lang) { lang } else { "es" };
    let tv = texts(l);
    let t = Tx(tv);
    let mut docs = crate::overview::case_docs(conf, case, version);
    docs.sort_by(|a, b| a.date.cmp(&b.date).then(a.doc_id.cmp(&b.doc_id)));
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let dir = root(conf, case).join(&stamp);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let cs = fname(case);
    let mut info = crate::aiprompt::PackInfo {
        anon: anon_on,
        ..Default::default()
    };
    let mut atts: Vec<Att> = Vec::new();
    let mut map = BTreeMap::new();
    let mut role_labels = HashMap::new();
    let mut verify: Vec<String> = Vec::new();
    let mut counts: BTreeMap<Kind, usize> = BTreeMap::new();
    let api_case = dir.join("_api_case.txt");
    let api_research = dir.join("_api_research.txt");

    if anon_on {
        struct Src {
            d: CaseDoc,
            c: Option<crate::research::Cached>,
        }
        let src: Vec<Src> = docs
            .iter()
            .filter(|d| d.rec.is_empty())
            .map(|d| Src {
                d: d.clone(),
                c: if local(d) {
                    crate::research::cached(conf, case, &d.doc_id)
                } else {
                    None
                },
            })
            .collect();
        let all: Vec<&str> = src.iter().filter_map(|s| s.c.as_ref().map(|c| c.text.as_str())).collect();
        let (mut a, rl) = engine(conf, case, l, o, &all);
        role_labels = rl;
        let mut outs: Vec<DocOut> = Vec::new();
        let mut api = String::new();
        let mut verify_text = String::new();
        let n_docs = src.len();
        for (i, s) in src.iter().enumerate() {
            let d = &s.d;
            let (title, title_spans) = a.apply(&d.description);
            let date = d.date.map(|x| x.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
            let mut out = DocOut {
                n: i + 1,
                date: date.clone(),
                tipo: d.tipo.clone(),
                estado: d.estado.clone(),
                title: title.clone(),
                title_spans,
                note: String::new(),
                pages: Vec::new(),
            };
            verify_text.push_str(&title);
            verify_text.push('\n');
            api.push_str(&format!(
                "\n\n===== {} · {} · {} · {} =====\n",
                t.f("pdf_doc", &[("i", (i + 1).to_string()), ("n", n_docs.to_string())]),
                date,
                d.tipo,
                title
            ));
            if !d.missing.is_empty() {
                out.note = t.s("pdf_missing");
            } else if let Some(c) = &s.c {
                let img = is_img(d);
                let pages = if img { vec![c.text.clone()] } else { pages_of(&c.text) };
                let sc = scanned(c, img);
                for (k, p) in pages.iter().enumerate() {
                    let no = k + 1;
                    if !sc {
                        let (tx, sp) = a.apply(p);
                        api.push_str(&format!("\n--- {} ---\n{}\n", t.f("pdf_page", &[("p", no.to_string())]), tx));
                        verify_text.push_str(&tx);
                        verify_text.push('\n');
                        out.pages.push(PageOut::Text { no, text: tx, spans: sp });
                        continue;
                    }
                    let has = p.chars().filter(|c| c.is_alphanumeric()).count() >= 20;
                    let conf_p = c
                        .conf
                        .get(k)
                        .copied()
                        .filter(|x| *x >= 0)
                        .unwrap_or_else(|| crate::anon::text_quality(p));
                    let sure = has && c.ocr && o.ocr_text && conf_p >= o.ocr_min;
                    let ocr = if sure {
                        let (tx, sp) = a.apply(p);
                        api.push_str(&format!("\n--- {} ---\n{}\n", t.f("pdf_ocr", &[("p", no.to_string())]), tx));
                        verify_text.push_str(&tx);
                        verify_text.push('\n');
                        Some((tx, sp))
                    } else {
                        api.push_str(&format!(
                            "\n--- {} --- [{}]\n",
                            t.f("pdf_page", &[("p", no.to_string())]),
                            t.s(if img { "pdf_stamp_img" } else { "pdf_stamp" }).replace('\n', " ")
                        ));
                        None
                    };
                    let bp = blur_path(conf, case, &d.doc_id, d.size, no);
                    out.pages.push(PageOut::Blur {
                        no,
                        img: Some(bp).filter(|p| p.is_file()),
                        photo: img,
                        ocr,
                        unsure: has && !sure,
                    });
                }
            } else {
                out.note = t.s("pdf_notext");
                api.push_str(&format!("[{}]\n", t.s("pdf_notext")));
            }
            info.title.insert(d.doc_id.clone(), title);
            outs.push(out);
        }
        verify = a.verify(&verify_text);
        counts = a.count.clone();
        let rep: Vec<String> = counts
            .iter()
            .map(|(k, n)| format!("{n} {}", t.s(&format!("k_{}", k.key()))))
            .collect();
        let pdf = dir.join(t.f("f_case", &[("c", cs.clone()), ("a", t.s("f_case_anon"))]));
        let (pages, start, blurred) = aipdf::write_case(
            &pdf,
            &t,
            &a.apply(case).0,
            &if rep.is_empty() {
                String::new()
            } else {
                t.f("pdf_counts", &[("l", rep.join(" · "))])
            },
            &outs,
            version,
        )?;
        for (j, s) in src.iter().enumerate() {
            info.page_of.insert(s.d.doc_id.clone(), start[j]);
        }
        info.case_label = if o.case_numbers { a.apply(case).0 } else { case.to_string() };
        info.case_pdf = pdf.file_name().unwrap().to_string_lossy().to_string();
        info.case_pages = pages;
        std::fs::write(&api_case, api.trim_start()).map_err(|e| e.to_string())?;
        let sub = t.f("case_sub", &[("p", pages.to_string()), ("d", n_docs.to_string())])
            + &if blurred > 0 {
                t.f("case_sub_blur", &[("b", blurred.to_string())])
            } else {
                String::new()
            };
        atts.push(Att {
            key: "case".into(),
            kind: "case".into(),
            name: info.case_pdf.clone(),
            path: pdf.to_string_lossy().into(),
            size: fsize(&pdf),
            pages,
            checked: true,
            sub,
        });

        info.party_labels = role_labels.get(&party.to_uppercase()).cloned().unwrap_or_default();
        map = a.map.clone();
    } else {
        let cp = crate::casepdf::out_path(conf, case);
        let name = t.f("f_case", &[("c", cs.clone()), ("a", String::new())]);
        let pdf = dir.join(&name);
        let have = cp.is_file() && (std::fs::hard_link(&cp, &pdf).is_ok() || std::fs::copy(&cp, &pdf).is_ok());
        let pages = if have { pdf_pages(&pdf) } else { 0 };
        info.case_label = case.to_string();
        info.case_pdf = name.clone();
        info.case_pages = pages;

        let mut api = String::new();
        let n_docs = docs.iter().filter(|d| d.rec.is_empty()).count();
        for (i, d) in docs.iter().filter(|d| d.rec.is_empty()).enumerate() {
            let date = d.date.map(|x| x.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
            api.push_str(&format!(
                "\n\n===== {} · {} · {} · {} =====\n",
                t.f("pdf_doc", &[("i", (i + 1).to_string()), ("n", n_docs.to_string())]),
                date,
                d.tipo,
                d.description
            ));
            match crate::research::cached(conf, case, &d.doc_id).filter(|_| local(d)) {
                Some(c) => api.push_str(&c.text.replace('\u{c}', "\n")),
                None => api.push_str(&format!(
                    "[{}]\n",
                    t.s(if d.missing.is_empty() { "pdf_notext" } else { "pdf_missing" })
                )),
            }
        }
        std::fs::write(&api_case, api.trim_start()).map_err(|e| e.to_string())?;
        if have {
            atts.push(Att {
                key: "case".into(),
                kind: "case".into(),
                name,
                path: pdf.to_string_lossy().into(),
                size: fsize(&pdf),
                pages,
                checked: true,
                sub: t.f("case_sub", &[("p", pages.to_string()), ("d", n_docs.to_string())]),
            });
        }
    }

    let analysis = crate::research::get(conf, case);
    let judgments: Vec<(String, PathBuf)> = crate::nexus::kept(conf, case)
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            Some((
                v["name"].as_str()?.trim_end_matches(".pdf").to_string(),
                PathBuf::from(v["path"].as_str()?),
            ))
        })
        .collect();
    let rpdf = dir.join(t.f("f_research", &[("c", cs.clone())]));
    let (rpages, nj) = aipdf::write_research(&rpdf, &t, &info.case_label, analysis.as_ref(), &judgments, version)?;
    info.research_pdf = rpdf.file_name().unwrap().to_string_lossy().to_string();
    info.research_n = nj;
    {
        let mut s = String::new();
        let ex = lopdf::Document::load(&rpdf).ok();
        if let Some(d) = &ex {
            s.push_str(&d.extract_text(&[1]).unwrap_or_default());
        }
        for (name, p) in &judgments {
            s.push_str(&format!("\n\n===== {name} =====\n{}", aipdf::pdf_text(p)));
        }
        std::fs::write(&api_research, s).map_err(|e| e.to_string())?;
    }
    let rsub = if analysis.is_some() {
        t.f("rs_sub", &[("n", nj.to_string())])
    } else {
        t.f("rs_sub_noan", &[("n", nj.to_string())])
    };
    atts.push(Att {
        key: "research".into(),
        kind: "research".into(),
        name: info.research_pdf.clone(),
        path: rpdf.to_string_lossy().into(),
        size: fsize(&rpdf),
        pages: rpages,
        checked: true,
        sub: rsub,
    });

    let ff = crate::media::ffmpeg(app_data);
    for r in recordings(conf, case) {
        let when = r.date().map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
        let base = t.f(
            "f_audio",
            &[("d", when), ("t", fname(&if anon_on { r.tipo.clone() } else { r.label() }))],
        );
        let (kind, path) = if r.av() != "av" {
            ("audio", PathBuf::from(&r.path))
        } else if !r.audio_path.is_empty() && Path::new(&r.audio_path).is_file() {
            ("avaudio", PathBuf::from(&r.audio_path))
        } else if let Some(ff) = &ff {
            let out = dir.join("_media").join(format!("{base}.m4a"));
            let _ = std::fs::create_dir_all(out.parent().unwrap());
            let mut c = std::process::Command::new(ff);
            c.args(["-y", "-loglevel", "error", "-i"])
                .arg(&r.path)
                .args(["-vn", "-c:a", "copy"])
                .arg(&out);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                c.creation_flags(0x0800_0000);
            }
            match c.status() {
                Ok(s) if s.success() && out.is_file() => ("avaudio", out),
                _ => ("video", PathBuf::from(&r.path)),
            }
        } else {
            ("video", PathBuf::from(&r.path))
        };
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_else(|| "mp3".into());
        let name = format!(
            "{base}.{}",
            if kind == "avaudio" && ext == "mp4" {
                "m4a".to_string()
            } else {
                ext
            }
        );
        let checked = kind != "video" && (!anon_on || o.media_anon);
        let len = r.minutes().map(|m| format!("{m} min")).unwrap_or_default();
        atts.push(Att {
            key: r.key(),
            kind: kind.into(),
            name,
            path: path.to_string_lossy().into(),
            size: fsize(&path),
            pages: 0,
            checked,
            sub: [if anon_on { r.tipo.clone() } else { r.label() }, len]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · "),
        });
    }

    if anon_on {
        let m: serde_json::Map<String, Value> = map.iter().map(|(k, v)| (k.clone(), json!(v.iter().collect::<Vec<_>>()))).collect();
        let _ = std::fs::write(
            root(conf, case).join(format!("{stamp}_map.json")),
            serde_json::to_string_pretty(&json!({ "case": case, "made": stamp, "map": m })).unwrap_or_default(),
        );
    }

    let _ = std::fs::write(
        dir.join("_pack.json"),
        json!({ "case": case, "made": stamp, "anon": anon_on, "atts": atts.iter().map(|a| a.name.clone()).collect::<Vec<_>>() })
            .to_string(),
    );
    let pack = Pack {
        id: stamp.clone(),
        case: case.to_string(),
        lang: l.to_string(),
        anon: anon_on,
        dir: dir.clone(),
        atts: atts.clone(),
        info,
        map,
        api_case,
        api_research,
        role_labels,
    };
    let checked: Vec<String> = atts.iter().filter(|a| a.checked).map(|a| a.key.clone()).collect();
    let pr = prompt(conf, &pack, party, &checked, version);
    PACKS.lock().unwrap().insert(stamp.clone(), pack);
    crate::focus::app_event(&format!(
        "AI handover: package of {case} made ({}, {} attachments)",
        if anon_on { "anonymised" } else { "not anonymised" },
        atts.len()
    ));
    let reps: Vec<Value> = counts.iter().map(|(k, n)| json!({ "kind": k.key(), "n": n })).collect();
    Ok(json!({
        "id": stamp, "dir": crate::paths::plain_path(&dir), "anon": anon_on, "atts": atts, "subject": pr["subject"], "prompt": pr["body"],
        "replaced": reps, "verify": verify,
    }))
}

pub fn history(conf: &Conf, cases: &[String]) -> Value {
    let mut out = serde_json::Map::new();
    for case in cases {
        let r = root(conf, case);
        let mut packs: Vec<Value> = Vec::new();
        let mut answers: Vec<Value> = Vec::new();
        for e in std::fs::read_dir(&r).into_iter().flatten().flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if !p.is_dir() || name.starts_with('_') {
                continue;
            }
            let meta: Value = std::fs::read_to_string(p.join("_pack.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null);
            let anon = meta["anon"]
                .as_bool()
                .unwrap_or_else(|| r.join(format!("{name}_map.json")).is_file());
            packs.push(json!({ "id": name, "anon": anon, "dir": crate::paths::plain_path(&p) }));
            for a in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                let n = a.file_name().to_string_lossy().to_string();

                if n.starts_with("Claude_")
                    && n.ends_with(".md")
                    && !n.ends_with("_names.md")
                    && a.metadata().map(|x| x.len() > 0).unwrap_or(false)
                {
                    let names = p.join(n.replace(".md", "_names.md"));
                    answers.push(json!({ "at": n.trim_start_matches("Claude_").trim_end_matches(".md"), "path": crate::paths::plain_path(&a.path()), "names": if names.is_file() { json!(crate::paths::plain_path(&names)) } else { Value::Null }, "pack": name }));
                }
            }
        }
        packs.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
        answers.sort_by(|a, b| b["at"].as_str().cmp(&a["at"].as_str()));
        let judg = crate::nexus::kept(conf, case).as_array().map(|a| a.len()).unwrap_or(0);
        out.insert(
            case.clone(),
            json!({ "judgments": judg, "recordings": recordings(conf, case).len(), "packs": packs, "answers": answers }),
        );
    }
    Value::Object(out)
}

pub fn prompt(conf: &Conf, p: &Pack, party: &str, checked: &[String], version: &str) -> Value {
    let mut info = p.info.clone();
    info.media = p
        .atts
        .iter()
        .filter(|a| !matches!(a.kind.as_str(), "case" | "research") && checked.contains(&a.key))
        .map(|a| (a.key.clone(), a.name.clone()))
        .collect();
    info.party_labels = p.role_labels.get(&party.to_uppercase()).cloned().unwrap_or_default();
    crate::aiprompt::set_party(conf, &p.case, party);
    crate::aiprompt::build_pack(conf, &p.case, &p.lang, party, version, &info)
}

pub fn send_folder(p: &Pack, keys: &[String], prompt: &str) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let out = p.dir.join(format!("{}_KI", fname(&p.case)));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let t = texts(&p.lang);
    let pf = out.join(t["f_prompt"].as_str().unwrap_or("00_Prompt.txt"));
    std::fs::write(&pf, format!("\u{feff}{}", prompt.replace("\r\n", "\n").replace('\n', "\r\n"))).map_err(|e| e.to_string())?;
    let mut files = vec![pf];
    for a in p.atts.iter().filter(|a| keys.contains(&a.key)) {
        let to = out.join(&a.name);
        if std::fs::hard_link(&a.path, &to).is_err() {
            std::fs::copy(&a.path, &to).map_err(|e| e.to_string())?;
        }
        files.push(to);
    }
    Ok((out, files))
}

pub fn zip_of(p: &Pack, files: &[PathBuf]) -> Result<PathBuf, String> {
    let z = p.dir.join(format!("{}_KI_{}.zip", fname(&p.case), p.id));
    let f = std::fs::File::create(&z).map_err(|e| e.to_string())?;
    let mut w = zip::ZipWriter::new(f);
    for x in files {
        let name = x.file_name().unwrap().to_string_lossy().to_string();

        let m = if name.ends_with(".txt") {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        };
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(m)
            .large_file(fsize(x) > 0xF000_0000);
        w.start_file(name, o).map_err(|e| e.to_string())?;
        let mut r = std::fs::File::open(x).map_err(|e| e.to_string())?;
        std::io::copy(&mut r, &mut w).map_err(|e| e.to_string())?;
    }
    w.finish().map_err(|e| e.to_string())?;
    Ok(z)
}

pub fn back(p: &Pack, text: &str) -> String {
    if p.anon {
        Anon::back(&p.map, text)
    } else {
        text.to_string()
    }
}

pub fn desktop_link(q: &str, folder: &Path) -> String {
    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    }
    format!("claude://cowork/new?q={}&folder={}", enc(q), enc(&crate::paths::plain_path(folder)))
}

pub fn desktop_installed() -> bool {
    #[cfg(windows)]
    {
        for root in ["HKCU\\Software\\Classes\\claude", "HKCR\\claude"] {
            let mut c = std::process::Command::new("reg");
            c.args(["query", root]);
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
            if c.output().map(|o| o.status.success()).unwrap_or(false) {
                return true;
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        false
    }
}

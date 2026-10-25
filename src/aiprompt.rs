use crate::config::Conf;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

static AIP: Lazy<HashMap<String, Value>> = Lazy::new(|| {
    let mut m = HashMap::new();
    for (l, src) in [
        ("de", include_str!("../../ui/i18n/app.de.json")),
        ("es", include_str!("../../ui/i18n/app.es.json")),
        ("en", include_str!("../../ui/i18n/app.en.json")),
        ("pt", include_str!("../../ui/i18n/app.pt.json")),
    ] {
        let a: Value = serde_json::from_str(src).expect("app texts");
        m.insert(l.to_string(), a["aip"].clone());
    }
    m
});

struct T<'a>(&'a Value);
impl T<'_> {
    fn s(&self, k: &str) -> String {
        self.0[k].as_str().unwrap_or("").to_string()
    }
    fn lines(&self, k: &str) -> Vec<String> {
        self.0[k]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }
}

fn safe(case: &str) -> String {
    case.replace(['/', '\\', ':'], "_")
}

fn file_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string())
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").replace('|', "/")
}

fn filed_key(f: &str) -> String {
    let p: Vec<&str> = f.split('/').collect();
    if p.len() == 3 {
        format!("{}{}{}", p[2], p[1], p[0])
    } else {
        f.to_string()
    }
}

fn party_file(conf: &Conf) -> PathBuf {
    conf.data_dir.join("FetchJ_ai_party.json")
}

fn parties(conf: &Conf) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(party_file(conf))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub fn set_party(conf: &Conf, case: &str, role: &str) {
    let mut m = parties(conf);
    m.insert(case.to_string(), Value::String(role.to_string()));
    let _ = std::fs::create_dir_all(&conf.data_dir);
    let _ = std::fs::write(
        party_file(conf),
        serde_json::to_string_pretty(&Value::Object(m)).unwrap_or_default(),
    );
}

pub fn roles(conf: &Conf, case: &str, lang: &str) -> Value {
    let mut raw: Vec<String> = Vec::new();
    for e in crate::research::case_entries(conf, case) {
        for r in e.roles {
            let r = r.trim().to_string();
            if !r.is_empty() && !raw.iter().any(|x| x.eq_ignore_ascii_case(&r)) {
                raw.push(r);
            }
        }
    }
    if raw.is_empty() {
        raw = ["IMPUTADO", "OFENDIDO", "ACTOR", "DEMANDADO"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    }
    let chosen = parties(conf).get(case).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let list: Vec<Value> = raw
        .iter()
        .map(|r| json!({ "raw": r, "label": crate::caseinfo::role_label(r, lang) }))
        .collect();
    json!({ "roles": list, "chosen": chosen })
}

pub fn build(conf: &Conf, case: &str, lang: &str, party: &str, version: &str) -> Value {
    build_with(conf, case, lang, party, version, None)
}

#[derive(Default, Clone)]
pub struct PackInfo {
    pub anon: bool,

    pub case_label: String,

    pub title: std::collections::HashMap<String, String>,
    pub page_of: std::collections::HashMap<String, usize>,
    pub case_pdf: String,
    pub case_pages: usize,
    pub research_pdf: String,
    pub research_n: usize,

    pub media: std::collections::HashMap<String, String>,

    pub party_labels: Vec<String>,
}

pub fn build_pack(conf: &Conf, case: &str, lang: &str, party: &str, version: &str, info: &PackInfo) -> Value {
    build_with(conf, case, lang, party, version, Some(info))
}

fn build_with(conf: &Conf, case: &str, lang: &str, party: &str, version: &str, pack: Option<&PackInfo>) -> Value {
    let l = if AIP.contains_key(lang) { lang } else { "es" };
    let t = T(&AIP[l]);
    let empty = t.s("empty");
    let or = |s: &str| if s.trim().is_empty() { empty.clone() } else { s.trim().to_string() };
    let mut docs = crate::overview::case_docs(conf, case, version);
    docs.sort_by(|a, b| a.date.cmp(&b.date).then(a.doc_id.cmp(&b.doc_id)));
    let mut entries = crate::research::case_entries(conf, case);
    entries.sort_by_key(|e| filed_key(&e.filed));
    let nexus: Vec<String> = crate::nexus::kept(conf, case)
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["name"].as_str().map(String::from))
        .collect();

    let mut recs: Vec<crate::media::MediaRec> = {
        let reg = crate::media::Register::load(&conf.protocol_dir);
        let mut seen = std::collections::HashSet::new();
        reg.recs
            .values()
            .filter(|r| r.case.eq_ignore_ascii_case(case) && !r.is_court())
            .filter(|r| seen.insert((r.label().to_lowercase(), r.created.clone())))
            .cloned()
            .collect()
    };
    recs.sort_by(|a, b| a.created.cmp(&b.created));
    let combined = crate::casepdf::out_path(conf, case);
    let has_combined = combined.is_file();
    let pages = crate::state::Pages::load(&conf.protocol_dir, &conf.case.dir);
    let plain = |p: &Path| crate::paths::plain_path(p);

    let code = case.rsplit('-').next().unwrap_or("").to_uppercase();
    let matter = t.0["matters"][&code]
        .as_str()
        .map(String::from)
        .or_else(|| {
            crate::research::lists(conf).nexus["materia"][&code]["name"]
                .as_str()
                .map(String::from)
        })
        .unwrap_or_default();
    let first = entries.first();
    let last = entries.last();

    let file_type = if entries.is_empty() {
        String::new()
    } else if entries.iter().all(|e| e.kind.to_lowercase().contains("legajo")) {
        t.s("file_legajo")
    } else {
        t.s("file_main")
    };
    let mut party_txt = if party.trim().is_empty() {
        t.s("party_none")
    } else {
        crate::caseinfo::role_label(party, l)
    };
    if let Some(pk) = pack.filter(|p| p.anon && !p.party_labels.is_empty() && !party.trim().is_empty()) {
        party_txt = t
            .s("pk_party_lbl")
            .replace("{{ROLE}}", &party_txt)
            .replace("{{LABELS}}", &pk.party_labels.join(", "));
    }
    let cutoff = crate::overview::load_run_info(conf)
        .filter(|r| r.end_ms > 0)
        .and_then(|r| chrono::DateTime::from_timestamp_millis(r.end_ms))
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    let mode = conf.case_modes.get(case).cloned().unwrap_or_else(|| conf.new_cases.clone());
    let completeness = match conf.docs_from_date {
        Some(d) if mode != "full" => t.s("comp_b").replace("{{FILTER_FROM_DATE}}", &d.format("%Y-%m-%d").to_string()),
        _ => t.s("comp_a"),
    };
    let combined_txt = if has_combined {
        file_name(&combined.to_string_lossy())
    } else {
        t.s("combined_none")
    };
    let vals: Vec<(&str, String)> = vec![
        (
            "CASE_NUMBER",
            pack.filter(|p| !p.case_label.is_empty())
                .map(|p| p.case_label.clone())
                .unwrap_or_else(|| case.to_string()),
        ),
        ("MATTER_CODE", or(&code)),
        ("MATTER_NAME", or(&matter)),
        ("COURT", or(&last.map(|e| e.office.clone()).unwrap_or_default())),
        ("FILING_DATE", or(&first.map(|e| e.filed.clone()).unwrap_or_default())),
        ("FILE_TYPE", or(&file_type)),
        ("REPRESENTED_PARTY", party_txt),
        ("CUTOFF_DATE", or(&cutoff)),
        ("COMPLETENESS_NOTE", completeness),
        ("DOC_COUNT", docs.len().to_string()),
        ("CASE_FOLDER", plain(&conf.case.dir.join(case))),
        ("ALL_CASES_FOLDER", plain(&conf.case.dir)),
        ("COMBINED_DOC", combined_txt),
        ("RESEARCH_COUNT", pack.map(|p| p.research_n).unwrap_or(nexus.len()).to_string()),
        ("ANALYZE_FOLDER", plain(&crate::research::case_dir(conf, case))),
        ("RESEARCH_FOLDER", plain(&crate::nexus::root(conf).join(case))),
        ("RECORDING_COUNT", recs.len().to_string()),
        ("CASE_PDF", pack.map(|p| p.case_pdf.clone()).unwrap_or_default()),
        ("CASE_PAGES", pack.map(|p| p.case_pages.to_string()).unwrap_or_default()),
        ("RESEARCH_PDF", pack.map(|p| p.research_pdf.clone()).unwrap_or_default()),
    ];
    let fill = |x: &str| {
        let mut s = x.to_string();
        for (k, v) in &vals {
            s = s.replace(&format!("{{{{{k}}}}}"), v);
        }
        s
    };

    let entries_lines: Vec<String> = if entries.is_empty() {
        vec![format!("  - {empty}")]
    } else {
        entries
            .iter()
            .map(|e| format!("  - {} · {} · {} · {}", or(&e.filed), or(&e.kind), or(&e.office), or(&e.clase)))
            .collect()
    };
    let block_key = if ["PE", "AG", "CI"].contains(&code.as_str()) {
        code.clone()
    } else {
        "OTHER".into()
    };
    let block: Vec<String> = t.0["blocks"][&block_key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    let index: Vec<String> = docs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let date = d
                .date
                .map(|x| x.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| empty.clone());
            let (pg, mut file) = if d.missing.is_empty() {
                (
                    pages
                        .get(Path::new(&d.path), Some(d.size))
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".into()),
                    file_name(&d.path),
                )
            } else {
                ("?".to_string(), format!("({})", one_line(&d.missing)))
            };
            let mut title = d.description.clone();
            if let Some(pk) = pack.filter(|p| p.anon) {
                title = pk.title.get(&d.doc_id).cloned().unwrap_or_default();
                if d.missing.is_empty() {
                    file = pk
                        .page_of
                        .get(&d.doc_id)
                        .map(|p| t.s("pk_file").replace("{{P}}", &p.to_string()))
                        .unwrap_or_default();
                }
            }
            format!(
                "{} | {} | {} | {} | {} | {} | {}",
                i + 1,
                date,
                or(&one_line(&d.tipo)),
                or(&one_line(&d.estado)),
                or(&one_line(&title)),
                pg,
                or(&file)
            )
        })
        .collect();
    let rec_lines: Vec<String> = if recs.is_empty() {
        vec![format!("- {}", t.s("rec_none"))]
    } else {
        recs.iter()
            .map(|r| {
                let when = r
                    .date()
                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| empty.clone());
                let len = r.minutes().map(|m| format!("{m} min")).unwrap_or_else(|| "?".into());
                let mut file = if r.present() {
                    file_name(&r.path)
                } else {
                    format!("({})", t.s("rec_not"))
                };
                if let Some(pk) = pack {
                    file = pk
                        .media
                        .get(&r.key())
                        .cloned()
                        .unwrap_or_else(|| format!("({})", t.s("pk_not_att")));
                }
                let label = if pack.map(|p| p.anon).unwrap_or(false) {
                    r.tipo.clone()
                } else {
                    r.label()
                };
                format!("- {when} · {} · {len} · {file}", or(&one_line(&label)))
            })
            .collect()
    };
    let research: Vec<String> = if nexus.is_empty() {
        vec![t.s("research_none")]
    } else {
        nexus.iter().map(|n| format!("- {n}")).collect()
    };

    let mut o: Vec<String> = Vec::new();
    let mut lines = t.lines("prompt");
    if let Some(pk) = pack {
        if let Some(ws) = lines.iter().position(|x| x.contains("{{CASE_FOLDER}}")).map(|i| i - 1) {
            let h1 = (ws + 1..lines.len()).find(|&i| lines[i].starts_with("# "));
            let h2 = h1.and_then(|h| (h + 1..lines.len()).find(|&i| lines[i].starts_with("# ")));
            if let Some(h2) = h2 {
                let new = t.lines(if pk.anon { "pk_where_anon" } else { "pk_where_full" });
                lines.splice(ws..h2 - 1, new);
            }
        }

        if let Some(r) = lines.iter().position(|x| x.trim() == "{{RECORDING_LIST}}") {
            if r + 1 < lines.len() && !lines[r + 1].trim().is_empty() {
                let k = if pk.anon && pk.media.is_empty() {
                    "pk_rec_anon"
                } else if pk.media.is_empty() {
                    "pk_rec_off"
                } else {
                    "pk_rec_full"
                };
                lines.splice(r + 1..r + 2, t.lines(k));
            }
        }
    } else {
        for x in t.lines(if has_combined { "intro" } else { "intro_nc" }) {
            o.push(fill(&x));
        }
        o.push(String::new());
    }
    for x in lines {
        match x.trim() {
            "{{ENTRIES}}" => o.extend(entries_lines.iter().cloned()),
            "{{MATTER_BLOCK}}" => o.extend(block.iter().cloned()),
            "{{DOCUMENT_INDEX}}" => o.extend(index.iter().cloned()),
            "{{RESEARCH_LIST}}" => o.extend(research.iter().cloned()),
            "{{RECORDING_LIST}}" => o.extend(rec_lines.iter().cloned()),
            _ => o.push(fill(&x)),
        }
    }

    if pack.is_some() {
        o.retain(|x| !x.starts_with("====="));
        while o.first().map(|x| x.trim().is_empty()).unwrap_or(false) {
            o.remove(0);
        }
        while o.last().map(|x| x.trim().is_empty()).unwrap_or(false) {
            o.pop();
        }
    }
    let body = crate::country::subst(&o.join("\n"), l);

    crate::casestate::remember(conf, case, "prompt", version);
    let subject = fill(&t.s(if pack.is_some() { "pk_subject" } else { "subject" }));
    json!({ "subject": subject, "chars": body.chars().count(), "body": body })
}

pub fn write(conf: &Conf, case: &str, body: &str) -> Result<PathBuf, String> {
    let dir = crate::research::case_dir(conf, case);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join(format!(
        "{}_ai-prompt_{}.txt",
        safe(case),
        chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
    ));

    let text = format!("\u{feff}{}", body.replace("\r\n", "\n").replace('\n', "\r\n"));
    std::fs::write(&p, text).map_err(|e| e.to_string())?;
    Ok(p)
}

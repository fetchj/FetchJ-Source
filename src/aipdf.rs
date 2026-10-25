use crate::anon::Span;
use crate::casepdf::{cp1252, fit, lit, utf16};
use crate::report::widths::{HELV, HELV_B};
use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use serde_json::Value;
use std::path::{Path, PathBuf};

const W: f32 = 595.28;
const H: f32 = 841.89;
const ML: f32 = 56.0;
const MR: f32 = 50.0;
const TOP: f32 = 790.0;
const BOTTOM: f32 = 52.0;

pub enum PageOut {
    Text {
        no: usize,
        text: String,
        spans: Vec<Span>,
    },

    Blur {
        no: usize,
        img: Option<PathBuf>,
        photo: bool,
        ocr: Option<(String, Vec<Span>)>,
        unsure: bool,
    },
}

pub struct DocOut {
    pub n: usize,
    pub date: String,
    pub tipo: String,
    pub estado: String,
    pub title: String,
    pub title_spans: Vec<Span>,

    pub note: String,
    pub pages: Vec<PageOut>,
}

pub struct Tx<'a>(pub &'a Value);
impl Tx<'_> {
    pub fn s(&self, k: &str) -> String {
        self.0[k].as_str().unwrap_or(k).to_string()
    }
    pub fn f(&self, k: &str, a: &[(&str, String)]) -> String {
        let mut s = self.s(k);
        for (n, v) in a {
            s = s.replace(&format!("{{{n}}}"), v);
        }
        s
    }
    pub fn lines(&self, k: &str) -> Vec<String> {
        self.0[k]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }
}

fn wid(b: &[u8], size: f32, bold: bool) -> f32 {
    let t = if bold { &HELV_B } else { &HELV };
    b.iter().map(|c| t[*c as usize] as f32).sum::<f32>() * size / 1000.0
}

struct Unit {
    b: Vec<u8>,
    label: bool,
    space: bool,
}

fn units(text: &str, a: usize, b: usize, spans: &[Span]) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut at = a;
    let mut space = false;
    let push_plain = |s: &str, out: &mut Vec<Unit>, space: &mut bool| {
        let mut first = true;
        let lead = s.starts_with(char::is_whitespace);
        for w in s.split_whitespace() {
            out.push(Unit {
                b: cp1252(w),
                label: false,
                space: if first { *space || lead } else { true },
            });
            first = false;
        }
        *space = s.ends_with(char::is_whitespace);
    };
    for sp in spans.iter().filter(|s| s.start >= a && s.end <= b) {
        if sp.start > at {
            push_plain(&text[at..sp.start], &mut out, &mut space);
        }
        out.push(Unit {
            b: cp1252(&text[sp.start..sp.end]),
            label: true,
            space: space || (sp.start > a && text[..sp.start].ends_with(char::is_whitespace)),
        });
        space = false;
        at = sp.end;
    }
    if at < b {
        push_plain(&text[at..b], &mut out, &mut space);
    }
    out
}

fn split_long(us: Vec<Unit>, size: f32, maxw: f32) -> Vec<Unit> {
    let mut o = Vec::with_capacity(us.len());
    for u in us {
        if wid(&u.b, size, u.label) <= maxw {
            o.push(u);
            continue;
        }
        let mut cur: Vec<u8> = Vec::new();
        let mut first = true;
        for c in u.b {
            cur.push(c);
            if wid(&cur, size, u.label) > maxw - 6.0 {
                o.push(Unit {
                    b: std::mem::take(&mut cur),
                    label: u.label,
                    space: first && u.space,
                });
                first = false;
            }
        }
        if !cur.is_empty() {
            o.push(Unit {
                b: cur,
                label: u.label,
                space: first && u.space,
            });
        }
    }
    o
}

struct Flow {
    pages: Vec<(String, String, Option<(ObjectId, f32, f32, f32)>)>,
    y: f32,
    left: String,
}

impl Flow {
    fn new() -> Flow {
        Flow {
            pages: Vec::new(),
            y: 0.0,
            left: String::new(),
        }
    }
    fn page(&mut self, left: &str) {
        self.pages.push((String::new(), left.to_string(), None));
        self.left = left.to_string();
        self.y = TOP;
    }
    fn c(&mut self) -> &mut String {
        &mut self.pages.last_mut().unwrap().0
    }
    fn room(&mut self, h: f32) {
        if self.pages.is_empty() || self.y - h < BOTTOM {
            let l = self.left.clone();
            self.page(&l);
        }
    }
    fn gap(&mut self, h: f32) {
        self.y -= h;
    }

    fn line(&mut self, s: &str, size: f32, bold: bool, gray: f32) {
        self.room(size + 4.0);
        self.y -= size + 3.0;
        let b = fit(s, size, W - ML - MR);
        let y = self.y;
        self.c().push_str(&format!(
            "BT /{} {size} Tf {gray} g {ML} {y} Td ({}) Tj ET\n",
            if bold { "FjHelvB" } else { "FjHelv" },
            lit(&b)
        ));
    }

    fn text(&mut self, text: &str, spans: &[Span], size: f32) {
        let lead = size * 1.32;
        let maxw = W - ML - MR;
        let mut blank = 0;
        let mut a = 0usize;
        for line in text.split('\n') {
            let b = a + line.len();
            let us = split_long(units(text, a, b, spans), size, W - ML - MR);
            a = b + 1;
            if us.is_empty() {
                blank += 1;
                if blank == 1 && !self.pages.is_empty() {
                    self.y -= lead * 0.6;
                }
                continue;
            }
            blank = 0;
            let mut row: Vec<&Unit> = Vec::new();
            let mut w = 0.0;
            let sp = wid(b" ", size, false);
            let flush = |row: &mut Vec<&Unit>, fl: &mut Flow| {
                if row.is_empty() {
                    return;
                }
                fl.room(lead);
                fl.y -= lead;
                let y = fl.y;
                let mut x = ML;
                let mut s = String::new();
                for (i, u) in row.iter().enumerate() {
                    if i > 0 && u.space {
                        x += sp;
                    }
                    let uw = wid(&u.b, size, u.label);
                    if u.label {
                        s.push_str(&format!(
                            "0.87 0.92 0.98 rg {} {} {} {} re f\n",
                            x - 1.5,
                            y - size * 0.28,
                            uw + 3.0,
                            size * 1.22
                        ));
                        s.push_str(&format!(
                            "BT /FjHelvB {size} Tf 0.12 0.3 0.55 rg {x} {y} Td ({}) Tj ET\n",
                            lit(&u.b)
                        ));
                    } else {
                        s.push_str(&format!("BT /FjHelv {size} Tf 0.1 g {x} {y} Td ({}) Tj ET\n", lit(&u.b)));
                    }
                    x += uw;
                }
                fl.c().push_str(&s);
                row.clear();
            };
            for u in &us {
                let uw = wid(&u.b, size, u.label);
                let add = if row.is_empty() { uw } else { uw + if u.space { sp } else { 0.0 } };
                if !row.is_empty() && w + add > maxw {
                    flush(&mut row, self);
                    w = 0.0;
                }
                w += if row.is_empty() { uw } else { add };
                row.push(u);
            }
            flush(&mut row, self);
        }
    }
}

pub fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let m = b[i + 1];
        let len = ((b[i + 2] as usize) << 8) | b[i + 3] as usize;
        if (0xC0..=0xCF).contains(&m) && m != 0xC4 && m != 0xC8 && m != 0xCC {
            let h = ((b[i + 5] as u32) << 8) | b[i + 6] as u32;
            let w = ((b[i + 7] as u32) << 8) | b[i + 8] as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

fn stamp(lines: &[String]) -> String {
    let (c, s) = (38f32.to_radians().cos(), 38f32.to_radians().sin());
    let size = 21.0;
    let mut o = format!("q {c} {s} {} {c} {} {} cm\n", -s, W / 2.0, H / 2.0);
    let n = lines.len() as f32;
    let band = n * size * 1.25 + 18.0;
    o.push_str(&format!("1 1 1 rg -420 {} 840 {} re f\n", -band / 2.0, band));
    o.push_str(&format!(
        "0.7 0.15 0.12 RG 3 w -420 {} m 420 {} l S -420 {} m 420 {} l S\n",
        band / 2.0,
        band / 2.0,
        -band / 2.0,
        -band / 2.0
    ));
    for (i, l) in lines.iter().enumerate() {
        let b = cp1252(l);
        let w = wid(&b, size, true);
        let y = (n - 1.0) / 2.0 * size * 1.25 - i as f32 * size * 1.25 - size * 0.35;
        o.push_str(&format!(
            "BT /FjHelvB {size} Tf 0.7 0.15 0.12 rg {} {y} Td ({}) Tj ET\n",
            -w / 2.0,
            lit(&b)
        ));
    }
    o.push_str("Q\n");
    o
}

fn fonts(doc: &mut Document) -> (ObjectId, ObjectId) {
    let font =
        doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" });
    let font_b = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica-Bold", "Encoding" => "WinAnsiEncoding" },
    );
    (font, font_b)
}

fn emit(doc: &mut Document, fl: Flow, font: ObjectId, font_b: ObjectId, right: &dyn Fn(usize) -> String, first_no: usize) -> Vec<ObjectId> {
    let mut ids = Vec::new();
    for (i, (content, left, img)) in fl.pages.into_iter().enumerate() {
        let mut c = String::new();
        let mut res = dictionary! { "Font" => dictionary! { "FjHelv" => Object::Reference(font), "FjHelvB" => Object::Reference(font_b) } };
        if let Some((im, iw, ih, top)) = img {
            let (bw, bh) = (W - 40.0, top - 20.0);
            let k = (bw / iw).min(bh / ih);
            let (dw, dh) = (iw * k, ih * k);
            c.push_str(&format!(
                "q {dw} 0 0 {dh} {} {} cm /FjIm Do Q\n",
                (W - dw) / 2.0,
                20.0 + (bh - dh) / 2.0
            ));
            res.set("XObject", dictionary! { "FjIm" => Object::Reference(im) });
        }
        c.push_str(&content);
        let lb = fit(&left, 8.0, W * 0.55);
        let r = right(first_no + i);
        let rb = cp1252(&r);
        c.push_str(&format!("BT /FjHelv 8 Tf 0.45 g {ML} {} Td ({}) Tj ET\n", H - 26.0, lit(&lb)));
        c.push_str(&format!(
            "BT /FjHelv 8 Tf 0.45 g {} {} Td ({}) Tj ET\n",
            W - MR - wid(&rb, 8.0, false),
            H - 26.0,
            lit(&rb)
        ));
        c.push_str(&format!("0.8 G 0.5 w {ML} {} m {} {} l S\n", H - 32.0, W - MR, H - 32.0));
        let cs = doc.add_object(Stream::new(dictionary! {}, c.into_bytes()));
        ids.push(doc.add_object(dictionary! {
            "Type" => "Page",
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(W), Object::Real(H)],
            "Contents" => Object::Reference(cs),
            "Resources" => res,
        }));
    }
    ids
}

fn finish(
    doc: &mut Document,
    order: &[ObjectId],
    marks: &[(String, ObjectId)],
    title: &str,
    version: &str,
    path: &Path,
) -> Result<(), String> {
    let pages_id = doc.new_object_id();
    for p in order {
        if let Ok(d) = doc.get_dictionary_mut(*p) {
            d.set("Parent", Object::Reference(pages_id));
        }
    }
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => order.iter().map(|i| Object::Reference(*i)).collect::<Vec<_>>(), "Count" => order.len() as i64 }));
    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) };
    if !marks.is_empty() {
        let root = doc.new_object_id();
        let ids: Vec<ObjectId> = marks.iter().map(|_| doc.new_object_id()).collect();
        for (k, (t, dest)) in marks.iter().enumerate() {
            let mut d = dictionary! { "Title" => utf16(t), "Parent" => Object::Reference(root), "Dest" => vec![Object::Reference(*dest), "Fit".into()] };
            if k > 0 {
                d.set("Prev", Object::Reference(ids[k - 1]));
            }
            if k + 1 < ids.len() {
                d.set("Next", Object::Reference(ids[k + 1]));
            }
            doc.objects.insert(ids[k], Object::Dictionary(d));
        }
        doc.objects.insert(root, Object::Dictionary(dictionary! { "Type" => "Outlines", "First" => Object::Reference(ids[0]), "Last" => Object::Reference(*ids.last().unwrap()), "Count" => ids.len() as i64 }));
        catalog.set("Outlines", Object::Reference(root));
        catalog.set("PageMode", "UseOutlines");
    }
    let cat = doc.add_object(catalog);
    doc.trailer.set("Root", Object::Reference(cat));
    let info = doc.add_object(dictionary! {
        "Title" => utf16(title),
        "Creator" => utf16(&format!("FetchJ {version}")),
        "CreationDate" => Object::string_literal(chrono::Local::now().format("D:%Y%m%d%H%M%S").to_string()),
    });
    doc.trailer.set("Info", Object::Reference(info));
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("pdf.part");
    {
        let f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut w = std::io::BufWriter::new(f);
        let opt = lopdf::SaveOptions::builder()
            .use_object_streams(true)
            .use_xref_streams(true)
            .build();
        doc.save_with_options(&mut w, opt).map_err(|e| e.to_string())?;
        std::io::Write::flush(&mut w).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
pub fn write_case(
    path: &Path,
    t: &Tx,
    case: &str,
    legend_counts: &str,
    docs: &[DocOut],
    version: &str,
) -> Result<(usize, Vec<usize>, usize), String> {
    let mut doc = Document::with_version("1.7");
    let (font, font_b) = fonts(&mut doc);
    let n = docs.len();

    let toc_per_page = 50usize;
    let toc_pages = if n == 0 { 0 } else { n.div_ceil(toc_per_page) };
    let front = 1 + toc_pages;

    let mut body = Flow::new();
    let mut start: Vec<usize> = Vec::with_capacity(n);
    let mut blurred = 0usize;
    let mut images: Vec<(usize, ObjectId, f32, f32, f32)> = Vec::new();
    for d in docs {
        let head = t.f("pdf_doc", &[("i", d.n.to_string()), ("n", n.to_string())]);
        let left = format!("{head} · {} · {}", d.date, d.tipo);
        body.page(&left);
        start.push(front + body.pages.len());
        body.line(
            &format!(
                "{head} · {} · {}{}",
                d.date,
                d.tipo,
                if d.estado.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", d.estado)
                }
            ),
            9.0,
            false,
            0.4,
        );
        body.gap(2.0);
        body.text(&d.title, &d.title_spans, 12.5);
        body.gap(8.0);
        if !d.note.is_empty() {
            body.text(&d.note, &[], 10.0);
        }

        let mut fresh = true;
        for p in &d.pages {
            match p {
                PageOut::Text { no, text, spans } => {
                    fresh = false;
                    body.room(30.0);
                    body.gap(4.0);
                    body.line(&format!("— {} —", t.f("pdf_page", &[("p", no.to_string())])), 8.0, false, 0.55);
                    body.text(text, spans, 9.5);
                }
                PageOut::Blur {
                    no,
                    img,
                    photo,
                    ocr,
                    unsure,
                } => {
                    blurred += 1;

                    let top = if fresh && body.y > 600.0 {
                        body.pages.last_mut().unwrap().1 = format!("{left} · {}", t.f("pdf_page", &[("p", no.to_string())]));
                        body.y - 6.0
                    } else {
                        body.page(&format!("{left} · {}", t.f("pdf_page", &[("p", no.to_string())])));
                        H - 40.0
                    };
                    fresh = false;
                    let lines: Vec<String> = t
                        .s(if *photo { "pdf_stamp_img" } else { "pdf_stamp" })
                        .split('\n')
                        .map(String::from)
                        .collect();
                    if let Some(ip) = img.as_ref().and_then(|p| std::fs::read(p).ok()) {
                        if let Some((iw, ih)) = jpeg_size(&ip) {
                            let im = doc.add_object(Stream::new(
                                dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => iw as i64, "Height" => ih as i64, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8, "Filter" => "DCTDecode" },
                                ip,
                            ));
                            images.push((body.pages.len() - 1, im, iw as f32, ih as f32, top));
                        }
                    } else {
                        body.c().push_str(&format!("0.93 g 40 30 515 {} re f\n", top - 40.0));
                    }
                    body.c().push_str(&stamp(&lines));
                    match ocr {
                        Some((tx, sp)) => {
                            body.page(&left);
                            body.line(&t.f("pdf_ocr", &[("p", no.to_string())]), 9.0, true, 0.3);
                            body.gap(4.0);
                            body.text(tx, sp, 9.5);
                        }
                        None if *unsure => {
                            body.y = 80.0;
                            body.line(&t.s("pdf_ocr_unsure"), 9.0, true, 0.3);
                        }
                        None => {}
                    }

                    body.y = BOTTOM;
                }
            }
        }
    }
    for (i, im, iw, ih, top) in images {
        body.pages[i].2 = Some((im, iw, ih, top));
    }
    let total = front + body.pages.len();
    let right = |p: usize| {
        t.f(
            "pdf_hdr",
            &[("c", case.to_string()), ("p", p.to_string()), ("n", total.to_string())],
        )
    };

    let mut cv = Flow::new();
    cv.page(&t.s("pdf_title_anon"));
    cv.c().push_str("0.12 0.3 0.55 rg 56 700 483 3 re f\n");
    cv.y = 760.0;
    cv.line(&t.s("pdf_title_anon"), 24.0, true, 0.1);
    cv.gap(6.0);
    cv.line(&t.f("pdf_case", &[("c", case.to_string())]), 15.0, true, 0.25);
    cv.y = 690.0;
    cv.line(
        &t.f(
            "pdf_made",
            &[
                ("d", chrono::Local::now().format("%d/%m/%Y %H:%M").to_string()),
                ("n", n.to_string()),
                ("p", total.to_string()),
            ],
        ),
        10.0,
        false,
        0.35,
    );
    cv.gap(14.0);
    for l in t.lines("pdf_legend") {
        cv.text(&format!("• {l}"), &[], 10.5);
        cv.gap(4.0);
    }
    if !legend_counts.is_empty() {
        cv.gap(6.0);
        cv.text(legend_counts, &[], 10.0);
    }

    let mut toc = Flow::new();
    for k in 0..toc_pages {
        toc.page(&t.s("pdf_toc"));
        toc.line(&t.s("pdf_toc"), 15.0, true, 0.1);
        toc.gap(6.0);
        for (j, d) in docs.iter().enumerate().skip(k * toc_per_page).take(toc_per_page) {
            let s = format!("{:>3}  {}  {}  ", d.n, d.date, d.tipo);
            let pg = start[j].to_string();
            let pb = cp1252(&pg);
            let pw = wid(&pb, 9.0, false);
            toc.y -= 13.2;
            let y = toc.y;
            let left = fit(&format!("{s}{}", d.title), 9.0, W - ML - MR - pw - 14.0);
            toc.c().push_str(&format!(
                "BT /FjHelv 9 Tf 0.12 g {ML} {y} Td ({}) Tj ET BT /FjHelv 9 Tf 0.12 g {} {y} Td ({}) Tj ET\n",
                lit(&left),
                W - MR - pw,
                lit(&pb)
            ));
        }
    }
    let mut order = emit(&mut doc, cv, font, font_b, &right, 1);
    order.extend(emit(&mut doc, toc, font, font_b, &right, 2));
    let body_ids = emit(&mut doc, body, font, font_b, &right, front + 1);
    let marks: Vec<(String, ObjectId)> = docs
        .iter()
        .enumerate()
        .filter_map(|(j, d)| {
            body_ids
                .get(start[j] - front - 1)
                .map(|id| (format!("{} · {} · {}", d.n, d.date, d.title), *id))
        })
        .collect();
    order.extend(body_ids);
    finish(
        &mut doc,
        &order,
        &marks,
        &format!("{case} – {}", t.s("pdf_title_anon")),
        version,
        path,
    )?;
    Ok((total, start, blurred))
}

pub fn write_research(
    path: &Path,
    t: &Tx,
    case: &str,
    analysis: Option<&Value>,
    judgments: &[(String, PathBuf)],
    version: &str,
) -> Result<(usize, usize), String> {
    let mut doc = Document::with_version("1.7");
    let (font, font_b) = fonts(&mut doc);
    let mut fl = Flow::new();
    fl.page(&t.s("pdf_title_rs"));
    fl.c().push_str("0.6 0.47 0.1 rg 56 700 483 3 re f\n");
    fl.y = 760.0;
    fl.line(&t.s("pdf_title_rs"), 24.0, true, 0.1);
    fl.gap(6.0);
    fl.line(&t.f("pdf_case", &[("c", case.to_string())]), 15.0, true, 0.25);
    fl.y = 690.0;
    fl.line(
        &t.f(
            "pdf_made_rs",
            &[
                ("d", chrono::Local::now().format("%d/%m/%Y %H:%M").to_string()),
                ("n", judgments.len().to_string()),
            ],
        ),
        10.0,
        false,
        0.35,
    );
    fl.gap(16.0);
    let list = |v: &Value, f: &dyn Fn(&Value) -> String| -> Vec<String> {
        v.as_array()
            .map(|a| a.iter().map(f).filter(|s| !s.trim().is_empty()).collect())
            .unwrap_or_default()
    };
    match analysis {
        Some(a) => {
            fl.line(&t.s("rs_analysis"), 14.0, true, 0.1);
            fl.gap(4.0);
            let mut kv = |k: &str, v: String| {
                if !v.trim().is_empty() {
                    fl.text(&format!("{}: {}", t.s(k), v), &[], 10.0);
                    fl.gap(2.0);
                }
            };
            kv(
                "rs_matter",
                format!(
                    "{} ({})",
                    a["materia"]["name"].as_str().unwrap_or(""),
                    a["materia"]["code"].as_str().unwrap_or("")
                ),
            );
            kv("rs_inst", a["instancia"].as_str().unwrap_or("").to_string());
            kv("rs_court", a["despacho_actual"].as_str().unwrap_or("").to_string());
            kv("rs_state", a["estado"].as_str().unwrap_or("").to_string());
            kv("rs_at", a["analyzed_at"].as_str().unwrap_or("").to_string());
            let sections: [(&str, &str, &dyn Fn(&Value) -> String); 5] = [
                ("rs_delitos", "delitos", &|d: &Value| {
                    format!(
                        "{}{} ({})",
                        d["canon"].as_str().unwrap_or(""),
                        if d["negado"].as_bool().unwrap_or(false) {
                            " – negado"
                        } else {
                            ""
                        },
                        d["score"]
                    )
                }),
                ("rs_arts", "articulos", &|d: &Value| {
                    format!(
                        "{} art. {}{} ({}×)",
                        d["ley"].as_str().unwrap_or(""),
                        d["art"].as_str().unwrap_or(""),
                        d["inciso"].as_str().map(|i| format!(" inc. {i}")).unwrap_or_default(),
                        d["count"]
                    )
                }),
                ("rs_laws", "leyes", &|d: &Value| {
                    format!("{} ({}×)", d["nombre"].as_str().unwrap_or(""), d["count"])
                }),
                ("rs_result", "resultado", &|d: &Value| {
                    format!("{} ({}×)", d["valor"].as_str().unwrap_or(""), d["count"])
                }),
                ("rs_kw", "stichwoerter", &|d: &Value| {
                    format!("{} ({}×)", d["wort"].as_str().unwrap_or(""), d["count"])
                }),
            ];
            for (h, k, f) in sections {
                let l = list(&a[k], f);
                if l.is_empty() {
                    continue;
                }
                fl.gap(8.0);
                fl.line(&t.s(h), 11.5, true, 0.15);
                fl.text(&l.join(" · "), &[], 10.0);
            }
            let nx = &a["nexus"];
            let words: Vec<String> = ["alle", "mindestens", "stichwoerter"]
                .iter()
                .flat_map(|k| {
                    list(&nx[*k], &|x: &Value| {
                        x.as_str()
                            .map(String::from)
                            .or_else(|| x["wort"].as_str().map(String::from))
                            .unwrap_or_default()
                    })
                })
                .collect();
            if !words.is_empty() {
                fl.gap(8.0);
                fl.line(&t.s("rs_nexus"), 11.5, true, 0.15);
                fl.text(&words.join(" · "), &[], 10.0);
            }
        }
        None => fl.text(&t.s("rs_noan"), &[], 10.5),
    }
    fl.gap(14.0);
    fl.line(&t.f("rs_judg", &[("n", judgments.len().to_string())]), 14.0, true, 0.1);
    if judgments.is_empty() {
        fl.text(&t.s("rs_nojudg"), &[], 10.5);
    }
    for (i, (name, _)) in judgments.iter().enumerate() {
        fl.text(&format!("{}. {}", i + 1, name), &[], 10.0);
    }
    let front_n = fl.pages.len();

    let mut parts: Vec<(String, Vec<ObjectId>)> = Vec::new();
    for (name, p) in judgments {
        if let Ok(pg) = crate::casepdf::take_pdf(&mut doc, p) {
            parts.push((name.clone(), pg));
        }
    }
    let total = front_n + parts.iter().map(|p| p.1.len()).sum::<usize>();
    let right = |p: usize| {
        t.f(
            "pdf_hdr_rs",
            &[("c", case.to_string()), ("p", p.to_string()), ("n", total.to_string())],
        )
    };
    let mut order = emit(&mut doc, fl, font, font_b, &right, 1);
    let mut marks = vec![(t.s("rs_analysis"), order[0])];
    for (name, pg) in &parts {
        if let Some(f) = pg.first() {
            marks.push((name.clone(), *f));
        }
        order.extend(pg.iter().cloned());
    }
    finish(
        &mut doc,
        &order,
        &marks,
        &format!("{case} – {}", t.s("pdf_title_rs")),
        version,
        path,
    )?;
    Ok((total, parts.len()))
}

pub fn pdf_text(p: &Path) -> String {
    let Ok(d) = Document::load(p) else { return String::new() };
    let n: Vec<u32> = d.get_pages().keys().cloned().collect();
    d.extract_text(&n).unwrap_or_default()
}

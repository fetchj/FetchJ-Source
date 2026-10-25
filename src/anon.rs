use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Party,
    Person,
    Contact,
    Own,
    Email,
    Phone,
    Id,
    Plate,
    Account,
    Address,
    Birth,
    Case,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Party => "party",
            Kind::Person => "person",
            Kind::Contact => "contact",
            Kind::Own => "own",
            Kind::Email => "email",
            Kind::Phone => "phone",
            Kind::Id => "id",
            Kind::Plate => "plate",
            Kind::Account => "account",
            Kind::Address => "address",
            Kind::Birth => "birth",
            Kind::Case => "case",
        }
    }
    pub const ALL: [Kind; 12] = [
        Kind::Party,
        Kind::Person,
        Kind::Contact,
        Kind::Own,
        Kind::Email,
        Kind::Phone,
        Kind::Id,
        Kind::Plate,
        Kind::Account,
        Kind::Address,
        Kind::Birth,
        Kind::Case,
    ];
}

#[derive(Clone, Debug, Serialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub label: String,
    pub kind: Kind,
    pub orig: String,
}

pub fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn tokens(s: &str) -> Vec<(usize, usize, String)> {
    let mut v = Vec::new();
    let mut st: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if c.is_alphanumeric() {
            if st.is_none() {
                st = Some(i);
            }
        } else if let Some(a) = st.take() {
            v.push((a, i, fold(&s[a..i])));
        }
    }
    if let Some(a) = st {
        v.push((a, s.len(), fold(&s[a..])));
    }
    v
}

const STOP: &[&str] = &[
    "de", "del", "la", "las", "los", "y", "e", "van", "von", "da", "do", "dos", "das", "di", "le", "el", "san", "santa", "mc",
];

static COMMON: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "mora",
        "paz",
        "luz",
        "cruz",
        "leon",
        "rosa",
        "rosas",
        "flor",
        "flores",
        "campo",
        "campos",
        "rios",
        "rio",
        "vega",
        "monte",
        "montes",
        "castillo",
        "torre",
        "torres",
        "prado",
        "prados",
        "lago",
        "sierra",
        "valle",
        "fuente",
        "fuentes",
        "puerto",
        "costa",
        "rica",
        "republica",
        "estado",
        "banco",
        "nacional",
        "caja",
        "seguro",
        "social",
        "ministerio",
        "publico",
        "publica",
        "fiscalia",
        "juzgado",
        "tribunal",
        "sala",
        "corte",
        "poder",
        "judicial",
        "defensa",
        "procuraduria",
        "general",
        "municipalidad",
        "instituto",
        "sociedad",
        "anonima",
        "limitada",
        "empresa",
        "compania",
        "cooperativa",
        "asociacion",
        "fundacion",
        "corporacion",
        "grupo",
        "servicios",
        "inversiones",
        "comercial",
        "internacional",
        "central",
        "norte",
        "oeste",
        "este",
        "pacifico",
        "atlantico",
        "caribe",
        "sentencia",
        "resolucion",
        "audiencia",
        "derecho",
        "justicia",
        "gracia",
        "gloria",
        "victoria",
        "esperanza",
        "amparo",
        "socorro",
        "remedios",
        "dolores",
        "mercedes",
        "pilar",
        "rocio",
        "consuelo",
        "angeles",
        "santos",
        "reyes",
        "rey",
        "blanco",
        "blanca",
        "negro",
        "moreno",
        "rojo",
        "rojas",
        "verde",
        "pena",
        "bravo",
        "leal",
        "franco",
        "real",
        "nuevo",
        "nueva",
        "alto",
        "alta",
        "bajo",
        "baja",
        "grande",
        "solano",
        "salas",
        "calvo",
        "duran",
        "parra",
        "mesa",
        "barrio",
        "cantón",
        "canton",
        "distrito",
        "provincia",
        "jose",
        "alajuela",
        "heredia",
        "cartago",
        "puntarenas",
        "limon",
        "guanacaste",
        "liberia",
        "pococi",
        "talamanca",
        "siquirres",
        "guapiles",
        "nicoya",
        "escazu",
        "desamparados",
        "goicoechea",
        "tibas",
        "moravia",
        "curridabat",
        "perez",
        "zeledon",
        "turrialba",
        "grecia",
        "naranjo",
        "palmares",
        "atenas",
        "orotina",
        "esparza",
        "quepos",
        "golfito",
        "osa",
        "corredores",
        "coto",
        "buenos",
        "aires",
        "upala",
        "guatuso",
        "matina",
        "bribri",
        "sixaola",
        "cahuita",
        "maria",
        "juan",
        "luis",
        "carlos",
        "ana",
        "jorge",
        "manuel",
        "francisco",
        "antonio",
        "jesus",
        "angel",
        "miguel",
        "pedro",
        "pablo",
        "rafael",
        "alberto",
        "andres",
        "eduardo",
        "fernando",
        "ricardo",
        "roberto",
        "mario",
        "sergio",
        "daniel",
        "david",
        "diego",
        "alejandro",
        "javier",
        "jose",
        "laura",
        "andrea",
        "carmen",
        "patricia",
        "sofia",
        "isabel",
        "elena",
        "marta",
        "martha",
        "silvia",
        "monica",
        "adriana",
        "gabriela",
        "paola",
        "karla",
        "carolina",
        "daniela",
        "mariana",
        "valeria",
        "natalia",
        "lucia",
        "teresa",
        "cristina",
        "alejandra",
        "fabiola",
    ]
    .into_iter()
    .collect()
});

static NOT_NAME: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "juez",
        "jueza",
        "fiscal",
        "fiscala",
        "defensor",
        "defensora",
        "licenciado",
        "licenciada",
        "tribunal",
        "juzgado",
        "sala",
        "ministerio",
        "publico",
        "publica",
        "penal",
        "civil",
        "agrario",
        "laboral",
        "familia",
        "contencioso",
        "administrativo",
        "segundo",
        "primero",
        "tercero",
        "circuito",
        "judicial",
        "zona",
        "atlantica",
        "atlantico",
        "sur",
        "norte",
        "este",
        "oeste",
        "republica",
        "costa",
        "rica",
        "por",
        "tanto",
        "considerando",
        "resultando",
        "resuelve",
        "expediente",
        "folio",
        "imputado",
        "imputada",
        "ofendido",
        "ofendida",
        "testigo",
        "victima",
        "actor",
        "actora",
        "demandado",
        "demandada",
        "en",
        "con",
        "que",
        "quien",
        "como",
        "para",
        "sobre",
        "ante",
        "estado",
        "unidos",
        "mexicanos",
        "abogado",
        "abogada",
        "notario",
        "notaria",
        "perito",
        "perita",
        "medico",
        "medica",
        "forense",
        "oij",
        "pj",
    ]
    .into_iter()
    .collect()
});

fn is_company(folded: &[String]) -> bool {
    const MARK: &[&str] = &[
        "sa",
        "s",
        "srl",
        "ltda",
        "limitada",
        "sociedad",
        "anonima",
        "banco",
        "municipalidad",
        "instituto",
        "ministerio",
        "estado",
        "caja",
        "cooperativa",
        "asociacion",
        "fundacion",
        "corporacion",
        "empresa",
        "compania",
        "procuraduria",
        "universidad",
        "colegio",
        "consejo",
        "junta",
        "comite",
        "grupo",
        "inversiones",
        "servicios",
    ];
    folded
        .iter()
        .any(|t| MARK.contains(&t.as_str()) || t.chars().any(|c| c.is_ascii_digit()))
}

const CAP: &str = r"(?:[A-ZÁÉÍÓÚÑÜ][a-záéíóúñü]+|[A-ZÁÉÍÓÚÑÜ]{2,})";

fn chain(min: usize) -> String {
    format!(r"({CAP}(?:\s+(?:de\s+la\s+|de\s+|del\s+)?{CAP}){{{},4}})", min.saturating_sub(1))
}

static TITLED: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(&format!(r"\b(?:Lic(?:da|do)?|Lcd[oa]|Licenciad[oa]|Dr|Dra|Doctor|Doctora|Sr|Sra|Srta|Señor|Señora|Señorita|Don|Doña|MSc|M\.Sc|Máster|Master|Ing|Bach|Prof|Profesor|Profesora)\.?\s+{}", chain(1))).unwrap(),
        Regex::new(&format!(
            r"\b(?i:imputad[oa]s?|ofendid[oa]s?|testigos?|v[ií]ctimas?|denunciantes?|actor(?:a)?|demandad[oa]s?|querellantes?|perit[oa]s?|defensor(?:a)?|apoderad[oa]s?|representantes?|esposa|esposo|c[oó]nyuge|hij[oa]s?|madre|padre|herman[oa]s?|menor(?:es)?|sentenciad[oa]s?|acusad[oa]s?)\s*:?\s+(?:(?i:el|la|señor|señora|don|doña|joven|niñ[oa])\s+)?{}",
            chain(2)
        ))
        .unwrap(),
    ]
});

static PATTERNS: Lazy<Vec<(Regex, Kind, usize)>> = Lazy::new(|| {
    let r = |s: &str| Regex::new(s).unwrap();
    vec![
        (r(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)+"), Kind::Email, 0),
        (
            r(r"\b[A-Z]{2}\d{2}(?:[ ]?[A-Z0-9]{4}){3,7}(?:[ ]?[A-Z0-9]{1,3})?\b"),
            Kind::Account,
            0,
        ),
        (r(r"\b\d{3}-\d{2}-\d{3}-\d{6}-\d\b"), Kind::Account, 0),
        (
            r(
                r"(?i)\b(?:cuenta|cta\.?|tarjeta|iban|cliente)(?:\s+(?:corriente|de\s+ahorros?|bancaria|n[º°o.]*|número|numero))*\s*:?\s*([0-9][0-9 \-]{7,30}[0-9])",
            ),
            Kind::Account,
            1,
        ),
        (r(r"\b\d{15,22}\b"), Kind::Account, 0),
        (r(r"\b0?[1-9][\- ]\d{3,4}[\- ]\d{3,4}\b"), Kind::Id, 0),
        (r(r"\b0?[1-9]\d{8}\b"), Kind::Id, 0),
        (r(r"\b\d{11,12}\b"), Kind::Id, 0),
        (
            r(
                r"(?i)\b(?:c[ée]dula|identificaci[oó]n|pasaporte|dimex|documento\s+de\s+identidad)(?:\s+(?:de\s+identidad|de\s+residencia|n[º°o.]*|número|numero))*\s*:?\s*([A-Z0-9][A-Z0-9\-\. ]{4,20}[A-Z0-9])",
            ),
            Kind::Id,
            1,
        ),
        (r(r"(?:\+?506[\- ]?)?\b[2-8]\d{3}[\- ]?\d{4}\b"), Kind::Phone, 0),
        (
            r(r"(?i)\b(?:tel[ée]fonos?|tel\.|cel(?:ular)?\.?|m[oó]vil|whatsapp|fax)\s*:?\s*(\+?[0-9][0-9 \-()]{6,18}[0-9])"),
            Kind::Phone,
            1,
        ),
        (r(r"\b[A-Z]{3}[\- ]?\d{3}\b"), Kind::Plate, 0),
        (
            r(r"(?i)\bplacas?(?:\s+(?:n[º°o.]*|número|numero|de\s+matr[ií]cula))*\s*:?\s*([A-Z]{0,4}[\- ]?\d{2,7})\b"),
            Kind::Plate,
            1,
        ),
        (
            r(
                r"(?i)\b\d{1,4}\s*(?:m|mts|metros|varas|km|kil[oó]metros)\.?\s*(?:al\s+|hacia\s+el\s+|en\s+direcci[oó]n\s+)?(?:norte|sur|este|oeste|noreste|noroeste|sureste|suroeste)\b[^.\n;]{0,120}",
            ),
            Kind::Address,
            0,
        ),
        (
            r(
                r"(?i)\b(?:domiciliad[oa]|vecin[oa]|resid(?:e|ente)|vive|con\s+domicilio|direcci[oó]n\s+exacta|direcci[oó]n)\s*(?:en|de|:)\s+([^.\n;]{3,120})",
            ),
            Kind::Address,
            1,
        ),
        (
            r(
                r"(?i)\b(?:fecha\s+de\s+nacimiento|nacid[oa]\s+(?:el|en\s+fecha|el\s+d[ií]a))\s*:?\s*(\d{1,2}\s*[/.\-]\s*\d{1,2}\s*[/.\-]\s*\d{2,4}|\d{1,2}\s+de\s+[a-záéíóú]+\s+(?:de|del)\s+\d{4})",
            ),
            Kind::Birth,
            1,
        ),
    ]
});

static CASE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b\d{2}-?\d{6}-?\d{4}-?[A-Z]{2}\b").unwrap());

struct NamePat {
    toks: Vec<String>,
    label: String,
    kind: Kind,
    orig: String,
}

pub struct Anon {
    pats: HashMap<String, Vec<NamePat>>,
    seen: HashSet<Vec<String>>,
    singles: HashMap<String, usize>,
    never: HashSet<String>,

    words: HashMap<Kind, String>,
    shared: String,
    counters: HashMap<Kind, usize>,
    by_value: HashMap<(Kind, String), String>,
    pub case_numbers: bool,

    pub map: BTreeMap<String, BTreeSet<String>>,

    pub count: BTreeMap<Kind, usize>,

    originals: Vec<Vec<String>>,
}

impl Anon {
    pub fn new(words: HashMap<Kind, String>, shared: &str) -> Anon {
        Anon {
            pats: HashMap::new(),
            seen: HashSet::new(),
            singles: HashMap::new(),
            never: HashSet::new(),
            words,
            shared: format!("[{shared}]"),
            counters: HashMap::new(),
            by_value: HashMap::new(),
            case_numbers: false,
            map: BTreeMap::new(),
            count: BTreeMap::new(),
            originals: Vec::new(),
        }
    }

    fn next_label(&mut self, kind: Kind) -> String {
        let n = self.counters.entry(kind).or_insert(0);
        *n += 1;
        format!(
            "[{} {}]",
            self.words.get(&kind).cloned().unwrap_or_else(|| kind.key().to_string()),
            n
        )
    }

    pub fn never(&mut self, phrase: &str) {
        let f: Vec<String> = tokens(phrase).into_iter().map(|t| t.2).collect();
        if !f.is_empty() {
            self.never.insert(f.join(" "));
        }
    }

    fn insert(&mut self, toks: Vec<String>, label: &str, kind: Kind, orig: &str) {
        if toks.is_empty() || self.seen.contains(&toks) || self.never.contains(&toks.join(" ")) {
            return;
        }
        self.seen.insert(toks.clone());
        let l = self.pats.entry(toks[0].clone()).or_default();
        l.push(NamePat {
            toks,
            label: label.to_string(),
            kind,
            orig: orig.to_string(),
        });
        l.sort_by(|a, b| b.toks.len().cmp(&a.toks.len()));
    }

    pub fn person(&mut self, name: &str, label: &str, kind: Kind) {
        self.person_parts(name, label, kind, true)
    }

    pub fn person_full(&mut self, name: &str, label: &str, kind: Kind) {
        self.person_parts(name, label, kind, false)
    }

    fn person_parts(&mut self, name: &str, label: &str, kind: Kind, singles: bool) {
        let t: Vec<String> = tokens(name).into_iter().map(|t| t.2).collect();
        if t.is_empty() || t.iter().all(|w| STOP.contains(&w.as_str())) {
            return;
        }
        self.originals.push(t.clone());
        self.insert(t.clone(), label, kind, name.trim());
        let company = is_company(&t);
        let n = t.len();
        if !company {
            for len in (2..n).rev() {
                for i in 0..=n - len {
                    let sub = &t[i..i + len];
                    if STOP.contains(&sub[0].as_str()) || STOP.contains(&sub[len - 1].as_str()) {
                        continue;
                    }
                    self.insert(sub.to_vec(), label, kind, name.trim());
                }
            }
            for w in t.iter().filter(|_| singles) {
                if w.chars().count() < 4
                    || STOP.contains(&w.as_str())
                    || COMMON.contains(w.as_str())
                    || w.chars().all(|c| c.is_ascii_digit())
                    || self.never.contains(w)
                {
                    continue;
                }
                let k = vec![w.clone()];
                if self.seen.contains(&k) {
                    if let Some(l) = self.pats.get_mut(w) {
                        for p in l.iter_mut().filter(|p| p.toks.len() == 1) {
                            if p.label != label {
                                p.label = self.shared.clone();
                            }
                        }
                    }
                    continue;
                }
                *self.singles.entry(w.clone()).or_insert(0) += 1;
                self.insert(k, label, kind, name.trim());
            }
        }
    }

    fn found_person(&mut self, name: &str) {
        let mut t: Vec<(usize, usize, String)> = tokens(name);
        while t
            .last()
            .map(|x| NOT_NAME.contains(x.2.as_str()) || STOP.contains(&x.2.as_str()))
            .unwrap_or(false)
        {
            t.pop();
        }
        while t
            .first()
            .map(|x| NOT_NAME.contains(x.2.as_str()) || STOP.contains(&x.2.as_str()))
            .unwrap_or(false)
        {
            t.remove(0);
        }
        if t.len() < 2 || t.iter().any(|x| NOT_NAME.contains(x.2.as_str())) {
            return;
        }
        let f: Vec<String> = t.iter().map(|x| x.2.clone()).collect();
        if self.seen.contains(&f) {
            return;
        }
        let label = self.next_label(Kind::Person);
        let orig = &name[t[0].0..t[t.len() - 1].1];

        self.originals.push(f.clone());
        self.insert(f.clone(), &label, Kind::Person, orig);
        let n = f.len();
        for len in (2..n).rev() {
            for i in 0..=n - len {
                let sub = &f[i..i + len];
                if !STOP.contains(&sub[0].as_str()) && !STOP.contains(&sub[len - 1].as_str()) {
                    self.insert(sub.to_vec(), &label, Kind::Person, orig);
                }
            }
        }
    }

    pub fn discover(&mut self, text: &str) {
        for re in TITLED.iter() {
            for c in re.captures_iter(text) {
                if let Some(m) = c.get(1) {
                    self.found_person(m.as_str());
                }
            }
        }
    }

    fn value_label(&mut self, kind: Kind, orig: &str) -> String {
        let key = match kind {
            Kind::Address | Kind::Own => fold(orig).split_whitespace().collect::<Vec<_>>().join(" "),
            Kind::Email => orig.to_lowercase(),
            _ => orig.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_uppercase(),
        };
        if let Some(l) = self.by_value.get(&(kind, key.clone())) {
            return l.clone();
        }
        let l = self.next_label(kind);
        self.by_value.insert((kind, key), l.clone());
        l
    }

    pub fn always(&mut self, phrase: &str) {
        let p = phrase.trim();
        if p.is_empty() {
            return;
        }
        let l = self.value_label(Kind::Own, p);
        let t: Vec<String> = tokens(p).into_iter().map(|t| t.2).collect();
        if !t.is_empty() {
            self.originals.push(t.clone());
            self.seen.remove(&t);
            self.insert(t, &l, Kind::Own, p);
        }
    }

    fn find(&mut self, text: &str) -> Vec<(usize, usize, String, Kind, String)> {
        let mut out: Vec<(usize, usize, String, Kind, String)> = Vec::new();
        let toks = tokens(text);
        let gap_ok = |a: usize, b: usize| {
            let g = &text[a..b];
            g.chars().count() <= 4 && g.chars().all(|c| c.is_whitespace() || matches!(c, '-' | '.' | ',' | '\''))
        };
        let mut i = 0;
        while i < toks.len() {
            let mut hit = None;
            if let Some(list) = self.pats.get(&toks[i].2) {
                for p in list {
                    let n = p.toks.len();
                    if i + n > toks.len() {
                        continue;
                    }
                    if (0..n).all(|k| toks[i + k].2 == p.toks[k] && (k == 0 || gap_ok(toks[i + k - 1].1, toks[i + k].0))) {
                        hit = Some((n, p.label.clone(), p.kind, p.orig.clone()));
                        break;
                    }
                }
            }
            if let Some((n, label, kind, orig)) = hit {
                out.push((toks[i].0, toks[i + n - 1].1, label, kind, orig));
                i += n;
            } else {
                i += 1;
            }
        }
        let free = |out: &Vec<(usize, usize, String, Kind, String)>, a: usize, b: usize| !out.iter().any(|x| a < x.1 && x.0 < b);

        let mut add: Vec<(usize, usize, Kind)> = Vec::new();
        for re in TITLED.iter() {
            for c in re.captures_iter(text) {
                if let Some(m) = c.get(1) {
                    let mut t = tokens(m.as_str());
                    while t
                        .last()
                        .map(|x| NOT_NAME.contains(x.2.as_str()) || STOP.contains(&x.2.as_str()))
                        .unwrap_or(false)
                    {
                        t.pop();
                    }
                    if t.is_empty()
                        || NOT_NAME.contains(t[0].2.as_str())
                        || STOP.contains(&t[0].2.as_str())
                        || COMMON.contains(t[0].2.as_str()) && t.len() == 1
                    {
                        continue;
                    }
                    add.push((m.start() + t[0].0, m.start() + t[t.len() - 1].1, Kind::Person));
                }
            }
        }
        for (re, kind, g) in PATTERNS.iter() {
            for c in re.captures_iter(text) {
                if let Some(m) = c.get(*g) {
                    let s = m.as_str().trim_end();
                    if !s.is_empty() {
                        add.push((m.start(), m.start() + s.len(), *kind));
                    }
                }
            }
        }
        if self.case_numbers {
            for m in CASE_RE.find_iter(text) {
                add.push((m.start(), m.end(), Kind::Case));
            }
        }

        add.sort_by(|a, b| (b.1 - b.0).cmp(&(a.1 - a.0)).then(a.0.cmp(&b.0)));
        for (a, b, kind) in add {
            if !free(&out, a, b) {
                continue;
            }
            let orig = text[a..b].to_string();
            if self
                .never
                .contains(&tokens(&orig).into_iter().map(|t| t.2).collect::<Vec<_>>().join(" "))
            {
                continue;
            }

            if matches!(kind, Kind::Phone | Kind::Id | Kind::Account) && CASE_RE.find_iter(text).any(|m| m.start() <= a && b <= m.end()) {
                continue;
            }
            let label = if kind == Kind::Person {
                self.value_label(Kind::Person, &orig)
            } else {
                self.value_label(kind, &orig)
            };
            out.push((a, b, label, kind, orig));
        }
        out.sort_by_key(|x| x.0);
        out
    }

    pub fn apply(&mut self, text: &str) -> (String, Vec<Span>) {
        let f = self.find(text);
        let mut o = String::with_capacity(text.len());
        let mut spans = Vec::with_capacity(f.len());
        let mut at = 0;
        for (a, b, label, kind, orig) in f {
            if a < at {
                continue;
            }
            o.push_str(&text[at..a]);
            let st = o.len();
            o.push_str(&label);
            let _ = orig;
            let original = text[a..b].to_string();
            self.map.entry(label.clone()).or_default().insert(original.clone());
            *self.count.entry(kind).or_insert(0) += 1;
            spans.push(Span {
                start: st,
                end: o.len(),
                label,
                kind,
                orig: original,
            });
            at = b;
        }
        o.push_str(&text[at..]);
        (o, spans)
    }

    pub fn verify(&mut self, text: &str) -> Vec<String> {
        let mut left = Vec::new();

        let lab = Regex::new(r"\[[^\[\]\n]{1,60}\]").unwrap();
        let plain = lab.replace_all(text, |c: &regex::Captures| " ".repeat(c[0].len())).to_string();
        let saved = (self.counters.clone(), self.by_value.clone());
        for (_, _, _, kind, orig) in self.find(&plain) {
            left.push(format!("{}: {}", kind.key(), orig));
        }

        let words: Vec<String> = tokens(&plain).into_iter().map(|t| t.2).collect();
        for o in &self.originals {
            if o.len() >= 2 && words.windows(o.len()).any(|w| w == o.as_slice()) && !self.never.contains(&o.join(" ")) {
                left.push(format!("name: {}", o.join(" ")));
            }
        }
        self.counters = saved.0;
        self.by_value = saved.1;
        left.sort();
        left.dedup();
        left
    }

    pub fn back(map: &BTreeMap<String, BTreeSet<String>>, text: &str) -> String {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort_by(|a, b| b.len().cmp(&a.len()));
        let mut o = text.to_string();
        for k in keys {
            if let Some(v) = map[k].iter().next() {
                o = o.replace(k.as_str(), v);
            }
        }
        o
    }
}

pub fn text_quality(page: &str) -> i64 {
    let ws: Vec<&str> = page.split_whitespace().collect();
    if ws.is_empty() {
        return 100;
    }
    let good = ws
        .iter()
        .filter(|w| {
            let t: String = w.chars().filter(|c| c.is_alphanumeric()).collect();
            let n = t.chars().count();
            if n == 0 {
                return false;
            }
            if t.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
            let letters = t.chars().filter(|c| c.is_alphabetic()).count();
            let vowels = fold(&t).chars().filter(|c| "aeiouy".contains(*c)).count();
            let punct = w
                .chars()
                .filter(|c| !c.is_alphanumeric() && !",.;:()¿?¡!\"'«»-–/".contains(*c))
                .count();
            (n == 1 && "aeoyAEOY".contains(&t)) || (n >= 2 && n <= 22 && letters == n && vowels > 0 && punct == 0)
        })
        .count();
    (good * 100 / ws.len()) as i64
}

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export const WAYS = ["local", "google", "claude"];

export function targetLang(lang) {
  return ["de", "en", "pt"].includes(lang) ? lang : "en";
}

const ocrBase = () => window.__fjOcrBase || (/Windows|Android/i.test(navigator.userAgent) ? "http://fjocr.localhost/" : "fjocr://localhost/");
let OCRW = null;
async function ocr(canvas) {
  if (!OCRW) {
    const T = await import("tesseract.js");
    const b = ocrBase();
    OCRW = await T.createWorker(["spa", "eng"], 1, { workerPath: b + "worker.min.js", corePath: b, langPath: b, gzip: true, cacheMethod: "none", workerBlobURL: true });
  }
  const r = await OCRW.recognize(canvas);
  return (r && r.data && r.data.text) || "";
}

export async function pageText(page) {
  const tc = await page.getTextContent();
  let t = "";
  for (const it of tc.items) {
    if (typeof it.str !== "string") continue;
    t += it.str + (it.hasEOL ? "\n" : "");
  }
  t = t.replace(/[ \t]+\n/g, "\n").trim();
  if (t.replace(/\s/g, "").length >= 20) return { text: t, ocr: false };
  try {
    const vp = page.getViewport({ scale: 2 });
    const c = document.createElement("canvas");
    c.width = Math.ceil(vp.width);
    c.height = Math.ceil(vp.height);
    await page.render({ canvasContext: c.getContext("2d"), viewport: vp }).promise;
    const o = (await ocr(c)).trim();
    return { text: o || t, ocr: !!o };
  } catch (e) {
    return { text: t, ocr: false, noocr: true };
  }
}

let BT = null;
async function bergamot() {
  if (BT) return BT;
  const { TranslatorBacking, BatchTranslator } = await import("./vendor/bergamot-translator.js");

  class FjBacking extends TranslatorBacking {
    async loadModelRegistery() {
      const buf = await invoke("tr_model_file", { url: this.registryUrl, sha256: null });
      const registry = JSON.parse(new TextDecoder().decode(new Uint8Array(buf)));
      return Array.from(Object.entries(registry), ([key, files]) => ({ from: key.substring(0, 2), to: key.substring(2, 4), files }));
    }
    async fetch(url, checksum) {
      const buf = await invoke("tr_model_file", { url, sha256: checksum || null });
      return buf instanceof ArrayBuffer ? buf : new Uint8Array(buf).buffer;
    }
  }
  const opts = { workerUrl: "/bergamot/translator-worker.js", downloadTimeout: 0, workers: 1, batchSize: 8 };
  BT = new BatchTranslator(opts, new FjBacking(opts));
  return BT;
}

async function localWay(pages, from, to, progress) {
  const t = await bergamot();
  const out = [];
  for (let i = 0; i < pages.length; i++) {
    progress(i, pages.length);
    if (!pages[i].trim()) {
      out.push("");
      continue;
    }

    const paras = pages[i].split(/\n/);
    const res = await Promise.all(paras.map((p) => (p.trim() ? t.translate({ from, to, text: p, html: false }).then((r) => r.target.text) : Promise.resolve(""))));
    out.push(res.join("\n"));
  }
  return out;
}

export async function translatePages(path, pages, way, to, progress, before) {
  const from = "es";
  const kept = await invoke("tr_cached", { path, to, way }).catch(() => null);
  if (kept && Array.isArray(kept.pages) && kept.pages.length) return Object.assign(kept, { kept: true });

  if (way !== "local" && before && !(await before())) throw new Error("cancelled");
  if (way === "local") {
    const out = await localWay(pages, from, to, progress);
    return invoke("tr_keep", { path, to, way, pages: out });
  }
  let un = null;
  try {
    un = await listen("fj-tr", (e) => progress((e.payload || {}).done || 0, (e.payload || {}).all || 1));
  } catch (e) {}
  try {
    return await invoke("tr_run", { path, from, to, way, pages });
  } finally {
    if (un) un();
  }
}

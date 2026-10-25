(function () {
  "use strict";
  if (window.__fj) return;
  var CFG = window.__fjCfg || {};
  var FJ = (window.__fj = {
    v: 2,
    hdr: null,
    hdrT: 0,
    lastList: null,
    list: { collect: false, resps: [], reqSeen: 0, legReqSeen: 0, failed: "", legRespT: 0 },
    jobs: {},
    dl: {},
    watch: {},
    dialogs: [],
    back: false,
    takeover: false,
    seq: 0,
  });
  var SKIP_HDR = /^(content-length|host|cookie|p_identificador|p_archivo|connection|accept-encoding)$/;
  var now = function () { return Date.now(); };
  var lc = function (s) { return String(s || "").toLowerCase(); };
  var norm = function (s) { return String(s || "").replace(/\s+/g, " ").trim(); };

  function noteRequest(url, headers, body) {
    url = String(url || "");

    if (CFG.caseList && url.indexOf(CFG.caseList) >= 0 && typeof body === "string") FJ.lastList = { url: url, body: body, t: now() };
    if (url.indexOf(CFG.apiPart || "/api/") >= 0 && headers && headers.authorization) {
      var h = {};
      for (var k in headers) {
        var kl = lc(k);
        if (kl.charAt(0) === ":" || SKIP_HDR.test(kl)) continue;
        h[kl] = headers[k];
      }
      FJ.hdr = h;
      FJ.hdrT = now();
    }
    if (FJ.list.collect) {
      if (CFG.caseList && url.indexOf(CFG.caseList) >= 0) FJ.list.reqSeen = now();
      if (CFG.legajoList && url.indexOf(CFG.legajoList) >= 0) FJ.list.legReqSeen = now();
    }
  }
  function noteResponse(url, status, text, reqBody) {
    url = String(url || "");
    for (var w in FJ.watch) {
      if (url.indexOf(w) >= 0) FJ.watch[w].push({ url: url, status: status, text: text, req: reqBody || "", t: now() });
    }
    if (!FJ.list.collect) return;
    var isCase = CFG.caseList && url.indexOf(CFG.caseList) >= 0;
    var isLeg = CFG.legajoList && url.indexOf(CFG.legajoList) >= 0;
    if (!isCase && !isLeg) return;
    FJ.list.resps.push({ url: url, status: status, text: text, t: now(), legajo: !!isLeg });
    if (isLeg) FJ.list.legRespT = now();
  }
  function noteFailure(url, why) {
    url = String(url || "");
    if (FJ.list.collect && CFG.caseList && url.indexOf(CFG.caseList) >= 0) FJ.list.failed = why || "failed";
  }

  var XO = XMLHttpRequest.prototype.open,
    XS = XMLHttpRequest.prototype.send,
    XH = XMLHttpRequest.prototype.setRequestHeader;
  XMLHttpRequest.prototype.open = function (m, u) {
    this.__fjU = u;
    this.__fjH = {};
    return XO.apply(this, arguments);
  };
  XMLHttpRequest.prototype.setRequestHeader = function (k, v) {
    if (this.__fjH) this.__fjH[lc(k)] = v;
    return XH.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function (body) {
    var x = this;
    x.__fjB = typeof body === "string" ? body : "";
    try { noteRequest(x.__fjU, x.__fjH, x.__fjB); } catch (e) {}
    x.addEventListener("load", function () {
      try {
        var t = x.responseType === "" || x.responseType === "text" ? x.responseText : (x.responseType === "json" ? JSON.stringify(x.response) : "");
        noteResponse(x.__fjU, x.status, t, x.__fjB);
      } catch (e) {}
    });
    x.addEventListener("error", function () { noteFailure(x.__fjU, "net::ERR_FAILED"); });
    x.addEventListener("timeout", function () { noteFailure(x.__fjU, "timeout"); });
    return XS.apply(this, arguments);
  };
  var F0 = window.fetch;
  if (F0) {
    window.fetch = function (input, init) {
      var url = typeof input === "string" ? input : (input && input.url) || "";
      var h = {};
      try {
        var src = (init && init.headers) || (input && input.headers);
        if (src && typeof src.forEach === "function") src.forEach(function (v, k) { h[lc(k)] = v; });
        else if (src) for (var k in src) h[lc(k)] = src[k];
        if (!(init && init.__fj)) noteRequest(url, h, init && typeof init.body === "string" ? init.body : "");
      } catch (e) {}
      var p = F0.apply(this, arguments);
      if (!(init && init.__fj)) {
        p.then(function (r) {
          var watched = false;
          for (var w in FJ.watch) if (url.indexOf(w) >= 0) watched = true;
          if (watched || (FJ.list.collect && (url.indexOf(CFG.caseList) >= 0 || url.indexOf(CFG.legajoList) >= 0))) {
            var rb = init && typeof init.body === "string" ? init.body : "";
            r.clone().text().then(function (t) { noteResponse(url, r.status, t, rb); });
          }
        }, function (e) { noteFailure(url, String(e)); });
      }
      return p;
    };
  }

  FJ.userT = 0;
  FJ.userX = null;
  FJ.userY = null;
  ["mousemove", "mousedown", "keydown", "wheel", "touchstart"].forEach(function (ev) {
    try {
      document.addEventListener(ev, function (e) {
        if (!e.isTrusted) return;
        if (ev === "mousemove") {
          var x = e.screenX, y = e.screenY, moved = FJ.userX !== null && (Math.abs(x - FJ.userX) >= 3 || Math.abs(y - FJ.userY) >= 3);
          FJ.userX = x;
          FJ.userY = y;
          if (!moved) return;
        }
        FJ.userT = now();
      }, { capture: true, passive: true });
    } catch (e) {}
  });
  FJ.userIdle = function () { return FJ.userT ? Math.round((now() - FJ.userT) / 1000) : -1; };

  try {
    window.alert = function (m) { FJ.dialogs.push({ type: "alert", text: String(m).slice(0, 300) }); };
    window.confirm = function (m) { FJ.dialogs.push({ type: "confirm", text: String(m).slice(0, 300) }); return true; };
  } catch (e) {}

  function visible(el) {
    if (!el || !el.isConnected) return false;
    var st = window.getComputedStyle(el);
    if (st.visibility === "hidden" || st.display === "none") return false;
    var r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }
  function splitTop(sel) {
    var out = [], cur = "", depth = 0, q = "";
    for (var i = 0; i < sel.length; i++) {
      var c = sel.charAt(i);
      if (q) { cur += c; if (c === q) q = ""; continue; }
      if (c === "'" || c === '"') { q = c; cur += c; continue; }
      if (c === "(" || c === "[") depth++;
      if (c === ")" || c === "]") depth--;
      if (c === "," && depth === 0) { out.push(cur.trim()); cur = ""; continue; }
      cur += c;
    }
    if (cur.trim()) out.push(cur.trim());
    return out;
  }
  function one(part) {
    var texts = [], vis = false;
    var css = part.replace(/:has-text\((['"])(.*?)\1\)/g, function (_, q, t) { texts.push(lc(norm(t))); return ""; });
    css = css.replace(/:visible/g, function () { vis = true; return ""; }).trim();
    var els;
    try { els = Array.prototype.slice.call(document.querySelectorAll(css || "*")); } catch (e) { return []; }
    return els.filter(function (el) {
      if (vis && !visible(el)) return false;
      if (texts.length) {
        var t = lc(norm(el.innerText || el.textContent));
        for (var i = 0; i < texts.length; i++) if (t.indexOf(texts[i]) < 0) return false;
      }
      return true;
    });
  }
  function all(sel) {
    var parts = splitTop(String(sel || "")), seen = [], out = [];
    parts.forEach(function (p) { one(p).forEach(function (el) { if (seen.indexOf(el) < 0) { seen.push(el); out.push(el); } }); });
    if (parts.length > 1) out.sort(function (a, b) { return a === b ? 0 : (a.compareDocumentPosition(b) & 2 ? 1 : -1); });
    return out;
  }

  function byText(text, exact) {
    var want = norm(text), res = [];
    var els = document.querySelectorAll("body *");
    for (var i = 0; i < els.length; i++) {
      var el = els[i];
      if (/^(SCRIPT|STYLE|NOSCRIPT)$/.test(el.tagName)) continue;
      var t = norm(el.innerText !== undefined ? el.innerText : el.textContent);
      var ok = exact ? t === want : lc(t).indexOf(lc(want)) >= 0;
      if (!ok) continue;
      var child = false;
      for (var j = 0; j < el.children.length; j++) {
        var ct = norm(el.children[j].innerText || el.children[j].textContent);
        if (exact ? ct === want : lc(ct).indexOf(lc(want)) >= 0) { child = true; break; }
      }
      if (!child) res.push(el);
    }
    return res;
  }
  function linkByName(name) {
    return Array.prototype.slice.call(document.querySelectorAll("a,[role=link]")).filter(function (a) {
      var n = norm(a.getAttribute("aria-label") || a.innerText || a.textContent || a.getAttribute("title"));
      return n === name;
    });
  }
  FJ.$$ = all;
  FJ.count = function (sel, vis) { return all(sel).filter(function (e) { return !vis || visible(e); }).length; };
  FJ.anyVisible = function (sel) { return all(sel).some(visible); };

  FJ.visibleExact = function (sel, t) {
    t = String(t || "").toLowerCase();
    return all(sel).some(function (e) { return visible(e) && norm(e.innerText || e.textContent).toLowerCase() === t; });
  };
  FJ.textCount = function (t) { return byText(t, true).length; };
  FJ.text = function (sel) { var e = all(sel)[0]; return e ? String(e.innerText || e.textContent || "") : null; };
  function doClick(el) {
    if (!el) return false;
    try { el.scrollIntoView({ block: "center" }); } catch (e) {}
    var r = el.getBoundingClientRect(), x = r.left + r.width / 2, y = r.top + r.height / 2;
    var o = { bubbles: true, cancelable: true, view: window, clientX: x, clientY: y };
    try { el.dispatchEvent(new PointerEvent("pointerdown", o)); } catch (e) {}
    el.dispatchEvent(new MouseEvent("mousedown", o));
    try { el.dispatchEvent(new PointerEvent("pointerup", o)); } catch (e) {}
    el.dispatchEvent(new MouseEvent("mouseup", o));
    el.click();
    return true;
  }
  FJ.click = function (sel, idx, vis) {
    var els = all(sel).filter(function (e) { return !vis || visible(e); });
    return doClick(els[idx || 0]);
  };
  FJ.clickText = function (t, exact, vis) {
    var els = byText(t, exact !== false).filter(function (e) { return !vis || visible(e); });
    return doClick(els[0]);
  };
  FJ.clickLink = function (name) { return doClick(linkByName(name)[0]); };
  FJ.focus = function (sel) { var e = all(sel)[0]; if (!e) return false; try { e.focus(); } catch (x) {} return true; };
  FJ.check = function (sel) { var e = all(sel)[0]; if (!e) return false; if (!e.checked) doClick(e); return true; };
  FJ.enabled = function (sel) { return all(sel).filter(function (e) { return visible(e) && !e.disabled && !/p-disabled/.test(e.className); }).length; };
  FJ.mouse = function () {

    var t = document.documentElement;
    if (!t) return false;
    var d = (Math.floor(now() / 1000) % 2) ? 1 : -1;
    try { t.dispatchEvent(new MouseEvent("mousemove", { bubbles: true, clientX: 300, clientY: 300, movementX: d, movementY: d })); } catch (e) {}
    try { t.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key: "Shift", code: "ShiftLeft", keyCode: 16, which: 16 })); } catch (e) {}
    try { t.dispatchEvent(new KeyboardEvent("keyup", { bubbles: true, key: "Shift", code: "ShiftLeft", keyCode: 16, which: 16 })); } catch (e) {}
    return true;
  };

  FJ.pageList = function () {
    var p = all(".p-paginator").filter(visible)[0];
    if (!p) return "no_paginator";
    var ok = function (b) { return b && !b.disabled && !/p-disabled/.test(b.className); };
    var n = p.querySelector(".p-paginator-next"), f = p.querySelector(".p-paginator-first");
    if (ok(n)) { doClick(n); return "next"; }
    if (ok(f)) { doClick(f); return "first"; }
    return "one_page";
  };

  function cardText(a) {
    var n = a;
    for (var i = 0; i < 10 && n; i++) {
      n = n.parentElement;
      if (n && /Fecha entrada/.test(n.innerText) && /Datos del/.test(n.innerText)) return n.innerText;
    }
    return a.innerText;
  }
  FJ.cards = function (sel) {
    return all(sel).map(function (a) { return { card: cardText(a), link: String(a.innerText || "") }; });
  };
  FJ.firstVisible = function (sel) { return all(sel).some(visible); };

  FJ.dialog = function () {
    var d = all(".p-dialog").filter(visible)[0];
    if (!d) return null;
    return { text: norm(d.innerText).slice(0, 400) };
  };
  FJ.dialogButton = function (label) {
    var d = all(".p-dialog").filter(visible)[0];
    if (!d) return false;
    var bs = Array.prototype.slice.call(d.querySelectorAll("button"));
    var b = bs.filter(function (x) { return lc(norm(x.innerText)).indexOf(lc(label)) >= 0; })[0];
    if (!b && label === "*last") b = bs[bs.length - 1];
    if (!b && label === "*close") b = d.querySelector(".p-dialog-header-close");
    return doClick(b);
  };
  FJ.watchStart = function (parts) { FJ.watch = {}; (parts || []).forEach(function (p) { FJ.watch[p] = []; }); return true; };
  FJ.watchTake = function (part) { var a = FJ.watch[part] || []; if (FJ.watch[part]) FJ.watch[part] = []; return a; };
  FJ.watchStop = function () { FJ.watch = {}; return true; };
  FJ.takeDialogs = function () { var d = FJ.dialogs; FJ.dialogs = []; return d; };

  var JWT = /^eyJ[\w-]+\.[\w-]+\.[\w-]+$/;
  function jwtExp(t) {
    try {
      var b = String(t).split(" ").pop().split(".")[1].replace(/-/g, "+").replace(/_/g, "/");
      var p = JSON.parse(atob(b + "===".slice((b.length + 3) % 4)));

      return { exp: p.exp || 0, scope: !!p.scope, iat: p.iat || 0, auth: p.auth_time || 0 };
    } catch (e) { return { exp: 0, scope: false, iat: 0, auth: 0 }; }
  }
  function findToken() {
    var found = [];
    function scan(o, depth) {
      if (o === null || o === undefined || depth > 6) return;
      if (typeof o === "string") {
        if (JWT.test(o)) { found.push(o); return; }
        if (o.length > 20 && (o[0] === "{" || o[0] === "[")) { try { scan(JSON.parse(o), depth + 1); } catch (e) {} }
        return;
      }
      if (typeof o === "object") for (var k in o) { if (/id_?token|refresh/i.test(k)) continue; scan(o[k], depth + 1); }
    }
    [window.sessionStorage, window.localStorage].forEach(function (st) {
      try {
        for (var i = 0; i < st.length; i++) { var k = st.key(i); if (/id_?token/i.test(k)) continue; scan(st.getItem(k), 0); }
      } catch (e) {}
    });
    var best = null, bestExp = 0, bestScore = -1;
    found.forEach(function (t) {
      var j = jwtExp(t);
      if (!j.exp) return;
      var score = j.scope ? 1 : 0;
      if (score > bestScore || (score === bestScore && j.exp > bestExp)) { best = t; bestExp = j.exp; bestScore = score; }
    });
    if (!best) return null;
    var bj = jwtExp(best);
    return { token: best, exp: bestExp, iat: bj.iat, auth: bj.auth };
  }

  FJ.updateToken = function (force, renewBefore) {
    if (!FJ.hdr) return 0;
    var cur = jwtExp(FJ.hdr.authorization || "").exp;
    if (!force && cur - now() / 1000 > (renewBefore || 120)) return 0;
    var info = findToken();
    if (!info) return 0;
    if ("Bearer " + info.token === FJ.hdr.authorization || info.exp < cur) return 0;
    FJ.hdr.authorization = "Bearer " + info.token;
    return info.exp;
  };
  FJ.info = function () {
    var path = (location.pathname || "").slice(0, 160);
    var t = findToken();
    return {
      host: location.hostname,
      path: path,
      ready: document.readyState,
      salir: byText(CFG.loggedInText || "Salir", true).length > 0,
      tokenExp: t ? t.exp : 0,
      hdrExp: FJ.hdr ? jwtExp(FJ.hdr.authorization || "").exp : 0,
      authTime: t ? t.auth : 0,
      tokenIat: t ? t.iat : 0,
      hasHdr: !!FJ.hdr,
      user: !!userNet(),
      dialogs: FJ.takeDialogs(),
      back: FJ.back,
    };
  };

  FJ.diag = function () {
    var t = norm(document.body ? document.body.innerText : "");
    var m = /(mantenimiento|fuera de servicio|no (est[aá] )?disponible|intermiten|sesi[oó]n (ha )?(expir|finaliz|caduc|cerr|termin)|vuelva a intentar|problemas de conexi)/i.exec(t);
    var d = all(".p-dialog, .p-toast-message, .p-message, [role=alert]").filter(visible)
      .map(function (x) { return norm(x.innerText).slice(0, 200); }).filter(Boolean).slice(0, 3);
    return {
      page: location.hostname + (location.pathname || "").slice(0, 120),
      title: String(document.title || "").slice(0, 80),
      notice: d.join(" | ").slice(0, 300),
      hit: m ? t.slice(Math.max(0, m.index - 60), m.index + 140) : "",
    };
  };

  FJ.down = function () {
    var path = String(location.pathname || "");
    var t = norm(document.body ? document.body.innerText : "");
    var urlHit = /notavailable|no-?disponible|mantenimiento|maintenance|bloque|blocked|outage/i.test(path);
    var rx = /(no se encuentra disponible|no est[aá] disponible|fuera de servicio|en mantenimiento|mantenimiento programado|servicio no disponible|service unavailable|demasiados intentos|ha sido bloquead|acceso (bloqueado|denegado)|access denied|temporalmente (fuera|no disponible|suspendid)|suspendid[oa] temporalmente)/i;
    var m = rx.exec(t);
    if (!urlHit && !m) return null;

    var msg = "";
    var els = all("h1, h2, h3, h4, .title, .titulo, .mensaje, .message, .alert, [role=alert], p, span, div").filter(visible);
    for (var i = 0; i < els.length && !msg; i++) {
      var x = norm(els[i].innerText || "");
      if (x && x.length <= 300 && (m ? x.toLowerCase().indexOf(m[1].toLowerCase()) >= 0 : /^h[1-4]$/i.test(els[i].tagName))) msg = x;
    }
    if (!msg && m) msg = t.slice(Math.max(0, m.index - 40), m.index + 160);
    if (!msg) msg = String(document.title || "").slice(0, 160);
    return { path: (location.hostname + path).slice(0, 160), msg: msg.slice(0, 300), known: !!m || urlHit };
  };

  FJ.status = function () {
    var t = findToken();
    return {
      salir: byText(CFG.loggedInText || "Salir", true).length > 0,
      tokenExp: t ? t.exp : 0,
      hasHdr: !!FJ.hdr,
      ingresar: FJ.visibleExact("button, a", "ingresar"),
    };
  };
  function userNet() {
    try { return JSON.parse(FJ.hdr["xyz-bitacoratransaccionbase"]).usuarioRed || ""; } catch (e) { return ""; }
  }

  function apiHeaders(extra, dropP) {
    var h = {};
    var src = FJ.hdr || {};
    for (var k in src) { if (dropP && /^p_/.test(k)) continue; h[k] = src[k]; }
    h["content-type"] = "application/json";
    if (!h.accept) h.accept = "application/json, text/plain, */*";
    for (var e in extra || {}) h[e] = extra[e];
    return h;
  }
  var FORBID = /^(host|content-length|cookie|user-agent|referer|origin|accept-encoding|connection|keep-alive|te|trailer|transfer-encoding|upgrade|via|expect|date|dnt)$|^(sec-|proxy-)/;
  function cleanHeaders(h) { var o = {}; for (var k in h) if (!FORBID.test(lc(k))) o[k] = h[k]; return o; }
  var EVID = ["server", "via", "x-powered-by", "x-aspnet-version", "x-azure-ref", "x-ms-request-id", "x-request-id", "x-cache", "x-served-by", "content-type", "content-disposition"];

  FJ.api = function (a) {
    var id = "j" + ++FJ.seq;
    var job = (FJ.jobs[id] = { end: false });
    var body = a.body;
    if (body && body.indexOf("__FJ_USER__") >= 0) body = body.split('"__FJ_USER__"').join(JSON.stringify(userNet()));
    var ac = new AbortController(), t0 = now();
    var to = setTimeout(function () { job.timeout = true; ac.abort(); }, a.timeout || 45000);
    F0.call(window, a.url, { method: a.method || "POST", headers: cleanHeaders(apiHeaders(a.headers, a.dropP)), body: a.method === "GET" ? undefined : body, signal: ac.signal, cache: "no-store", __fj: true })
      .then(function (r) {
        job.status = r.status;
        job.headers = {};
        EVID.forEach(function (k) { var v = r.headers.get(k); if (v) job.headers[k] = v; });
        return r.text();
      })
      .then(function (t) { job.text = t; }, function (e) { job.err = job.timeout ? "timeout" : (e && e.name ? e.name + ": " : "") + String((e && e.message) || e); })
      .then(function () { clearTimeout(to); job.dur = (now() - t0) / 1000; job.end = true; });
    return id;
  };
  FJ.job = function (id) {
    var j = FJ.jobs[id];
    if (!j) return { gone: true };
    if (!j.end) return { end: false };
    delete FJ.jobs[id];
    return j;
  };
  FJ.hasUser = function () { return !!userNet(); };

  FJ.dlStart = function (a) {
    var id = "d" + ++FJ.seq;
    var st = (FJ.dl[id] = { chunks: [], end: false, err: "", why: "", status: 0, ctype: "", cdisp: "", bytes: 0, head: null, first: null, gap: 0, t0: performance.now(), last: performance.now(), headers: {} });
    var ac = new AbortController();
    st.ac = ac;
    var wd = setInterval(function () {
      var n = performance.now();
      if (st.head === null) { if (n - st.t0 > a.answer * 1000) { st.why = "answer"; ac.abort(); } }
      else if (n - st.last > a.silence * 1000) { st.why = "silence"; ac.abort(); }
    }, 500);
    (async function () {
      try {

        var opt = a.method === "GET"
          ? { method: "GET", signal: ac.signal, cache: "no-store", credentials: "omit", __fj: true }
          : { method: "POST", headers: cleanHeaders(apiHeaders(a.headers, false)), body: a.body, signal: ac.signal, cache: "no-store", __fj: true };
        var r = await F0.call(window, a.url, opt);
        st.head = performance.now(); st.last = st.head; st.status = r.status; st.url = r.url || "";
        st.ctype = r.headers.get("content-type") || ""; st.cdisp = r.headers.get("content-disposition") || "";
        EVID.forEach(function (k) { var v = r.headers.get(k); if (v) st.headers[k] = v; });
        var rd = r.body.getReader();
        for (;;) {
          var x = await rd.read(); var t = performance.now();
          if (x.done) break;
          if (st.first === null) st.first = t; else st.gap = Math.max(st.gap, t - st.last);
          st.last = t; st.bytes += x.value.length; st.chunks.push(x.value);
        }
      } catch (e) {
        st.err = st.why || ((e && e.name ? e.name + ": " : "") + String((e && e.message) || e));
      } finally { clearInterval(wd); st.end = true; }
    })();
    return id;
  };
  function b64(parts) {
    var n = 0; parts.forEach(function (p) { n += p.length; });
    var all = new Uint8Array(n), o = 0;
    parts.forEach(function (p) { all.set(p, o); o += p.length; });
    var s = "";
    for (var i = 0; i < all.length; i += 0x8000) s += String.fromCharCode.apply(null, all.subarray(i, i + 0x8000));
    return btoa(s);
  }
  FJ.dlTake = function (id, max) {
    var st = FJ.dl[id];
    if (!st) return { gone: true };
    var take = [], n = 0;
    while (st.chunks.length && n < (max || 2097152)) { var c = st.chunks.shift(); take.push(c); n += c.length; }
    var done = st.end && !st.chunks.length;
    var out = { b64: take.length ? b64(take) : "", end: done, err: st.err, status: st.status, ctype: st.ctype, cdisp: st.cdisp, bytes: st.bytes, headers: st.headers, url: st.url || "",
      head: st.head === null ? null : (st.head - st.t0) / 1000, first: st.first === null ? null : (st.first - st.t0) / 1000,
      gap: st.gap / 1000, total: (performance.now() - st.t0) / 1000 };
    if (done) delete FJ.dl[id];
    return out;
  };
  FJ.dlAbort = function (id) { var st = FJ.dl[id]; if (st) { try { st.ac.abort(); } catch (e) {} delete FJ.dl[id]; } return true; };

  FJ.bar = function (text, backLabel) {
    var b = document.getElementById("__fjbar");
    if (!text) { if (b) b.remove(); return true; }
    if (!b) {
      b = document.createElement("div");
      b.id = "__fjbar";
      b.style.cssText = "position:fixed;left:0;right:0;bottom:0;z-index:2147483647;background:#123c3f;color:#fff;font:13px/1.3 system-ui,sans-serif;padding:6px 10px;display:flex;gap:8px;align-items:center";
      var s = document.createElement("span"); s.id = "__fjbar_t"; s.style.flex = "1";
      var k = document.createElement("button"); k.id = "__fjbar_b";
      k.style.cssText = "background:#fff;color:#123c3f;border:0;border-radius:4px;padding:4px 8px;font:inherit";
      k.onclick = function () { FJ.back = true; };
      b.appendChild(s); b.appendChild(k);
      (document.body || document.documentElement).appendChild(b);
    }
    document.getElementById("__fjbar_t").textContent = text;
    document.getElementById("__fjbar_b").textContent = backLabel || "FetchJ";
    return true;
  };

  FJ.lock = function (on, mode, banner, dlg, ok, cancel) {
    try {
      if (on) sessionStorage.setItem("__fjlock", JSON.stringify({ mode: mode, banner: banner, dlg: dlg, ok: ok, cancel: cancel }));
      else sessionStorage.removeItem("__fjlock");
    } catch (e) {}
    var o = document.getElementById("__fjlock");
    if (!on) { if (o) o.remove(); var d0 = document.getElementById("__fjlock_d"); if (d0) d0.remove(); return true; }
    if (!o) {
      o = document.createElement("div");
      o.id = "__fjlock";
      o.style.cssText = "position:fixed;inset:0;z-index:2147483646;background:rgba(10,20,22,.14);cursor:not-allowed";
      var bn = document.createElement("div"); bn.id = "__fjlock_bn";
      bn.style.cssText = "position:absolute;top:0;left:0;right:0;background:#123c3f;color:#fff;font:13px/1.35 system-ui,sans-serif;padding:7px 12px;text-align:center";
      o.appendChild(bn);
      o.onclick = function () { FJ.__dlg(); };
      (document.body || document.documentElement).appendChild(o);
    }
    o.dataset.mode = mode || "wait";
    o.dataset.dlg = dlg || ""; o.dataset.ok = ok || "OK"; o.dataset.cancel = cancel || "";
    document.getElementById("__fjlock_bn").textContent = banner || "";
    return true;
  };
  FJ.__dlg = function () {
    var o = document.getElementById("__fjlock"); if (!o || document.getElementById("__fjlock_d")) return;
    var online = o.dataset.mode === "online";
    var d = document.createElement("div"); d.id = "__fjlock_d";
    d.style.cssText = "position:fixed;inset:0;z-index:2147483647;background:rgba(10,20,22,.55);display:flex;align-items:center;justify-content:center;font:15px/1.45 system-ui,sans-serif";
    var c = document.createElement("div");
    c.style.cssText = "max-width:430px;margin:16px;padding:22px 24px;background:#fff;color:#123c3f;border-radius:12px;box-shadow:0 10px 40px rgba(0,0,0,.4);text-align:center";
    var s = document.createElement("div"); s.style.cssText = "margin-bottom:18px;white-space:pre-line"; s.textContent = o.dataset.dlg || "";
    var row = document.createElement("div"); row.style.cssText = "display:flex;gap:10px;justify-content:center";
    if (online && o.dataset.cancel) {
      var kc = document.createElement("button");
      kc.style.cssText = "background:#eef1f1;color:#123c3f;border:0;border-radius:6px;padding:9px 16px;font:inherit;cursor:pointer";
      kc.textContent = o.dataset.cancel; kc.onclick = function () { d.remove(); };
      row.appendChild(kc);
    }
    var kk = document.createElement("button");
    kk.style.cssText = "background:#123c3f;color:#fff;border:0;border-radius:6px;padding:9px 16px;font:inherit;cursor:pointer";
    kk.textContent = o.dataset.ok || "OK";
    kk.onclick = function () { d.remove(); if (online) { FJ.takeover = true; FJ.lock(false); } };
    row.appendChild(kk);
    c.appendChild(s); c.appendChild(row); d.appendChild(c);
    (document.body || document.documentElement).appendChild(d);
  };
  FJ.takeN = function () { var v = !!FJ.takeover; FJ.takeover = false; return v; };

  try {
    var _l = JSON.parse(sessionStorage.getItem("__fjlock") || "null");
    if (_l) {
      var _show = function () { FJ.lock(true, _l.mode, _l.banner, _l.dlg, _l.ok, _l.cancel); };
      if (document.body) _show(); else document.addEventListener("DOMContentLoaded", _show);
    }
  } catch (e) {}
})();

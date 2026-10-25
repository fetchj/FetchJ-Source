(function () {
  "use strict";
  if (window.__fjl) return;
  var norm = function (s) { return String(s || "").replace(/\s+/g, " ").trim(); };
  var lc = function (s) { return String(s || "").toLowerCase(); };
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
    var out = [];
    splitTop(String(sel || "")).forEach(function (p) { one(p).forEach(function (el) { if (out.indexOf(el) < 0) out.push(el); }); });
    return out;
  }
  function first(sel) { return all(sel).filter(visible)[0] || null; }
  var L = (window.__fjl = { v: 1 });
  L.anyVisible = function (sel) { return !!first(sel); };
  L.count = function (sel) { return all(sel).length; };

  L.rect = function (sel) {
    var e = first(sel);
    if (!e) return null;
    try { e.scrollIntoView({ block: "center" }); } catch (x) {}
    var r = e.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2, dpr: window.devicePixelRatio || 1 };
  };

  L.click = function (sel) {
    var e = first(sel);
    if (!e) return false;
    try { e.scrollIntoView({ block: "center" }); } catch (x) {}
    e.click();
    return true;
  };

  var armed = null, hov = false;
  document.addEventListener("mousemove", function (ev) { if (armed && armed.contains(ev.target)) hov = true; }, true);
  L.arm = function (sel) { armed = first(sel); hov = false; return !!armed; };
  L.hov = function () { var h = hov; hov = false; return h; };
  L.check = function (sel) {
    var e = all(sel)[0];
    if (!e) return false;
    if (!e.checked) e.click();
    return true;
  };
  L.checked = function (sel) { var e = all(sel)[0]; return !!(e && e.checked); };
  L.focus = function (sel) { var e = first(sel); if (!e) return false; try { e.focus(); } catch (x) {} return true; };

  L.clear = function (sel) { var e = first(sel); if (!e) return false; try { e.value = ""; } catch (x) {} return true; };
  L.len = function (sel) { var e = first(sel); return e && typeof e.value === "string" ? e.value.length : -1; };
  L.fill = function (sel, v) {
    var e = first(sel);
    if (!e) return false;
    try {
      var set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
      set.call(e, String(v));
      e.dispatchEvent(new Event("input", { bubbles: true }));
      e.dispatchEvent(new Event("change", { bubbles: true }));
    } catch (x) { return false; }
    return true;
  };

  L.filled = function (u, p) {
    return [u, p].map(function (s) {
      var e = document.querySelector(s);
      if (!e) return false;
      var a = false;
      try { a = e.matches(":-webkit-autofill"); } catch (x) {}
      try { a = a || e.matches(":autofill"); } catch (x) {}
      return a || e.value.length > 0;
    });
  };

  L.error = function (sel) {
    var e = first(sel);
    if (!e) return null;
    var t = norm(e.innerText || e.textContent);
    return t ? t.slice(0, 160) : null;
  };
})();

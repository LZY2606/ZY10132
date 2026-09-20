"use strict";
// spectra-lab front end.
// All user edits are stored in *raw dataset units*.  The display unit only
// controls projection; interval endpoints and peak widths are converted with
// the same physical-range rules as the backend (src/units.rs).

const HC_EV_NM = 1239.8419843929196;
const CM_IN_NM = 10000000;

const state = {
  datasetId: null,
  inputHash: null,
  rawUnit: "nm",
  x: [], y: [],
  tool: "pan",
  dispUnit: "nm",
  windows: [],       // [lo,hi] in raw units
  excluded: [],      // raw x
  peaks: [],         // {kind, center0, width0, amp0, fixCenter}
  lastFit: null,
  schemes: [],
  view: null,        // {x0,x1} in display units, null = auto
  drag: null,
};

const $ = (id) => document.getElementById(id);

// ---------------- unit transforms (mirror of src/units.rs) ----------------
function convertScalar(x, from, to) {
  if (from === to || !isFinite(x) || x === 0) return x;
  let wl;
  if (from === "nm") wl = x;
  else if (from === "cm^-1") wl = CM_IN_NM / x;
  else wl = HC_EV_NM / x;
  if (to === "nm") return wl;
  if (to === "cm^-1") return CM_IN_NM / wl;
  return HC_EV_NM / wl;
}
function convArr(xs, to) {
  if (to === state.rawUnit) return xs.slice();
  return xs.map((x) => convertScalar(x, state.rawUnit, to));
}
function toRaw(xDisp) { return convertScalar(xDisp, state.dispUnit, state.rawUnit); }
function toDisp(xRaw) { return convertScalar(xRaw, state.rawUnit, state.dispUnit); }
function convertPeakDisp(c, w, fromUnit, toUnit) {
  if (fromUnit === toUnit) return [c, w];
  const cc = convertScalar(c, fromUnit, toUnit);
  const lo = convertScalar(c - w / 2, fromUnit, toUnit);
  const hi = convertScalar(c + w / 2, fromUnit, toUnit);
  return [cc, Math.abs(hi - lo)];
}

// ---------------- API ----------------
async function api(path, payload) {
  const res = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  let v = null;
  try { v = await res.json(); } catch (_) {}
  if (!res.ok) throw new Error((v && v.error) || `HTTP ${res.status}`);
  return v;
}
async function apiGet(path) {
  const res = await fetch(path);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

// ---------------- canvas chart ----------------
const canvas = $("chart");
const ctx = canvas.getContext("2d");
const M = { l: 54, r: 16, t: 14, b: 30 };
function cssScale() {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth, h = 460;
  if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
  }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { w, h };
}
function plotGeom() {
  const { w, h } = cssScale();
  const residH = $("show-resid").checked && state.lastFit ? h * 0.22 : 0;
  const topH = h - M.t - M.b - residH - (residH ? 8 : 0);
  return { w, h, top: M.t, topH, resid: M.t + topH + 8, residH };
}
function xRange() {
  const xs = convArr(state.x, state.dispUnit);
  if (state.view) return state.view;
  let lo = Infinity, hi = -Infinity;
  for (const x of xs) { if (x < lo) lo = x; if (x > hi) hi = x; }
  return { x0: lo, x1: hi };
}
function yRangeOver(ysIn, ymap) {
  let lo = Infinity, hi = -Infinity;
  for (let i = 0; i < state.x.length; i++) {
    if (isExcluded(i)) continue;
    const y = ysIn ? ysIn[i] : state.y[i];
    if (y === null || !isFinite(y)) continue;
    if (y < lo) lo = y; if (y > hi) hi = y;
  }
  if (!isFinite(lo)) { lo = 0; hi = 1; }
  const pad = (hi - lo) * 0.06 || 1;
  return { y0: lo - pad, y1: hi + pad };
}
function isExcluded(i) {
  const xv = state.x[i];
  return state.excluded.some((ex) => Math.abs(xv - ex) <= 1e-9 * Math.max(1, Math.abs(ex)));
}
function makeScales() {
  const g = plotGeom();
  const xr = xRange();
  const topYs = [];
  if ($("show-raw").checked) state.y.forEach((v, i) => { if (!isExcluded(i)) topYs.push(v); });
  const fit = state.lastFit;
  if (fit) {
    if ($("show-baseline").checked) fit.baseline.forEach((v) => topYs.push(v));
    if ($("show-corrected").checked) fit.corrected.forEach((v, i) => { if (!isExcluded(i)) topYs.push(v); });
    if ($("show-smoothed").checked && fit.smoothed) fit.smoothed.forEach((v, i) => { if (!isExcluded(i) && isFinite(v)) topYs.push(v); });
    if ($("show-fit").checked) fit.report.fitted.forEach((v, i) => { if (!isExcluded(i)) topYs.push(v + fit.baseline[i]); });
  }
  let lo = Math.min(...topYs), hi = Math.max(...topYs);
  if (!isFinite(lo)) { lo = -1; hi = 1; }
  const pad = (hi - lo) * 0.06 || 1; lo -= pad; hi += pad;
  const X = (x) => M.l + ((x - xr.x0) / (xr.x1 - xr.x0)) * (g.w - M.l - M.r);
  const Y = (y) => g.top + g.topH - ((y - lo) / (hi - lo)) * g.topH;
  const RY = (y) => g.resid + g.residH - ((y + 1) / 2) * g.residH; // placeholder
  return { g, xr, lo, hi, X, Y, RY };
}

function draw() {
  if (!state.x.length) return;
  const { g, xr, lo, hi, X, Y } = makeScales();
  ctx.clearRect(0, 0, g.w, g.h);
  // grid + axes
  ctx.strokeStyle = "#22303d"; ctx.fillStyle = "#8ba0b2"; ctx.lineWidth = 1;
  ctx.font = "11px ui-monospace";
  const nTicks = 8;
  for (let t = 0; t <= nTicks; t++) {
    const xv = xr.x0 + (xr.x1 - xr.x0) * t / nTicks;
    const px = X(xv);
    ctx.beginPath(); ctx.moveTo(px, g.top); ctx.lineTo(px, g.top + g.topH); ctx.stroke();
    ctx.fillText(xv.toFixed(xv < 100 ? 2 : 1), px - 22, g.top + g.topH + 16);
  }
  for (let t = 0; t <= 5; t++) {
    const yv = lo + (hi - lo) * t / 5;
    const py = Y(yv);
    ctx.beginPath(); ctx.moveTo(M.l, py); ctx.lineTo(g.w - M.r, py); ctx.stroke();
    ctx.fillText(yv.toFixed(2), 6, py + 3);
  }
  // baseline windows (shading)
  for (const [a, b] of state.windows) {
    const xa = X(toDisp(a)), xb = X(toDisp(b));
    ctx.fillStyle = "rgba(74,168,255,0.10)";
    ctx.fillRect(Math.min(xa, xb), g.top, Math.abs(xb - xa), g.topH);
  }
  const xs = convArr(state.x, state.dispUnit);
  const fit = state.lastFit;

  function polyline(arr, color, width, rawYTransform, skipMask) {
    ctx.strokeStyle = color; ctx.lineWidth = width; ctx.beginPath();
    let started = false;
    for (let i = 0; i < xs.length; i++) {
      if (skipMask && isExcluded(i)) { started = false; continue; }
      let yv = rawYTransform(i);
      if (yv === null || !isFinite(yv)) { started = false; continue; }
      const px = X(xs[i]), py = Y(yv);
      if (!started) { ctx.moveTo(px, py); started = true; } else ctx.lineTo(px, py);
    }
    ctx.stroke();
  }
  if ($("show-corrected").checked && fit)
    polyline(null, "#94d082", 1.2, (i) => isExcluded(i) ? null : fit.corrected[i], true);
  if ($("show-smoothed").checked && fit && fit.smoothed)
    polyline(null, "#ffb454", 1.3, (i) => (isExcluded(i) ? null : fit.smoothed[i]), true);
  if ($("show-baseline").checked && fit)
    polyline(null, "#4aa8ff", 1.4, (i) => fit.baseline[i], false);
  if ($("show-fit").checked && fit)
    polyline(null, "#c792ea", 1.8, (i) => isExcluded(i) ? null : fit.report.fitted[i] + fit.baseline[i], true);

  // raw points
  if ($("show-raw").checked) {
    ctx.fillStyle = "#5b7186";
    for (let i = 0; i < xs.length; i++) {
      if (isExcluded(i)) {
        ctx.fillStyle = "#ff6b6b";
        ctx.fillRect(X(xs[i]) - 2, Y(state.y[i]) - 2, 4, 4);
        ctx.fillStyle = "#5b7186";
      } else {
        ctx.fillRect(X(xs[i]) - 1, Y(state.y[i]) - 1, 2, 2);
      }
    }
  }

  // peak markers
  for (const pk of state.peaks) {
    const [cc, ww] = convertPeakDisp(pk.center0, pk.width0, state.rawUnit, state.dispUnit);
    const px = X(cc);
    const col = pk.kind === "gauss" ? "#4aa8ff" : pk.kind === "lorentz" ? "#c792ea" : "#5bd391";
    ctx.strokeStyle = col; ctx.fillStyle = col; ctx.lineWidth = 1.4;
    ctx.beginPath(); ctx.moveTo(px, g.top + 6); ctx.lineTo(px, g.top + g.topH - 4); ctx.stroke();
    const wpx = Math.abs(X(cc + ww / 2) - X(cc - ww / 2));
    ctx.strokeRect(px - wpx / 2, g.top + 10, wpx, g.topH - 18);
    ctx.font = "10px ui-monospace";
    ctx.fillText((pk.fixCenter ? "🔒" : "") + pk.kind[0].toUpperCase(), px + 3, g.top + 14);
  }

  // in-progress drag rectangle
  if (state.drag && state.drag.tool === "baseline") {
    const a = X(state.drag.x0), b = X(state.drag.x1);
    ctx.fillStyle = "rgba(74,168,255,0.18)";
    ctx.fillRect(Math.min(a, b), g.top, Math.abs(b - a), g.topH);
  }

  // residual sub-panel
  if ($("show-resid").checked && fit && g.residH > 0) {
    let rlo = Infinity, rhi = -Infinity;
    fit.report.residual.forEach((r, i) => { if (!isExcluded(i) && isFinite(r)) { rlo = Math.min(rlo, r); rhi = Math.max(rhi, r); } });
    if (!isFinite(rlo)) { rlo = -1; rhi = 1; }
    const p = Math.max(Math.abs(rlo), Math.abs(rhi)) || 1;
    const RY = (r) => g.resid + g.residH / 2 - (r / p) * (g.residH / 2 - 2);
    ctx.strokeStyle = "#2a3744";
    ctx.beginPath(); ctx.moveTo(M.l, RY(0)); ctx.lineTo(g.w - M.r, RY(0)); ctx.stroke();
    ctx.strokeStyle = "#8ba0b2"; ctx.beginPath();
    for (let i = 0; i < xs.length; i++) {
      if (isExcluded(i)) continue;
      const r = fit.report.residual[i];
      if (!isFinite(r)) continue;
      const px = X(xs[i]), py = RY(r);
      ctx.moveTo(px, RY(0)); ctx.lineTo(px, py);
    }
    ctx.stroke();
    ctx.fillStyle = "#8ba0b2"; ctx.font = "10px ui-monospace";
    ctx.fillText(`residual ±${p.toFixed(3)}`, M.l + 4, g.resid + 10);
  }
  ctx.fillStyle = "#8ba0b2"; ctx.font = "11px ui-monospace";
  ctx.fillText(state.dispUnit, g.w - M.r - 40, g.h - 8);
}

// ---------------- mouse interaction ----------------
function eventX(e) {
  const r = canvas.getBoundingClientRect();
  return ((e.clientX - r.left) / r.width) * canvas.clientWidth;
}
function pxToXData(px) {
  const { xr } = makeScales();
  const { w } = cssScale();
  const t = (px - M.l) / (w - M.l - M.r);
  return xr.x0 + t * (xr.x1 - xr.x0);
}
function nearestPointIndex(dispX) {
  let best = -1, bd = Infinity;
  const xs = convArr(state.x, state.dispUnit);
  for (let i = 0; i < xs.length; i++) {
    const d = Math.abs(xs[i] - dispX);
    if (d < bd) { bd = d; best = i; }
  }
  return best;
}
canvas.addEventListener("mousedown", (e) => {
  if (!state.x.length) return;
  const xd = pxToXData(eventX(e));
  if (state.tool === "baseline") {
    state.drag = { tool: "baseline", x0: xd, x1: xd };
  } else if (state.tool === "mask") {
    const i = nearestPointIndex(xd);
    if (i >= 0) {
      const raw = state.x[i];
      if (!state.excluded.some((v) => v === raw)) state.excluded.push(raw);
      renderLists(); draw();
    }
  } else if (state.tool === "peak") {
    const centerRaw = toRaw(xd);
    const kind = $("peak-kind").value;
    const widthDisp = parseFloat($("peak-width").value) || 1;
    const widthRaw = convertPeakDisp(centerRaw, widthDisp, state.dispUnit, state.rawUnit)[1];
    state.peaks.push({
      kind, center0: centerRaw, width0: widthRaw,
      amp0: peakInitialHeight(), fixCenter: $("fix-center").checked,
    });
    renderLists(); draw();
  }
});
canvas.addEventListener("mousemove", (e) => {
  if (state.drag) { state.drag.x1 = pxToXData(eventX(e)); draw(); }
});
window.addEventListener("mouseup", () => {
  if (state.drag && state.drag.tool === "baseline") {
    let a = toRaw(state.drag.x0), b = toRaw(state.drag.x1);
    if (a > b) [a, b] = [b, a];
    if (b - a > 1e-12) state.windows.push([a, b]);
    state.drag = null; renderLists(); draw();
  }
});
function peakInitialHeight() {
  const ys = state.y;
  let m = 0; for (const v of ys) if (isFinite(v) && v > m) m = v;
  return Math.max(0.1, m * 0.8);
}

// ---------------- editable lists ----------------
function renderLists() {
  const wl = $("window-list"); wl.innerHTML = "";
  state.windows.forEach((w, i) => {
    const li = document.createElement("li");
    li.innerHTML = `<span class="dim">区间</span>
      <input type="number" step="any" value="${w[0].toPrecision(7)}" data-k="0">
      <span>–</span><input type="number" step="any" value="${w[1].toPrecision(7)}" data-k="1">
      <span class="dim">${state.rawUnit}</span><button>删</button>`;
    li.querySelectorAll("input").forEach((inp) => inp.addEventListener("change", () => {
      state.windows[i][+inp.dataset.k] = parseFloat(inp.value); draw();
    }));
    li.querySelector("button").onclick = () => { state.windows.splice(i, 1); renderLists(); draw(); };
    wl.appendChild(li);
  });
  const ml = $("mask-list"); ml.innerHTML = "";
  state.excluded.forEach((x, i) => {
    const li = document.createElement("li");
    li.innerHTML = `<span class="dim">点</span><input type="number" step="any" value="${x.toPrecision(8)}">
      <span class="dim">${state.rawUnit}</span><button>恢复</button>`;
    li.querySelector("input").addEventListener("change", (e) => {
      state.excluded[i] = parseFloat(e.target.value); draw();
    });
    li.querySelector("button").onclick = () => { state.excluded.splice(i, 1); renderLists(); draw(); };
    ml.appendChild(li);
  });
  const pl = $("peak-list"); pl.innerHTML = "";
  state.peaks.forEach((pk, i) => {
    const [cc, ww] = [pk.center0, pk.width0]; // shown in RAW units
    const li = document.createElement("li");
    li.innerHTML = `<span class="tag ${pk.kind}">${pk.kind}</span>
      <input type="number" step="any" value="${cc.toPrecision(7)}" data-f="center0" title="峰位(${state.rawUnit})">
      <input type="number" step="any" value="${ww.toPrecision(5)}" data-f="width0" title="FWHM(${state.rawUnit})">
      <input type="number" step="any" value="${pk.amp0.toPrecision(4)}" data-f="amp0" title="${pk.kind==='voigt'?'面积':'高度'}">
      <label class="dim"><input type="checkbox" data-f="fixCenter" ${pk.fixCenter ? "checked" : ""}>锁</label>
      <button>删</button>`;
    li.querySelectorAll("input[data-f]").forEach((inp) => {
      inp.addEventListener("change", () => {
        const f = inp.dataset.f;
        if (f === "fixCenter") state.peaks[i][f] = inp.checked;
        else state.peaks[i][f] = parseFloat(inp.value);
        draw();
      });
    });
    li.querySelector("button").onclick = () => { state.peaks.splice(i, 1); renderLists(); draw(); };
    pl.appendChild(li);
  });
}

document.querySelectorAll("button.tool").forEach((b) =>
  b.addEventListener("click", () => {
    state.tool = b.dataset.tool;
    document.querySelectorAll("button.tool").forEach((x) => x.classList.toggle("active", x === b));
  }));
$("disp-unit").addEventListener("change", (e) => {
  state.dispUnit = e.target.value; state.view = null; draw();
});
["show-raw","show-baseline","show-corrected","show-smoothed","show-fit","show-resid"]
  .forEach((id) => $(id).addEventListener("change", draw));

// ---------------- data loading / diagnostics ----------------
function showDiagnostics(d) {
  const el = $("diag");
  const lines = [];
  lines.push(`读取 ${d.rows_read} 行，保留 ${d.rows_kept} 个有效点`);
  if (d.too_few_rows) lines.push(`✗ 样本太少：有效点 < 8，拒绝拟合（独立诊断）`);
  if (d.nonfinite_rows.length)
    lines.push(`✗ 非有限值行（已丢弃）: ${d.nonfinite_rows.slice(0, 20).join(", ")}${d.nonfinite_rows.length > 20 ? " …" : ""}`);
  if (d.duplicate_x.length)
    lines.push(`! 重复 x 坐标: ${d.duplicate_x.slice(0, 12).map((v) => v.toPrecision(8)).join(", ")}${d.duplicate_x.length > 12 ? " …" : ""}`);
  if (d.monotonic === null && d.rows_kept > 1)
    lines.push(`✗ 非单调输入：检测到 ${d.nonmonotonic_pairs.length} 处逆序（保留数据供检查）`);
  else if (d.monotonic) lines.push(`单调性：${d.monotonic === "decreasing" ? "递减（换算后已自动处理端点排序）" : "递增"}`);
  if (d.parse_errors.length)
    lines.push(`✗ 解析错误: ${d.parse_errors.slice(0, 8).map(([r, m]) => `L${r}:${m}`).join("; ")}`);
  if (lines.length > 1 || d.too_few_rows || d.nonfinite_rows.length) {
    el.textContent = lines.join("\n"); el.classList.remove("hidden");
  } else el.classList.add("hidden");
}

async function loadObject(obj) {
  const v = await api("/api/upload", obj);
  state.datasetId = v.dataset_id;
  state.inputHash = v.input_hash;
  state.rawUnit = v.x_unit;
  state.x = v.x; state.y = v.y;
  state.windows = []; state.excluded = []; state.peaks = [];
  state.lastFit = null; state.view = null;
  $("disp-unit").value = state.rawUnit;
  state.dispUnit = state.rawUnit;
  $("dataset-info").textContent =
    `数据集 #${v.dataset_id} · ${v.n_points} 点 · 输入 ${v.x_header}/${v.y_header} · sha256 ${v.input_hash.slice(0, 16)}…`;
  $("versions").textContent =
    Object.entries(v.versions).map(([k, x]) => `${k}=${x}`).join("  ·  ");
  showDiagnostics(v.diagnostics);
  if (v.synthetic_truth) {
    // seed the suggested windows/exclusions/peaks from the truth
    state.windows = [[380, 470], [650, 720]];
    state.excluded = []; // user marks cosmic rays by hand
    state.peaks = v.synthetic_truth.peaks.map((p) => ({
      kind: p.kind,
      center0: p.center_nm + 2, width0: p.fwhm_nm + 1,
      amp0: p.height * 0.8, fixCenter: false,
    }));
    $("baseline-degree").value = 1; $("smooth").value = 0;
  }
  renderLists(); draw(); refreshSchemes();
}

$("load-synthetic").onclick = () =>
  loadObject({ synthetic: true }).catch((e) => alert(e.message));
$("file").addEventListener("change", () => {
  const f = $("file").files[0];
  if (!f) return;
  const reader = new FileReader();
  reader.onload = () => loadObject({ csv: reader.result }).catch((e) => alert(e.message));
  reader.readAsText(f);
});

// ---------------- fitting ----------------
function currentSpec() {
  return {
    smooth_sigma: parseFloat($("smooth").value) || 0,
    baseline: { degree: parseInt($("baseline-degree").value, 10) || 0, windows: state.windows },
    excluded_x: state.excluded,
    peaks: state.peaks.map((p) => ({
      kind: p.kind, center0: p.center0, width0: p.width0,
      amp0: p.amp0, fix_center: p.fixCenter,
    })),
  };
}
async function doFit(autoCandidate) {
  if (!state.datasetId) return alert("请先载入数据");
  if (!state.windows.length) return alert("请先圈定至少一个基线区间");
  if (!state.peaks.length) return alert("请先放置至少一个峰候选");
  $("fit-status").textContent = autoCandidate ? "自动候选拟合中…" : "拟合中…";
  try {
    const v = await api("/api/fit", {
      dataset_id: state.datasetId, spec: currentSpec(),
      auto_candidate: !!autoCandidate,
    });
    state.lastFit = v;
    renderResults(v);
    const r = v.report;
    $("fit-status").innerHTML =
      `${v.replayed ? "重放缓存结果（相同指纹）" : "新拟合完成"} · ` +
      (r.converged
        ? `<span style="color:var(--ok)">已收敛</span>`
        : `<span style="color:var(--bad)">未收敛：${r.status}（保留最后有限迭代用于诊断，未标记为已接受）</span>`) +
      ` · ${r.iterations} 次迭代 · RMSE ${r.rmse.toExponential(3)}`;
    draw(); refreshSchemes();
  } catch (e) {
    $("fit-status").textContent = "失败：" + e.message;
  }
}
$("run-fit").onclick = () => doFit(false);
$("auto-fit").onclick = () => doFit(true);

// ---------------- result tables ----------------
function fmt(x, n = 5) { return isFinite(x) ? Number(x).toPrecision(n) : String(x); }
function renderResults(v) {
  $("results").classList.remove("hidden");
  const r = v.report;
  const m = $("metrics");
  m.innerHTML = "";
  const rows = [
    ["收敛", r.converged ? "是" : `否（${r.status}）`],
    ["迭代次数", r.iterations],
    ["χ² (RSS)", fmt(r.rss, 6)],
    ["自由度", r.dof],
    ["RMSE", fmt(r.rmse, 5)],
    ["AIC", fmt(r.aic, 6)],
    ["BIC", fmt(r.bic, 6)],
    ["方案指纹", `<span class="mono">${v.fingerprint.slice(0, 24)}…</span>`],
    ["输入哈希", `<span class="mono">${state.inputHash.slice(0, 24)}…</span>`],
  ];
  for (const [k, val] of rows) {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${k}</td><td>${val}</td>`; m.appendChild(tr);
  }
  const pt = $("peaks");
  pt.innerHTML = "<thead><tr><th>#</th><th>型</th><th>峰位</th><th>宽</th><th>高</th><th>面积</th><th>SE</th><th>95% CI</th></tr></thead>";
  r.peaks.forEach((pk, i) => {
    const tr = document.createElement("tr");
    const extras = pk.sigma != null
      ? `<br><span class="dim">σ=${fmt(pk.sigma, 4)} γ=${fmt(pk.gamma, 4)}</span>` : "";
    tr.innerHTML = `<td>${i + 1}</td><td>${pk.kind}</td>
      <td>${fmt(pk.center, 7)}</td><td>${fmt(pk.width, 6)}</td>
      <td>${fmt(pk.height, 5)}</td><td>${fmt(pk.area, 6)}</td><td>${fmt(pk.area_se, 3)}</td>
      <td>[${fmt(pk.area_ci95[0], 6)}, ${fmt(pk.area_ci95[1], 6)}]${extras}</td>`;
    pt.appendChild(tr);
  });

  // correlation heat map (free parameters)
  const c = r.correlation, nf = Math.round(Math.sqrt(c.length));
  const host = $("corr"); host.innerHTML = "";
  if (!nf) { host.textContent = "无自由参数"; return; }
  const labels = r.free_map.map(([pi, slot]) => {
    const nm = ["c", "w", "h", "a"][slot] || `p${slot}`;
    return `${pi + 1}${nm}`;
  });
  let html = "<table><thead><tr><th></th>" + labels.map((l) => `<th>${l}</th>`).join("") + "</tr></thead><tbody>";
  for (let i = 0; i < nf; i++) {
    html += `<tr><th>${labels[i]}</th>`;
    for (let j = 0; j < nf; j++) {
      const vv = c[i * nf + j];
      const a = isFinite(vv) ? Math.min(1, Math.abs(vv)) : 0;
      const col = vv >= 0 ? `rgba(91,211,145,${a})` : `rgba(255,107,107,${a})`;
      html += `<td style="background:${col}" title="${labels[i]}–${labels[j]} = ${fmt(vv, 3)}">${fmt(vv, 2)}</td>`;
    }
    html += "</tr>";
  }
  html += "</tbody></table>";
  host.innerHTML = html;
}

// ---------------- scheme history ----------------
async function refreshSchemes() {
  if (!state.datasetId) return;
  const v = await apiGet(`/api/schemes?dataset_id=${state.datasetId}`);
  state.schemes = v.schemes;
  const tb = $("schemes").querySelector("tbody");
  tb.innerHTML = "";
  v.schemes.forEach((s) => {
    const tr = document.createElement("tr");
    if (s.accepted) tr.className = "accepted";
    const convBadge = "";
    const nPeaks = s.spec.peaks ? s.spec.peaks.length : 0;
    const origin = s.origin === "manual"
      ? `<span class="badge manual">人工</span>`
      : `<span class="badge auto">自动候选</span>`;
    const accepted = s.accepted ? `<span class="badge accepted">已确认</span>` : "";
    tr.innerHTML = `<td>${s.id}</td><td>${origin}</td><td>${nPeaks}</td>
      <td>${accepted} ${convBadge}</td>
      <td class="mono dim">${s.fingerprint ? s.fingerprint.slice(0, 16) + "…" : ""}</td>
      <td><button>${s.accepted ? "撤销确认" : "确认此方案"}</button></td>`;
    // spec fingerprint not returned in list; use id-based accept
    tr.querySelector("button").onclick = async () => {
      try {
        const want = !s.accepted;
        let payload = { scheme_id: s.id, accepted: want };
        if (want && s.origin !== "manual") {
          if (!confirm("自动候选不会自动取代人工方案。确认要提升此自动候选为已接受方案吗？")) return;
          payload.confirm_promote_auto = true;
        }
        await api("/api/schemes/accept", payload);
        refreshSchemes();
      } catch (e) { alert(e.message); }
    };
    tb.appendChild(tr);
  });
}

// resize / init
window.addEventListener("resize", draw);
draw();

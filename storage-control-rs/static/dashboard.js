// Dashboard polling and rendering. All strings that come from the host
// (model names, alert messages, systemd output) are escaped before they reach
// innerHTML: smartctl output is untrusted input.
"use strict";

const $ = (id) => document.getElementById(id);
const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (c) => (
  { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]
));
const fmtBytes = (bytes) => {
  if (!bytes) return "-";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) { value /= 1024; i++; }
  return value.toFixed(1) + " " + units[i];
};

async function getJSON(url, options) {
  const response = await fetch(url, options);
  if (!response.ok) throw new Error(url + " -> " + response.status);
  return response.json();
}

function badgeClass(status) {
  if (status === "active" || status === "success") return "active";
  if (status === "inactive" || status === "failed" || status === "never-run") return "inactive";
  return "unknown";
}

// ---------------------------------------------------------------- host

async function refreshSummary() {
  try {
    const data = await getJSON("/api/health/summary");

    $("host").textContent =
      data.hostname + " — " + data.os + " — uptime " + Math.round(data.uptime_seconds / 3600) + "h";

    const cpu = data.cpu;
    $("cpu-pct").textContent = cpu.percent_total.toFixed(1) + "%";
    $("cpu-meta").textContent =
      cpu.count_logical + " threads / " + cpu.count_physical + " cores @ " + Math.round(cpu.freq_mhz || 0) +
      " MHz · load " + cpu.loadavg.map((x) => x.toFixed(2)).join(", ");
    $("cpu-bar").style.width = Math.max(0, Math.min(100, cpu.percent_total)) + "%";

    $("mem-pct").textContent = data.memory.percent.toFixed(1) + "%";
    $("mem-meta").textContent = fmtBytes(data.memory.used) + " / " + fmtBytes(data.memory.total) +
      " · cached " + fmtBytes(data.memory.cached);
    $("mem-bar").style.width = Math.max(0, Math.min(100, data.memory.percent)) + "%";

    $("swap-pct").textContent = data.swap.percent.toFixed(1) + "%";
    $("swap-meta").textContent = fmtBytes(data.swap.used) + " / " + fmtBytes(data.swap.total);
    $("swap-bar").style.width = Math.max(0, Math.min(100, data.swap.percent)) + "%";

    renderTemps(data.temps || {});
    renderDisks(data.disks || []);
    renderRaid(data.raid || {});
  } catch (e) {
    $("host").textContent = "summary error: " + e.message;
  }
}

function renderTemps(temps) {
  const html = [];
  for (const [chip, value] of Object.entries(temps)) {
    if (Array.isArray(value)) {
      for (const entry of value) {
        const current = entry.current != null ? Number(entry.current).toFixed(1) : "-";
        const high = entry.high != null ? Number(entry.high).toFixed(0) : "-";
        const critical = entry.critical != null ? Number(entry.critical).toFixed(0) : "-";
        html.push(
          '<div class="card"><div class="muted">' + esc(entry.label || chip) + "</div>" +
          '<div class="stat">' + current + "°C</div>" +
          '<div class="muted">high ' + high + " · crit " + critical + "</div></div>"
        );
      }
    } else if (value && typeof value === "object") {
      for (const [key, reading] of Object.entries(value)) {
        if (typeof reading === "number") {
          html.push(
            '<div class="card"><div class="muted">' + esc(chip + " " + key) + "</div>" +
            '<div class="stat">' + reading.toFixed(1) + "°C</div></div>"
          );
        }
      }
    }
  }
  $("temps").innerHTML = html.join("") || '<div class="muted">n/a</div>';
}

function renderDisks(disks) {
  const box = $("disks");
  box.innerHTML = "";
  for (const disk of disks) {
    const used = Math.max(0, Math.min(100, disk.percent || 0));
    const card = document.createElement("div");
    card.className = "card";
    card.innerHTML =
      '<div style="display:flex;justify-content:space-between;align-items:baseline;margin-bottom:6px;">' +
      "<strong>" + esc(disk.device) + '</strong><span class="muted">' +
      esc(disk.mountpoint) + " · " + esc(disk.fstype) + "</span></div>" +
      '<div class="bar"><div style="width:' + used + '%;background:var(--accent);"></div></div>' +
      '<div style="display:flex;justify-content:space-between;margin-top:6px;font-size:13px;">' +
      "<span>Used " + fmtBytes(disk.used) + " (" + used.toFixed(1) + "%)</span>" +
      "<span>Free " + fmtBytes(disk.free) + "</span>" +
      "<span>Total " + fmtBytes(disk.total) + "</span></div>";
    box.appendChild(card);
  }
  if (!box.hasChildNodes()) box.innerHTML = '<div class="muted">n/a</div>';
}

function renderRaid(raid) {
  const md0 = raid.md0 || {};
  const arrays = raid.arrays || [];
  const lines = [];
  if (arrays.length) {
    lines.push(
      '<div class="muted">' + esc(arrays.map((a) => a.name + ": " + a.summary).join(" | ")) + "</div>"
    );
  }
  lines.push(
    "<div style=\"display:flex;gap:16px;flex-wrap:wrap;margin-top:8px;\">" +
    "<div>State: <strong>" + esc(md0.state || "unknown") + "</strong></div>" +
    "<div>Devices: <strong>" + esc(md0.active_devices ?? "-") + "/" + esc(md0.raid_devices ?? "-") + "</strong></div>" +
    "<div>Failed: <strong>" + esc(md0.failed_devices ?? "-") + "</strong></div>" +
    "<div>Spare: <strong>" + esc(md0.spare_devices ?? "-") + "</strong></div>" +
    "</div>"
  );
  if (md0.failed_devices > 0 || !["clean", "active"].includes(String(md0.state))) {
    $("raid").innerHTML = '<div style="color:#fca5a5;">' + lines.join("") + "</div>";
  } else {
    $("raid").innerHTML = lines.join("");
  }
}

// ---------------------------------------------------------------- services

async function refreshServices() {
  try {
    const list = await getJSON("/api/health/services");
    const body = document.querySelector("#svc-table tbody");
    body.innerHTML = "";
    for (const svc of list) {
      const row = document.createElement("tr");
      const cls = badgeClass(svc.display_status);
      row.innerHTML =
        "<td>" + esc(svc.name) + "</td><td>" + esc(svc.scope) + "</td><td>" + esc(svc.group) +
        '</td><td><span class="badge ' + cls + '">' + esc(svc.display_status) + "</span></td><td>" +
        esc(svc.enabled) + "</td><td>" + esc(svc.last_run || svc.active_since || "") + "</td>";
      body.appendChild(row);
    }

    const immichCore = list.filter(
      (x) => x.group === "immich" && x.name.endsWith(".service") && !x.name.includes("nightfix")
    );
    setHealth("immich", immichCore);

    const ncNames = ["php8.5-fpm.service", "nginx.service", "redis-server.service", "postgresql.service"];
    const nextcloudCore = list.filter((x) => ncNames.includes(x.name));
    setHealth("nextcloud", nextcloudCore, ncNames.length);

    const stalwartCore = list.filter((x) => x.group === "stalwart" && x.name.endsWith(".service"));
    setHealth("stalwart", stalwartCore);
  } catch (e) {
    console.error(e);
  }
}

function setHealth(name, services, expected) {
  const badge = $(name + "-health");
  const meta = $(name + "-health-meta");
  if (!badge || !meta) return;
  const total = expected ?? services.length;
  const ok = total > 0 && services.length === total && services.every((x) => x.active === "active");
  badge.className = "badge " + (ok ? "active" : "inactive");
  badge.textContent = ok ? "OK" : "DEGRADED";
  meta.textContent = services.map((x) => x.name + ": " + x.active).join(" · ");
}

// Versions come from the updates module; until it answers, leave "checking".
async function refreshVersions() {
  let data;
  try {
    data = await getJSON("/updates/api/availability");
  } catch (e) {
    for (const name of ["immich", "nextcloud", "stalwart"]) {
      const badge = $(name + "-version-badge");
      if (badge) { badge.className = "badge unknown"; badge.textContent = "n/a"; }
    }
    return;
  }
  for (const name of ["immich", "nextcloud", "stalwart"]) {
    const target = (data.targets || []).find((t) => t.target === name) || {};
    const current = $(name + "-version-current");
    const latest = $(name + "-version-latest");
    const badge = $(name + "-version-badge");
    if (current) current.textContent = target.current || "-";
    if (latest) latest.textContent = target.latest || "-";
    if (badge) {
      const update = Boolean(target.update_available);
      badge.className = "badge " + (update ? "inactive" : "active");
      badge.textContent = update ? "update" : "up to date";
    }
  }
}

// ---------------------------------------------------------------- SMART

async function refreshSmart() {
  try {
    const data = await getJSON("/api/health/smart/status");
    const body = document.querySelector("#smart-table tbody");
    body.innerHTML = "";

    for (const disk of data.states || []) {
      const baseline = disk.baseline || {};
      const increased = new Set();
      if (baseline.reallocated != null && disk.reallocated > baseline.reallocated) increased.add("reallocated");
      if (baseline.pending != null && disk.pending > baseline.pending) increased.add("pending");
      if (baseline.offline != null && disk.offline_uncorrectable > baseline.offline) increased.add("offline");
      if (baseline.ata_errors != null && disk.ata_error_count > baseline.ata_errors) increased.add("ata");

      const cell = (value, key) => increased.has(key) ? "<td><strong>" + esc(value) + "</strong></td>" : "<td>" + esc(value) + "</td>";
      const baselineText = Object.keys(baseline).length
        ? "R:" + (baseline.reallocated ?? "-") + " P:" + (baseline.pending ?? "-") +
          " O:" + (baseline.offline ?? "-") + " A:" + (baseline.ata_errors ?? "-")
        : "-";

      const row = document.createElement("tr");
      row.setAttribute("data-device", disk.device);
      row.innerHTML =
        "<td>" + esc(disk.device) + "</td><td>" + esc(disk.model || "-") + "</td><td>" +
        esc(disk.health || "-") + "</td><td>" +
        (disk.temperature != null ? esc(disk.temperature) + "°C" : "-") + "</td>" +
        cell(disk.reallocated, "reallocated") + cell(disk.pending, "pending") +
        cell(disk.offline_uncorrectable, "offline") + cell(disk.ata_error_count, "ata") +
        "<td>" + esc(baselineText) + "</td>";
      body.appendChild(row);
    }

    renderSmartAlerts(data.alerts || []);
  } catch (e) {
    console.error(e);
  }
}

function renderSmartAlerts(alerts) {
  const block = $("smart-alarms-block");
  const title = $("smart-alarms-title");
  const box = $("smart-alerts");
  if (!block || !title || !box) return;

  block.style.borderColor = alerts.length ? "#7f1d1d" : "#334155";
  block.style.background = alerts.length ? "#2b1212" : "#0f172a";
  title.style.color = alerts.length ? "#fecaca" : "#94a3b8";

  if (!alerts.length) {
    box.innerHTML = '<div class="muted">Активных алармов SMART нет.</div>';
  } else {
    box.innerHTML = alerts.map((alert) =>
      '<div class="flash danger" style="background:#7f1d1d;border-left:4px solid #ef4444;display:flex;justify-content:space-between;align-items:flex-start;gap:12px;">' +
      '<div style="flex:1;">' + esc(alert.device) + " · " + esc(String(alert.severity).toUpperCase()) + ": " +
      esc(alert.message) + "</div>" +
      '<button class="btn" style="padding:4px 10px;font-size:12px;flex-shrink:0;" data-alert-id="' +
      esc(alert.id) + '">Resolve</button></div>'
    ).join("");
    for (const button of box.querySelectorAll("button[data-alert-id]")) {
      button.addEventListener("click", () => resolveSmartAlert(button.dataset.alertId));
    }
  }

  const alertDevices = new Set(alerts.map((a) => a.device));
  for (const row of document.querySelectorAll("#smart-table tbody tr")) {
    const isAlerting = alertDevices.has(row.getAttribute("data-device"));
    row.style.background = isAlerting ? "#7f1d1d" : "";
    if (isAlerting && row.cells[2]) {
      row.cells[2].innerHTML = '<span class="badge inactive">ALARM</span>';
    }
  }
}

async function refreshSmartManual() {
  const button = $("smart-refresh-btn");
  if (!button) return;
  button.disabled = true;
  const original = button.textContent;
  button.textContent = "Refreshing…";
  try {
    await getJSON("/api/health/smart/refresh", { method: "POST" });
    await refreshSmart();
  } catch (e) {
    alert("SMART refresh failed: " + e.message);
  } finally {
    button.disabled = false;
    button.textContent = original;
  }
}

async function resolveSmartAlert(id) {
  try {
    await getJSON("/api/health/smart/alert/" + encodeURIComponent(id) + "/resolve", { method: "POST" });
    await refreshSmart();
  } catch (e) {
    alert("Resolve failed: " + e.message);
  }
}

// ---------------------------------------------------------------- boot

function boot() {
  const button = $("smart-refresh-btn");
  if (button) button.addEventListener("click", refreshSmartManual);

  refreshSummary();
  refreshServices();
  refreshSmart();
  refreshVersions();

  setInterval(refreshSummary, 5000);
  setInterval(refreshServices, 30000);
  setInterval(refreshSmart, 60000);
  setInterval(refreshVersions, 300000);
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", boot);
} else {
  boot();
}

"use strict";

const SESSION_KEY = "docker-services-web-token";
const MAX_ACTIVITY = 600;

const dom = {};
const state = {
  token: "",
  meta: null,
  snapshot: null,
  selectedService: null,
  filter: "",
  view: "fleet",
  activeOperation: null,
  diagnoses: new Map(),
  activity: [],
  integrations: null,
  backups: null,
  restoreInspection: null,
  streamAbort: null,
  streamGeneration: 0,
  reconnectDelay: 1000,
  lastSampleAt: null,
  dialogOptions: null,
};

document.addEventListener("DOMContentLoaded", initialize);

async function initialize() {
  cacheDom();
  bindEvents();
  try {
    const response = await fetch("/api/meta", { cache: "no-store" });
    state.meta = await response.json();
    dom.tokenHint.textContent = `Token file: ${state.meta.token_hint}`;
  } catch (error) {
    dom.tokenHint.textContent = "Could not read the server token location.";
  }

  const existing = sessionStorage.getItem(SESSION_KEY);
  if (existing) {
    dom.tokenInput.value = existing;
    await authenticate(existing, false);
  } else {
    dom.tokenInput.focus();
  }
  window.setInterval(updateSampleAge, 1000);
}

function cacheDom() {
  const ids = [
    "auth-gate", "auth-form", "token-input", "toggle-token", "token-hint", "auth-error",
    "app-shell", "connection-state", "host-identity", "sample-age", "lock-session",
    "metric-ip", "metric-cpu", "metric-load", "metric-memory", "metric-disk", "disk-metric",
    "metric-uptime", "operation-rail", "fleet-summary", "state-tally", "service-search",
    "service-rows", "workbench-empty", "workbench-content", "selected-service-state",
    "selected-service-name", "selected-service-purpose", "selected-service-link", "action-enable",
    "action-disable", "action-restart", "action-diagnose", "config-readout", "port-list",
    "interlock-state", "dependency-list", "diagnosis-compact", "diagnosis-compact-body",
    "integration-ledger", "refresh-integrations", "apply-integrations", "diagnostic-surface",
    "scan-fleet", "backup-policy", "backup-list", "refresh-backups", "create-backup",
    "upload-form", "bundle-file", "url-form", "bundle-url", "restore-inspection",
    "activity-strip-list", "activity-full", "clear-activity", "footer-status", "confirm-dialog",
    "confirm-form", "confirm-signal", "confirm-title", "confirm-detail", "confirm-phrase-field",
    "confirm-phrase-label", "confirm-phrase", "confirm-submit", "toast-rack",
  ];
  for (const id of ids) dom[toCamel(id)] = document.getElementById(id);
  dom.tabs = [...document.querySelectorAll("[data-view]")];
  dom.viewLinks = [...document.querySelectorAll("[data-open-view]")];
  dom.views = [...document.querySelectorAll("[data-view-panel]")];
}

function bindEvents() {
  dom.authForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    await authenticate(dom.tokenInput.value.trim(), true);
  });
  dom.toggleToken.addEventListener("click", () => {
    const visible = dom.tokenInput.type === "text";
    dom.tokenInput.type = visible ? "password" : "text";
    dom.toggleToken.setAttribute("aria-label", visible ? "Show token" : "Hide token");
  });
  dom.lockSession.addEventListener("click", lockSession);
  dom.tabs.forEach((button) => button.addEventListener("click", () => openView(button.dataset.view)));
  dom.viewLinks.forEach((button) => button.addEventListener("click", () => openView(button.dataset.openView)));
  dom.serviceSearch.addEventListener("input", () => {
    state.filter = dom.serviceSearch.value.trim().toLowerCase();
    renderFleet();
  });
  dom.serviceRows.addEventListener("click", (event) => {
    const row = event.target.closest("tr[data-service]");
    if (row) selectService(row.dataset.service, true);
  });
  dom.serviceRows.addEventListener("keydown", (event) => {
    const row = event.target.closest("tr[data-service]");
    if (!row) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      selectService(row.dataset.service, true);
    }
  });
  dom.actionEnable.addEventListener("click", () => reviewServiceAction("enable"));
  dom.actionDisable.addEventListener("click", () => reviewServiceAction("disable"));
  dom.actionRestart.addEventListener("click", () => reviewServiceAction("restart"));
  dom.actionDiagnose.addEventListener("click", diagnoseSelected);
  dom.portList.addEventListener("click", (event) => {
    const button = event.target.closest("[data-port-index]");
    if (button) editPort(Number(button.dataset.portIndex));
  });
  dom.diagnosisCompactBody.addEventListener("click", (event) => {
    const button = event.target.closest("[data-safe-fix]");
    if (button) reviewServiceAction(button.dataset.safeFix);
  });
  dom.refreshIntegrations.addEventListener("click", loadIntegrations);
  dom.applyIntegrations.addEventListener("click", reviewIntegrations);
  dom.scanFleet.addEventListener("click", scanFleet);
  dom.refreshBackups.addEventListener("click", loadBackups);
  dom.createBackup.addEventListener("click", createBackup);
  dom.backupList.addEventListener("click", handleBackupAction);
  dom.uploadForm.addEventListener("submit", inspectUpload);
  dom.urlForm.addEventListener("submit", inspectUrl);
  dom.restoreInspection.addEventListener("click", (event) => {
    if (event.target.closest("[data-restore-now]")) restoreInspectedBundle();
  });
  dom.clearActivity.addEventListener("click", () => {
    state.activity = [];
    renderActivity();
  });
  dom.confirmForm.addEventListener("submit", validateDialogSubmission);
  document.addEventListener("keydown", handleGlobalKey);
}

async function authenticate(token, announceFailure) {
  if (token.length < 32) {
    dom.authError.textContent = "The token must contain at least 32 characters.";
    return;
  }
  dom.authError.textContent = "";
  const previous = state.token;
  state.token = token;
  setFooter("Authenticating private watch channel…");
  try {
    const data = await api("/api/state");
    sessionStorage.setItem(SESSION_KEY, token);
    dom.authGate.hidden = true;
    dom.appShell.hidden = false;
    requestAnimationFrame(() => dom.appShell.classList.add("ready"));
    applySnapshot(data);
    connectEventStream();
    setFooter(state.meta?.demo ? "Authenticated · demo state is read-only" : "Authenticated · live stack control enabled");
  } catch (error) {
    state.token = previous;
    if (announceFailure) dom.authError.textContent = messageFrom(error);
    else sessionStorage.removeItem(SESSION_KEY);
    dom.authGate.hidden = false;
    dom.appShell.hidden = true;
    dom.tokenInput.focus();
  }
}

function lockSession() {
  state.streamGeneration += 1;
  state.streamAbort?.abort();
  state.streamAbort = null;
  state.token = "";
  sessionStorage.removeItem(SESSION_KEY);
  dom.tokenInput.value = "";
  dom.appShell.hidden = true;
  dom.authGate.hidden = false;
  dom.authError.textContent = "Watch session locked in this browser tab.";
  dom.tokenInput.focus();
}

async function api(path, options = {}) {
  const headers = new Headers(options.headers || {});
  if (state.token) headers.set("Authorization", `Bearer ${state.token}`);
  if (options.body && !(options.body instanceof FormData) && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  const response = await fetch(path, { ...options, headers, cache: "no-store" });
  if (response.status === 401) {
    const error = await readError(response);
    if (!path.endsWith("/state")) lockSession();
    throw new Error(error);
  }
  if (!response.ok) throw new Error(await readError(response));
  if (response.status === 204) return null;
  const type = response.headers.get("Content-Type") || "";
  return type.includes("application/json") ? response.json() : response;
}

async function readError(response) {
  try {
    const body = await response.json();
    return [body.error, body.recovery].filter(Boolean).join(" ");
  } catch {
    return `Request failed with HTTP ${response.status}.`;
  }
}

function connectEventStream() {
  state.streamGeneration += 1;
  const generation = state.streamGeneration;
  state.streamAbort?.abort();
  const controller = new AbortController();
  state.streamAbort = controller;
  consumeEventStream(generation, controller).catch((error) => {
    if (controller.signal.aborted || generation !== state.streamGeneration || !state.token) return;
    setConnection("reconnecting", "wait");
    setFooter(`Live channel interrupted · retrying in ${Math.round(state.reconnectDelay / 1000)}s`);
    window.setTimeout(() => {
      if (generation === state.streamGeneration && state.token) connectEventStream();
    }, state.reconnectDelay);
    state.reconnectDelay = Math.min(state.reconnectDelay * 1.8, 15000);
    console.warn("event stream interrupted", error);
  });
}

async function consumeEventStream(generation, controller) {
  const response = await fetch("/api/events", {
    headers: { Authorization: `Bearer ${state.token}` },
    cache: "no-store",
    signal: controller.signal,
  });
  if (response.status === 401) {
    lockSession();
    return;
  }
  if (!response.ok || !response.body) throw new Error(`Event channel HTTP ${response.status}`);
  state.reconnectDelay = 1000;
  setConnection("live", "ready");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  while (generation === state.streamGeneration) {
    const { value, done } = await reader.read();
    if (done) throw new Error("Event channel closed");
    buffer += decoder.decode(value, { stream: true });
    const frames = buffer.split(/\r?\n\r?\n/);
    buffer = frames.pop() || "";
    for (const frame of frames) processSseFrame(frame);
  }
}

function processSseFrame(frame) {
  let eventName = "message";
  const data = [];
  for (const line of frame.split(/\r?\n/)) {
    if (line.startsWith("event:")) eventName = line.slice(6).trim();
    if (line.startsWith("data:")) data.push(line.slice(5).trimStart());
  }
  if (!data.length) return;
  let payload;
  try {
    payload = JSON.parse(data.join("\n"));
  } catch {
    return;
  }
  if (eventName === "snapshot") applySnapshot(payload);
  else if (eventName === "activity") addActivity(payload);
  else if (eventName === "runtime_error") {
    setConnection("stale", "fault");
    setFooter(payload.error || "Runtime refresh failed; showing last good state.", true);
  } else if (eventName === "reload") {
    refreshState();
  }
}

async function refreshState() {
  try {
    applySnapshot(await api("/api/state"));
  } catch (error) {
    toast(messageFrom(error), "error");
  }
}

function applySnapshot(payload) {
  if (!payload?.snapshot) return;
  const oldOperation = state.activeOperation?.id;
  state.snapshot = payload.snapshot;
  state.activeOperation = payload.active_operation || null;
  state.lastSampleAt = new Date(payload.sampled_at);
  if (!state.selectedService || !serviceByName(state.selectedService)) {
    state.selectedService = state.snapshot.services[0]?.service || null;
  }
  renderHost();
  renderFleet();
  renderWorkbench();
  renderOperation();
  updateSampleAge();
  setConnection("live", "ready");
  if (oldOperation && !state.activeOperation) {
    if (state.view === "backups") loadBackups();
    if (state.view === "integrations") loadIntegrations();
  }
}

function renderHost() {
  const snapshot = state.snapshot;
  if (!snapshot) return;
  const stats = snapshot.host_stats || {};
  dom.hostIdentity.textContent = `${snapshot.project || "compose"} · ${snapshot.host}`;
  dom.metricIp.textContent = stats.ip || snapshot.host || "—";
  dom.metricCpu.textContent = stats.cpu_percent == null
    ? `${stats.cpu_cores || "—"} cores`
    : `${stats.cpu_percent.toFixed(1)}% · ${stats.cpu_cores || "—"}c`;
  dom.metricLoad.textContent = stats.load_1m == null ? "—" : stats.load_1m.toFixed(2);
  dom.metricMemory.textContent = stats.memory_used == null || stats.memory_total == null
    ? "—"
    : `${formatBytes(stats.memory_used)} / ${formatBytes(stats.memory_total)}`;
  const diskUsed = stats.disk_total != null && stats.disk_free != null ? stats.disk_total - stats.disk_free : null;
  const diskPercent = diskUsed != null && stats.disk_total ? (diskUsed / stats.disk_total) * 100 : null;
  dom.metricDisk.textContent = diskPercent == null ? "—" : `${diskPercent.toFixed(0)}% · ${formatBytes(stats.disk_free)} free`;
  dom.diskMetric.classList.toggle("pressure", diskPercent != null && diskPercent >= 90);
  dom.diskMetric.querySelector("dt").textContent = diskPercent != null && diskPercent >= 90 ? "DISK!" : "DISK";
  dom.metricUptime.textContent = formatDuration(stats.uptime_seconds);
}

function renderFleet() {
  const services = filteredServices();
  if (!state.snapshot) return;
  const all = state.snapshot.services;
  const totals = all.reduce((result, service) => {
    result[serviceState(service)] += 1;
    return result;
  }, { ready: 0, wait: 0, fault: 0, off: 0 });
  dom.fleetSummary.textContent = `${all.length} configured · ${totals.ready} ready · ${totals.fault} faulted · ${totals.off} off`;
  dom.stateTally.innerHTML = [
    tally("ready", totals.ready, "READY"),
    tally("wait", totals.wait, "WAIT"),
    tally("fault", totals.fault, "FAULT"),
    tally("off", totals.off, "OFF"),
  ].join("");
  if (!services.length) {
    dom.serviceRows.innerHTML = `<tr><td colspan="5" class="empty-cell">No service matches “${escapeHtml(state.filter)}”. Clear the filter to restore the full watch.</td></tr>`;
    return;
  }
  dom.serviceRows.innerHTML = services.map((service) => {
    const selected = service.service === state.selectedService;
    const endpoint = service.urls?.[0] || "—";
    const kind = serviceState(service);
    return `<tr class="service-row${selected ? " selected" : ""}" data-service="${escapeAttr(service.service)}" tabindex="${selected ? "0" : "-1"}" aria-selected="${selected}">
      <td><span class="state-badge ${kind}">[${stateWord(kind)}]</span></td>
      <td class="service-name">${escapeHtml(service.service)}${service.issue_count ? ` <span class="fault" aria-label="${service.issue_count} issue">!${service.issue_count}</span>` : ""}</td>
      <td class="purpose-cell">${escapeHtml(service.purpose)}</td>
      <td class="endpoint-cell">${escapeHtml(endpoint)}</td>
      <td><span class="row-open" aria-hidden="true">${icon("external")}</span></td>
    </tr>`;
  }).join("");
}

function renderWorkbench() {
  const service = selectedService();
  dom.workbenchEmpty.hidden = Boolean(service);
  dom.workbenchContent.hidden = !service;
  if (!service) return;
  const kind = serviceState(service);
  dom.selectedServiceState.className = `state-badge ${kind}`;
  dom.selectedServiceState.textContent = `[${stateWord(kind)}]`;
  dom.selectedServiceName.textContent = service.service;
  dom.selectedServicePurpose.textContent = service.purpose;
  const url = safeUrl(service.urls?.[0]);
  if (url) {
    dom.selectedServiceLink.href = url;
    dom.selectedServiceLink.removeAttribute("aria-disabled");
    dom.selectedServiceLink.removeAttribute("tabindex");
  } else {
    dom.selectedServiceLink.removeAttribute("href");
    dom.selectedServiceLink.setAttribute("aria-disabled", "true");
    dom.selectedServiceLink.setAttribute("tabindex", "-1");
  }
  dom.selectedServiceLink.setAttribute("aria-label", url ? `Open ${service.service}` : `${service.service} has no published URL`);
  const blocked = Boolean(state.activeOperation) || !service.configured;
  dom.actionEnable.disabled = blocked || (service.running && !service.disabled);
  dom.actionDisable.disabled = blocked || service.disabled;
  dom.actionRestart.disabled = blocked || !service.running;
  dom.actionDiagnose.disabled = !service.container;
  dom.actionEnable.hidden = !service.disabled && service.running;
  dom.actionDisable.hidden = service.disabled;
  const health = service.healthcheck_configured ? "Declared" : "Not declared";
  const endpoint = service.urls?.length ? service.urls.join(", ") : "No published endpoint";
  dom.configReadout.innerHTML = readout([
    ["Runtime", service.status],
    ["Image", service.image || "Not reported"],
    ["Endpoint", endpoint],
    ["Mounts", `${service.mounts} declared`],
    ["Health check", health],
    ["Enablement", service.disabled ? "Durably disabled" : "Enabled in manager intent"],
  ]);
  renderPorts(service);
  renderDependencies(service);
  renderCompactDiagnosis(service);
}

function renderPorts(service) {
  const ports = service.port_specs || [];
  if (!ports.length) {
    dom.portList.innerHTML = `<div class="port-line"><span>No editable published ports</span></div>`;
    return;
  }
  dom.portList.innerHTML = ports.map((port, index) => `<div class="port-line">
    <span><code>${escapeHtml(port.published)}</code> host → <code>${escapeHtml(port.target)}</code>/${escapeHtml(port.protocol || "tcp")}</span>
    <button class="button port-edit" type="button" data-port-index="${index}" ${state.activeOperation ? "disabled" : ""}>${icon("edit")}Edit</button>
  </div>`).join("");
}

function renderDependencies(service) {
  const dependencies = service.depends_on || [];
  dom.interlockState.textContent = service.dependencies_ready ? "[READY]" : "[WAIT]";
  dom.interlockState.className = service.dependencies_ready ? "ready" : "wait";
  if (!dependencies.length) {
    dom.dependencyList.innerHTML = `<div class="dependency-line"><span>No declared dependencies</span><span class="ready">[DIRECT]</span></div>`;
    return;
  }
  dom.dependencyList.innerHTML = dependencies.map((name) => {
    const condition = service.dependency_conditions?.[name] || "service_started";
    const status = service.dependency_status?.[name] || "unknown";
    const ready = status.includes("ready") || status.includes("running") || status.includes("healthy");
    return `<div class="dependency-line"><strong>${escapeHtml(name)}</strong><code>${escapeHtml(condition)}</code><span class="${ready ? "ready" : "wait"}">[${ready ? "READY" : "WAIT"}] ${escapeHtml(status)}</span></div>`;
  }).join("");
}

function renderCompactDiagnosis(service) {
  const diagnosis = state.diagnoses.get(service.service);
  dom.diagnosisCompact.hidden = !diagnosis;
  if (!diagnosis) return;
  if (!diagnosis.issues?.length) {
    dom.diagnosisCompactBody.innerHTML = `<p class="ready">[CLEAR] No known issue pattern found in the recent logs.</p>`;
    return;
  }
  const issue = diagnosis.issues[0];
  const fix = diagnosis.safe_fix
    ? `<button class="button" type="button" data-safe-fix="${escapeAttr(diagnosis.safe_fix.action)}">${escapeHtml(diagnosis.safe_fix.label)}</button>`
    : "";
  dom.diagnosisCompactBody.innerHTML = `<div class="issue-readout"><strong>[${escapeHtml(issue.type)}] ${escapeHtml(issue.summary)}</strong><span>${escapeHtml(issue.evidence)}</span></div>${fix}`;
}

function renderOperation() {
  const operation = state.activeOperation;
  dom.operationRail.classList.toggle("busy", Boolean(operation));
  const signal = dom.operationRail.querySelector(".operation-signal");
  const label = dom.operationRail.querySelector(".operation-label");
  if (operation) {
    signal.textContent = "[WORKING]";
    label.textContent = `${operation.label} · ${operation.source} · ${formatTime(operation.started_at)}`;
  } else {
    signal.textContent = "[IDLE]";
    label.textContent = "No protected operation is active";
  }
  const busy = Boolean(operation);
  dom.createBackup.disabled = busy;
  dom.applyIntegrations.disabled = busy || !(state.integrations?.pending?.length);
  document.querySelectorAll("[data-requires-idle]").forEach((control) => {
    control.disabled = busy;
  });
}

async function reviewServiceAction(action) {
  const service = selectedService();
  if (!service) return;
  try {
    setFooter(`Calculating ${action} scope for ${service.service}…`);
    const scope = await api(`/api/services/${encodeURIComponent(service.service)}/scope?action=${encodeURIComponent(action)}`);
    const actionCopy = {
      enable: "Dependencies start first; supported integrations reconcile afterward.",
      disable: "Dependents stop first; service-owned dependencies are disabled too.",
      restart: "The selected service is restarted in place.",
    }[action];
    const confirmed = await showDialog({
      signal: action === "disable" ? "[PROTECTED]" : "[CONFIRM]",
      title: `${capitalize(action)} ${service.service}?`,
      detail: `<p>${escapeHtml(actionCopy)}</p><div class="scope-readout"><strong>Exact scope</strong><br>${scope.scope.map(escapeHtml).join(" → ")}</div>`,
      confirmLabel: capitalize(action),
      danger: action === "disable",
    });
    if (!confirmed) return;
    const accepted = await api(`/api/services/${encodeURIComponent(service.service)}/action`, {
      method: "POST",
      body: JSON.stringify({ action }),
    });
    state.activeOperation = accepted.operation;
    renderOperation();
    renderWorkbench();
    toast(`${accepted.operation.label} accepted; progress is live below.`, "success");
  } catch (error) {
    toast(messageFrom(error), "error");
    setFooter(messageFrom(error), true);
  }
}

async function editPort(portIndex) {
  const service = selectedService();
  const current = service?.port_specs?.[portIndex];
  if (!service || !current) return;
  const result = await showDialog({
    signal: "[VALIDATE]",
    title: `Change ${service.service} host port`,
    detail: `<p>Compose is backed up and validated before this enabled service is recreated. Integrations are recalculated after the change.</p><div class="scope-readout">Current mapping: <strong>${escapeHtml(current.published)}</strong> host → ${escapeHtml(current.target)}/${escapeHtml(current.protocol || "tcp")}</div>`,
    confirmLabel: "Validate & change",
    inputLabel: "New host port",
    inputValue: current.published,
    validate(value) {
      return /^\d{1,5}$/.test(value) && Number(value) >= 1 && Number(value) <= 65535
        ? null
        : "Enter a port between 1 and 65535.";
    },
  });
  if (result === false) return;
  try {
    const accepted = await api(`/api/services/${encodeURIComponent(service.service)}/port`, {
      method: "POST",
      body: JSON.stringify({ port_index: portIndex, port: Number(result) }),
    });
    state.activeOperation = accepted.operation;
    renderOperation();
    toast("Port change accepted; validation and recreation are visible in the watch log.", "success");
  } catch (error) {
    toast(messageFrom(error), "error");
  }
}

async function diagnoseSelected() {
  const service = selectedService();
  if (!service) return;
  dom.actionDiagnose.disabled = true;
  setFooter(`Inspecting recent ${service.service} logs…`);
  try {
    const diagnosis = await api(`/api/diagnostics/${encodeURIComponent(service.service)}`, { method: "POST" });
    state.diagnoses.set(service.service, diagnosis);
    renderWorkbench();
    renderDiagnostics();
    openView("diagnostics");
    setFooter(`Log inspection complete · ${diagnosis.issues.length} issue(s) in ${service.service}`);
  } catch (error) {
    toast(messageFrom(error), "error");
    setFooter(messageFrom(error), true);
  } finally {
    dom.actionDiagnose.disabled = false;
  }
}

async function scanFleet() {
  dom.scanFleet.disabled = true;
  setFooter("Scanning recent logs across every created service…");
  try {
    const diagnoses = await api("/api/diagnostics", { method: "POST" });
    state.diagnoses = new Map(Object.entries(diagnoses));
    renderDiagnostics();
    renderWorkbench();
    const issues = [...state.diagnoses.values()].reduce((sum, item) => sum + item.issues.length, 0);
    setFooter(`Fleet log scan complete · ${issues} issue(s)`);
  } catch (error) {
    toast(messageFrom(error), "error");
  } finally {
    dom.scanFleet.disabled = false;
  }
}

function renderDiagnostics() {
  if (!state.diagnoses.size) {
    dom.diagnosticSurface.innerHTML = `<p class="empty-state">Select a service in Fleet and inspect its logs, or scan every created container.</p>`;
    return;
  }
  const selected = state.selectedService && state.diagnoses.has(state.selectedService)
    ? [[state.selectedService, state.diagnoses.get(state.selectedService)]]
    : [...state.diagnoses.entries()];
  dom.diagnosticSurface.innerHTML = selected.map(([name, diagnosis]) => diagnosticMarkup(name, diagnosis)).join("");
  dom.diagnosticSurface.querySelectorAll("[data-diagnostic-service]").forEach((button) => {
    button.addEventListener("click", () => {
      selectService(button.dataset.diagnosticService, false);
      openView("fleet");
    });
  });
  dom.diagnosticSurface.querySelectorAll("[data-diagnostic-fix]").forEach((button) => {
    button.addEventListener("click", () => {
      selectService(button.dataset.service, false);
      reviewServiceAction(button.dataset.diagnosticFix);
    });
  });
}

function diagnosticMarkup(name, diagnosis) {
  const issues = diagnosis.issues?.length
    ? diagnosis.issues.map((issue) => `<div class="diagnostic-item"><strong class="fault">[${escapeHtml(issue.type)}] ${escapeHtml(issue.summary)}</strong><p>${escapeHtml(issue.manual)}</p><p class="evidence">${escapeHtml(issue.evidence)}</p></div>`).join("")
    : `<div class="diagnostic-item"><strong class="ready">[CLEAR]</strong><p>No known issue pattern found in the recent logs.</p><p class="evidence">${escapeHtml(diagnosis.manual || "No manual action suggested.")}</p></div>`;
  const fix = diagnosis.safe_fix
    ? `<button class="button" type="button" data-diagnostic-fix="${escapeAttr(diagnosis.safe_fix.action)}" data-service="${escapeAttr(name)}">${escapeHtml(diagnosis.safe_fix.label)}</button>`
    : "";
  const logs = diagnosis.logs?.length ? diagnosis.logs.join("\n") : "No recent log output.";
  return `<article class="diagnostic-service"><div class="diagnostic-service-heading"><div><span class="${diagnosis.issues?.length ? "fault" : "ready"}">[${diagnosis.issues?.length ? "FLAGGED" : "CLEAR"}]</span><h2>${escapeHtml(name)}</h2></div><div class="heading-actions"><button class="text-button" type="button" data-diagnostic-service="${escapeAttr(name)}">Open service</button>${fix}</div></div><div class="diagnostic-group">${issues}</div><pre class="log-tail">${escapeHtml(logs)}</pre></article>`;
}

async function loadIntegrations() {
  dom.integrationLedger.innerHTML = `<p class="empty-state loading-copy">Recalculating supported integration state…</p>`;
  try {
    state.integrations = await api("/api/integrations");
    renderIntegrations();
  } catch (error) {
    dom.integrationLedger.innerHTML = `<p class="empty-state fault">${escapeHtml(messageFrom(error))}</p>`;
  }
}

function renderIntegrations() {
  const plan = state.integrations;
  if (!plan) return;
  dom.applyIntegrations.disabled = !plan.pending.length || Boolean(state.activeOperation);
  const pending = plan.pending.length
    ? `<section class="integration-group"><header><h2>Pending changes</h2><span class="wait">[${plan.pending.length} PENDING]</span></header>${plan.pending.map((item) => `<article class="integration-item"><strong>${escapeHtml(item.target)}</strong><p>${escapeHtml(item.summary)}</p><p class="manual">${escapeHtml(item.manual)}</p></article>`).join("")}</section>`
    : `<section class="integration-group"><header><h2>Pending changes</h2><span class="ready">[ALIGNED]</span></header><p class="empty-state">No supported integration change is pending.</p></section>`;
  const blocked = plan.blocked.length
    ? `<section class="integration-group"><header><h2>Manual prerequisites</h2><span class="wait">[${plan.blocked.length} BLOCKED]</span></header>${plan.blocked.map((item) => `<article class="integration-item"><strong>${escapeHtml(item.target)}</strong><p>${escapeHtml(item.reason)}</p><p class="manual">Complete this prerequisite in the named service, then recalculate.</p></article>`).join("")}</section>`
    : `<section class="integration-group"><header><h2>Manual prerequisites</h2><span class="ready">[CLEAR]</span></header><p class="empty-state">No supported integration prerequisite is blocked.</p></section>`;
  dom.integrationLedger.innerHTML = pending + blocked;
}

async function reviewIntegrations() {
  if (!state.integrations) await loadIntegrations();
  const pending = state.integrations?.pending || [];
  if (!pending.length) return;
  const confirmed = await showDialog({
    signal: "[REVIEWED PLAN]",
    title: `Apply ${pending.length} integration change${pending.length === 1 ? "" : "s"}?`,
    detail: `<p>The server recalculates the plan immediately before applying it. A private integration backup is written first.</p><div class="scope-readout">${pending.map((item) => `${escapeHtml(item.target)} · ${escapeHtml(item.action)}`).join("<br>")}</div>`,
    confirmLabel: "Apply plan",
  });
  if (!confirmed) return;
  try {
    const accepted = await api("/api/integrations", { method: "POST" });
    state.activeOperation = accepted.operation;
    renderOperation();
    renderIntegrations();
    toast("Integration plan accepted; persistence checks are live in the activity channel.", "success");
  } catch (error) {
    toast(messageFrom(error), "error");
  }
}

async function loadBackups() {
  try {
    state.backups = await api("/api/backups");
    renderBackups();
  } catch (error) {
    dom.backupList.innerHTML = `<p class="empty-state fault">${escapeHtml(messageFrom(error))}</p>`;
  }
}

function renderBackups() {
  const data = state.backups;
  if (!data) return;
  const scheduleState = data.last_error ? "fault" : data.overdue ? "wait" : "ready";
  const scheduleWord = data.last_error ? "FAILED" : data.overdue ? "OVERDUE" : "ON SCHEDULE";
  const lastSuccess = data.last_success
    ? `${formatDateTime(data.last_success.created_at)} · ${data.last_success.file_name}`
    : "No successful bundle yet";
  const lastAttempt = data.last_attempt_at ? formatDateTime(data.last_attempt_at) : "No attempt in this session";
  const nextDue = data.next_due_at ? formatDateTime(data.next_due_at) : "Due now";
  dom.backupPolicy.innerHTML = `<span class="${scheduleState}"><strong>[${scheduleWord}]</strong></span><span><strong>Every</strong> ${escapeHtml(formatDuration(data.interval_seconds))}</span><span><strong>Retention</strong> ${data.retention} bundles</span><span><strong>Last success</strong> ${escapeHtml(lastSuccess)}</span><span><strong>Last attempt</strong> ${escapeHtml(lastAttempt)}</span><span><strong>Next due</strong> ${escapeHtml(nextDue)}</span><span><strong>Storage</strong> ${escapeHtml(data.directory)}</span><span class="wait"><strong>[SENSITIVE]</strong> private configuration</span>${data.last_error ? `<span class="fault"><strong>Recovery</strong> ${escapeHtml(data.last_error)} ${escapeHtml(data.recovery || "Check Activity and retry a manual bundle.")}</span>` : ""}`;
  if (!data.records.length) {
    dom.backupList.innerHTML = `<p class="empty-state">No retained setup bundle exists yet. Create one now; the scheduler also captures overdue state after startup.</p>`;
    return;
  }
  dom.backupList.innerHTML = data.records.map((record) => `<article class="bundle-row">
    <div class="bundle-name"><strong>${escapeHtml(record.file_name)}</strong><span>${escapeHtml(formatDateTime(record.created_at))} · ${escapeHtml(record.kind)} · ${escapeHtml(record.consistency)}</span></div>
    <div class="bundle-meta">${formatBytes(record.size_bytes)}<br>${record.mount_count} state mounts · ${record.service_count} services</div>
    <div class="bundle-actions"><button class="button" type="button" data-backup-action="download" data-backup-id="${escapeAttr(record.id)}">${icon("download")}Download</button><button class="button" type="button" data-backup-action="inspect" data-backup-id="${escapeAttr(record.id)}">Inspect restore</button><button class="button button-danger" type="button" data-requires-idle data-backup-action="delete" data-backup-id="${escapeAttr(record.id)}" ${state.activeOperation ? "disabled" : ""}>${icon("trash")}Delete</button></div>
  </article>`).join("");
}

async function createBackup() {
  const confirmed = await showDialog({
    signal: "[PRIVATE EXPORT]",
    title: "Create an online setup bundle?",
    detail: `<p>The bundle includes Compose, environment, durable enablement intent, configuration bind mounts, and Docker named volumes. Multi-terabyte media, downloads, caches, logs, devices, generated assets, and nested backups stay excluded.</p><p class="wait">Online capture avoids service interruption, but a database file can reflect an in-flight write.</p>`,
    confirmLabel: "Create bundle",
  });
  if (!confirmed) return;
  try {
    const accepted = await api("/api/backups", { method: "POST" });
    state.activeOperation = accepted.operation;
    renderOperation();
    toast("Setup bundle started; archive progress is live in the watch log.", "success");
  } catch (error) {
    toast(messageFrom(error), "error");
  }
}

async function handleBackupAction(event) {
  const button = event.target.closest("[data-backup-action]");
  if (!button) return;
  const id = button.dataset.backupId;
  const action = button.dataset.backupAction;
  if (action === "download") await downloadBackup(id, button);
  if (action === "inspect") await inspectRetainedBackup(id);
  if (action === "delete") await deleteBackup(id);
}

async function downloadBackup(id, button) {
  button.disabled = true;
  try {
    const ticket = await api(`/api/backups/${encodeURIComponent(id)}/download-ticket`, { method: "POST" });
    const anchor = document.createElement("a");
    anchor.href = ticket.url;
    anchor.rel = "noopener";
    document.body.append(anchor);
    anchor.click();
    anchor.remove();
    toast("Private bundle download authorized for this one request.", "success");
  } catch (error) {
    toast(messageFrom(error), "error");
  } finally {
    button.disabled = false;
  }
}

async function inspectRetainedBackup(id) {
  openView("backups");
  dom.restoreInspection.hidden = false;
  dom.restoreInspection.innerHTML = `<p class="loading-copy">Inspecting retained bundle…</p>`;
  try {
    state.restoreInspection = await api(`/api/backups/${encodeURIComponent(id)}/inspect`, { method: "POST" });
    renderRestoreInspection();
    dom.restoreInspection.scrollIntoView({ behavior: reducedMotion() ? "auto" : "smooth", block: "start" });
  } catch (error) {
    dom.restoreInspection.innerHTML = `<p class="fault">${escapeHtml(messageFrom(error))}</p>`;
  }
}

async function deleteBackup(id) {
  const confirmed = await showDialog({
    signal: "[DELETE RECOVERY]",
    title: `Delete setup bundle ${id}?`,
    detail: `<p>This removes the retained archive and its catalog record. It cannot be downloaded or used for rollback afterward.</p>`,
    confirmLabel: "Delete bundle",
    danger: true,
    phrase: id,
    inputLabel: `Type ${id} to delete`,
  });
  if (!confirmed) return;
  try {
    await api(`/api/backups/${encodeURIComponent(id)}`, {
      method: "DELETE",
      body: JSON.stringify({ confirmation: id }),
    });
    toast(`Deleted setup bundle ${id}.`, "success");
    await loadBackups();
  } catch (error) {
    toast(messageFrom(error), "error");
  }
}

async function inspectUpload(event) {
  event.preventDefault();
  const file = dom.bundleFile.files?.[0];
  if (!file) {
    toast("Choose a setup .tar.gz file first.", "error");
    return;
  }
  const form = new FormData();
  form.append("bundle", file, file.name);
  await runInspectionRequest("/api/setup/inspect/upload", { method: "POST", body: form });
}

async function inspectUrl(event) {
  event.preventDefault();
  const url = dom.bundleUrl.value.trim();
  if (!url) return;
  await runInspectionRequest("/api/setup/inspect/url", {
    method: "POST",
    body: JSON.stringify({ url }),
  });
}

async function runInspectionRequest(path, options) {
  dom.restoreInspection.hidden = false;
  dom.restoreInspection.innerHTML = `<p class="loading-copy">Transferring and validating the setup bundle…</p>`;
  try {
    state.restoreInspection = await api(path, options);
    renderRestoreInspection();
  } catch (error) {
    state.restoreInspection = null;
    dom.restoreInspection.innerHTML = `<p class="fault">${escapeHtml(messageFrom(error))}</p>`;
  }
}

function renderRestoreInspection() {
  const result = state.restoreInspection;
  if (!result) {
    dom.restoreInspection.hidden = true;
    return;
  }
  const inspection = result.inspection;
  const manifest = inspection.manifest;
  dom.restoreInspection.hidden = false;
  dom.restoreInspection.innerHTML = `<span class="inspection-signal">[INSPECTED · NOT APPLIED]</span><h3>${escapeHtml(inspection.file_name)}</h3>
    <dl class="inspection-readout">${readout([
      ["Created", formatDateTime(manifest.created_at)],
      ["Source host", manifest.host],
      ["Application", `${manifest.application} ${manifest.application_version}`],
      ["Consistency", manifest.consistency],
      ["Compressed", formatBytes(inspection.compressed_size)],
      ["Services", String(manifest.services.length)],
      ["State mounts", String(manifest.mounts.filter((item) => item.present).length)],
      ["Excluded mounts", String(manifest.excluded_mounts.length)],
      ["Excluded paths", String((manifest.excluded_paths || []).length)],
      ["SHA-256", inspection.sha256],
    ])}</dl>
    <ul class="warning-list">${inspection.warnings.map((warning) => `<li>${escapeHtml(warning)}</li>`).join("")}</ul>
    ${(manifest.excluded_paths || []).length ? `<div class="scope-readout"><strong>Transient paths omitted</strong><br>${manifest.excluded_paths.map((item) => `${escapeHtml(item.mount_id)} · ${escapeHtml(item.path)} · ${escapeHtml(item.reason)}`).join("<br>")}</div>` : ""}
    <div class="restore-authorize"><p><strong class="fault">Restore stops the current stack.</strong> A pre-restore recovery bundle is created first. Files are staged, Compose is validated, enabled services start in dependency order, and failure triggers rollback.</p><button class="button button-danger" type="button" data-requires-idle data-restore-now ${state.activeOperation ? "disabled" : ""}>Review stopped-stack restore</button></div>`;
}

async function restoreInspectedBundle() {
  const result = state.restoreInspection;
  if (!result) return;
  const manifest = result.inspection.manifest;
  const confirmed = await showDialog({
    signal: "[STOPPED-STACK RESTORE]",
    title: `Restore ${manifest.services.length} service definitions and state?`,
    detail: `<p>The current Compose stack will stop. A pre-restore recovery bundle is created, ${manifest.mounts.filter((item) => item.present).length} configuration or named-volume mounts are restored, Compose is validated, and only enabled services start again.</p><div class="scope-readout">Source host: ${escapeHtml(manifest.host)}<br>Created: ${escapeHtml(formatDateTime(manifest.created_at))}<br>SHA-256: ${escapeHtml(result.inspection.sha256)}</div>`,
    confirmLabel: "Stop & restore",
    danger: true,
    phrase: "RESTORE",
    inputLabel: "Type RESTORE to authorize",
  });
  if (!confirmed) return;
  try {
    const accepted = await api("/api/setup/restore", {
      method: "POST",
      body: JSON.stringify({ ticket: result.ticket, confirmation: "RESTORE" }),
    });
    state.restoreInspection = null;
    dom.restoreInspection.hidden = true;
    state.activeOperation = accepted.operation;
    renderOperation();
    toast("Restore accepted. Keep this page open and follow the recovery sequence in Activity.", "success");
    openView("activity");
  } catch (error) {
    toast(messageFrom(error), "error");
  }
}

function addActivity(event) {
  if (!event?.id || state.activity.some((item) => item.id === event.id)) return;
  state.activity.push(event);
  if (state.activity.length > MAX_ACTIVITY) state.activity.splice(0, state.activity.length - MAX_ACTIVITY);
  renderActivity();
  if (event.message?.startsWith("DONE") || event.message?.startsWith("FAILED") || event.message?.startsWith("ABORTED")) {
    if (state.activeOperation?.id === event.operation_id) {
      state.activeOperation = null;
      renderOperation();
      renderWorkbench();
    }
    if (event.level === "success") toast(event.message, "success");
    if (event.level === "error") toast(event.message, "error");
    refreshState();
    if (event.source === "backup" || state.view === "backups") loadBackups();
    if (state.view === "integrations") loadIntegrations();
  }
}

function renderActivity() {
  const markup = state.activity.length
    ? state.activity.map(activityMarkup).join("")
    : `<li class="empty-log">Waiting for shared activity…</li>`;
  const stripWasAtEnd = nearScrollEnd(dom.activityStripList);
  const fullWasAtEnd = nearScrollEnd(dom.activityFull);
  dom.activityStripList.innerHTML = state.activity.length
    ? state.activity.slice(-8).map(activityMarkup).join("")
    : markup;
  dom.activityFull.innerHTML = markup;
  if (stripWasAtEnd) dom.activityStripList.scrollTop = dom.activityStripList.scrollHeight;
  if (fullWasAtEnd) dom.activityFull.scrollTop = dom.activityFull.scrollHeight;
}

function activityMarkup(event) {
  const level = ["success", "warning", "error"].includes(event.level) ? event.level : "info";
  return `<li class="activity-line ${level}"><span class="at">${escapeHtml(formatTime(event.at))}</span><span class="source">${escapeHtml((event.source || "watch").toUpperCase())}</span><span class="message">${escapeHtml(event.message || "")}</span></li>`;
}

function openView(view) {
  if (!view) return;
  state.view = view;
  dom.tabs.forEach((button) => {
    const active = button.dataset.view === view;
    button.classList.toggle("active", active);
    if (active) button.setAttribute("aria-current", "page");
    else button.removeAttribute("aria-current");
  });
  dom.views.forEach((panel) => {
    const active = panel.dataset.viewPanel === view;
    panel.hidden = !active;
    panel.classList.toggle("active", active);
  });
  if (view === "integrations" && !state.integrations) loadIntegrations();
  if (view === "backups") loadBackups();
  if (view === "diagnostics") renderDiagnostics();
  if (view === "activity") renderActivity();
  document.querySelector(`[data-view-panel="${CSS.escape(view)}"]`)?.querySelector("h1")?.focus?.({ preventScroll: true });
}

function selectService(name, scrollWorkbench) {
  if (!serviceByName(name)) return;
  state.selectedService = name;
  renderFleet();
  renderWorkbench();
  renderDiagnostics();
  const row = dom.serviceRows.querySelector(`[data-service="${CSS.escape(name)}"]`);
  row?.focus({ preventScroll: true });
  if (scrollWorkbench && window.matchMedia("(max-width: 860px)").matches) {
    document.getElementById("service-workbench")?.scrollIntoView({ behavior: reducedMotion() ? "auto" : "smooth", block: "start" });
  }
}

function handleGlobalKey(event) {
  if (!dom.authGate.hidden || dom.confirmDialog.open) return;
  const tag = event.target?.tagName?.toLowerCase();
  const editing = ["input", "textarea", "select"].includes(tag) || event.target?.isContentEditable;
  if (!editing && /^[1-5]$/.test(event.key)) {
    event.preventDefault();
    openView(["fleet", "integrations", "diagnostics", "backups", "activity"][Number(event.key) - 1]);
    return;
  }
  if (!editing && event.key === "/") {
    event.preventDefault();
    openView("fleet");
    dom.serviceSearch.focus();
    return;
  }
  if (!editing && state.view === "fleet" && ["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) {
    event.preventDefault();
    const services = filteredServices();
    if (!services.length) return;
    let index = services.findIndex((item) => item.service === state.selectedService);
    if (event.key === "ArrowDown") index = (index + 1 + services.length) % services.length;
    if (event.key === "ArrowUp") index = (index - 1 + services.length) % services.length;
    if (event.key === "Home") index = 0;
    if (event.key === "End") index = services.length - 1;
    selectService(services[Math.max(0, index)].service, false);
  }
}

function showDialog(options) {
  state.dialogOptions = options;
  dom.confirmSignal.textContent = options.signal || "[CONFIRM]";
  dom.confirmSignal.className = `dialog-signal ${options.danger ? "fault" : "wait"}`;
  dom.confirmTitle.textContent = options.title || "Confirm operation";
  dom.confirmDetail.innerHTML = options.detail || "";
  dom.confirmSubmit.textContent = options.confirmLabel || "Confirm";
  dom.confirmSubmit.className = `button ${options.danger ? "button-danger" : "button-primary"}`;
  const needsInput = Boolean(options.phrase || options.inputLabel || options.validate);
  dom.confirmPhraseField.hidden = !needsInput;
  dom.confirmPhraseLabel.textContent = options.inputLabel || "Confirmation phrase";
  dom.confirmPhrase.value = options.inputValue || "";
  dom.confirmPhrase.removeAttribute("aria-invalid");
  dom.confirmDialog.showModal();
  if (needsInput) {
    dom.confirmPhrase.focus();
    dom.confirmPhrase.select();
  } else {
    dom.confirmSubmit.focus();
  }
  return new Promise((resolve) => {
    dom.confirmDialog.addEventListener("close", () => {
      const confirmed = dom.confirmDialog.returnValue === "confirm";
      const value = dom.confirmPhrase.value;
      state.dialogOptions = null;
      if (!confirmed) resolve(false);
      else if (needsInput && !options.phrase) resolve(value);
      else resolve(true);
    }, { once: true });
  });
}

function validateDialogSubmission(event) {
  const submitter = event.submitter;
  if (submitter?.value !== "confirm") return;
  const options = state.dialogOptions;
  if (!options) return;
  const value = dom.confirmPhrase.value;
  let error = null;
  if (options.phrase && value !== options.phrase) error = `Type ${options.phrase} exactly to continue.`;
  if (!error && options.validate) error = options.validate(value);
  if (error) {
    event.preventDefault();
    dom.confirmPhrase.setAttribute("aria-invalid", "true");
    toast(error, "error");
  }
}

function filteredServices() {
  const services = state.snapshot?.services || [];
  if (!state.filter) return services;
  return services.filter((service) => [service.service, service.purpose, service.status]
    .some((value) => String(value || "").toLowerCase().includes(state.filter)));
}

function selectedService() {
  return serviceByName(state.selectedService);
}

function serviceByName(name) {
  return state.snapshot?.services?.find((service) => service.service === name) || null;
}

function serviceState(service) {
  if (service.disabled) return "off";
  const status = String(service.status || "").toLowerCase();
  if (service.issue_count || /unhealthy|restart|exited|dead|error|failed/.test(status)) return "fault";
  if (/starting|created|paused|settling/.test(status)) return "wait";
  if (service.running || /running|healthy/.test(status)) return "ready";
  return "off";
}

function stateWord(kind) {
  return { ready: "READY", wait: "WAIT", fault: "FAULT", off: "OFF" }[kind] || "UNKNOWN";
}

function tally(kind, count, label) {
  return `<span class="tally-item ${kind}"><strong>${count}</strong><span>[${label}]</span></span>`;
}

function readout(items) {
  return items.map(([label, value]) => `<dt>${escapeHtml(label)}</dt><dd>${escapeHtml(value ?? "—")}</dd>`).join("");
}

function icon(name) {
  return `<svg aria-hidden="true"><use href="#icon-${escapeAttr(name)}"></use></svg>`;
}

function setConnection(label, kind) {
  dom.connectionState.textContent = `[${String(label).toUpperCase()}]`;
  dom.connectionState.className = `connection-state ${kind}`;
}

function updateSampleAge() {
  if (!state.lastSampleAt || Number.isNaN(state.lastSampleAt.getTime())) {
    dom.sampleAge.textContent = "—";
    return;
  }
  const seconds = Math.max(0, Math.floor((Date.now() - state.lastSampleAt.getTime()) / 1000));
  dom.sampleAge.textContent = `sample ${seconds}s ago`;
  if (seconds > Math.max(15, (state.meta?.refresh_milliseconds || 5000) / 1000 * 3)) setConnection("stale", "wait");
}

function setFooter(message, error = false) {
  dom.footerStatus.textContent = message;
  dom.footerStatus.className = error ? "fault" : "";
}

function toast(message, kind = "info") {
  const item = document.createElement("div");
  item.className = `toast ${kind}`;
  item.textContent = message;
  item.setAttribute("role", kind === "error" ? "alert" : "status");
  dom.toastRack.append(item);
  window.setTimeout(() => item.remove(), kind === "error" ? 9000 : 5000);
}

function safeUrl(value) {
  if (!value) return null;
  try {
    const url = new URL(value, window.location.href);
    return ["http:", "https:"].includes(url.protocol) ? url.href : null;
  } catch {
    return null;
  }
}

function formatBytes(value) {
  if (value == null || Number.isNaN(Number(value))) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let amount = Number(value);
  let index = 0;
  while (amount >= 1024 && index < units.length - 1) {
    amount /= 1024;
    index += 1;
  }
  return index === 0 ? `${amount} ${units[index]}` : `${amount.toFixed(1)} ${units[index]}`;
}

function formatDuration(seconds) {
  if (seconds == null) return "—";
  const value = Number(seconds);
  if (value >= 86400) return `${Math.floor(value / 86400)}d ${Math.floor((value % 86400) / 3600)}h`;
  if (value >= 3600) return `${Math.floor(value / 3600)}h ${Math.floor((value % 3600) / 60)}m`;
  if (value >= 60) return `${Math.floor(value / 60)}m`;
  return `${value}s`;
}

function formatTime(value) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "—" : new Intl.DateTimeFormat([], { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false }).format(date);
}

function formatDateTime(value) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? String(value || "—") : new Intl.DateTimeFormat([], { dateStyle: "medium", timeStyle: "short" }).format(date);
}

function capitalize(value) {
  return value ? value[0].toUpperCase() + value.slice(1) : "";
}

function nearScrollEnd(element) {
  return element.scrollHeight - element.scrollTop - element.clientHeight < 40;
}

function reducedMotion() {
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>'"]/g, (character) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;",
  })[character]);
}

function escapeAttr(value) {
  return escapeHtml(value).replace(/`/g, "&#96;");
}

function messageFrom(error) {
  return error instanceof Error ? error.message : String(error);
}

function toCamel(value) {
  return value.replace(/-([a-z])/g, (_, character) => character.toUpperCase());
}

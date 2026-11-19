"use strict";

const VIEWS = ["overview", "tokens", "approvals", "audit", "sources"];
const TOOLS = [
  "list_tables",
  "describe_table",
  "explain_query",
  "query_sql",
  "mutate_sql",
  "execute_ddl",
  "create_database",
];
const TOOL_LABELS = {
  list_tables: "list_tables（列出表）",
  describe_table: "describe_table（查看表结构）",
  explain_query: "explain_query（查看执行计划）",
  query_sql: "query_sql（执行查询）",
  mutate_sql: "mutate_sql（执行写入）",
  execute_ddl: "execute_ddl（结构变更）",
  create_database: "create_database（建库）",
};
const TOOL_PURPOSE = {
  list_tables: "query",
  describe_table: "query",
  explain_query: "query",
  query_sql: "query",
  mutate_sql: "mutate",
  execute_ddl: "schema",
  create_database: "provision",
};
const PURPOSE_LABELS = {
  query: "查询",
  mutate: "写入",
  schema: "结构变更",
  provision: "建库",
};
const ENVIRONMENT_LABELS = { production: "生产", nonproduction: "非生产" };
const STATUS_LABELS = { active: "启用中", archived: "已归档" };
const POLICY_LABELS = { direct: "直接执行", approval: "逐次审批" };
const DECISION_LABELS = {
  allowed: "允许",
  rejected: "拒绝",
  error: "错误",
  unknown: "未知",
  expired: "已过期",
  pending: "待批",
  approved: "已批准",
  denied: "已拒绝",
  consumed: "已消费",
};

const NAV = [
  { id: "overview", label: "概览", icon: "overview" },
  { id: "tokens", label: "访问令牌", icon: "key" },
  { id: "approvals", label: "待批请求", icon: "approval" },
  { id: "audit", label: "审计日志", icon: "audit" },
  { id: "sources", label: "数据源", icon: "source" },
];

const ICONS = {
  overview:
    '<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>',
  key: '<circle cx="7.5" cy="15.5" r="4.5"/><path d="M11 12 20 3"/><path d="M17 6l3 3"/><path d="M14 9l2 2"/>',
  approval: '<circle cx="12" cy="12" r="9"/><path d="M8 12l2.5 2.5L16 9"/>',
  audit: '<path d="M4 5h16"/><path d="M4 12h16"/><path d="M4 19h10"/>',
  source:
    '<ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v6c0 1.7 3.6 3 8 3s8-1.3 8-3V5"/><path d="M4 11v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6"/>',
  edit: '<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/>',
  trash:
    '<path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="M19 6l-1 14H6L5 6"/><path d="M10 11v5"/><path d="M14 11v5"/>',
  copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1"/>',
  plus: '<path d="M12 5v14"/><path d="M5 12h14"/>',
  refresh: '<path d="M20 11a8 8 0 1 0-2.3 5.7"/><path d="M20 4v7h-7"/>',
  logout: '<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="M16 17l5-5-5-5"/><path d="M21 12H9"/>',
  alert: '<circle cx="12" cy="12" r="9"/><path d="M12 8v4"/><path d="M12 16h.01"/>',
  chevronLeft: '<path d="M15 18l-6-6 6-6"/>',
  chevronRight: '<path d="M9 18l6-6-6-6"/>',
};

const state = {
  session: false,
  csrf: "",
  view: "overview",
  sources: [],
  tokens: [],
  approvals: [],
  overview: null,
  reservations: [],
  audit: { items: [], total: 0 },
  overviewAudit: { items: [], total: 0 },
  auditFilters: { agent: "", source: "", decision: "" },
  auditLimit: 50,
  auditOffset: 0,
  showRevoked: false,
  pollTimer: null,
  loginChallenge: "",
};

let modalDismissible = true;

// ---------- DOM helpers ----------

function appendChildren(element, children) {
  for (const child of children) {
    if (child === null || child === undefined || child === false || child === "") continue;
    if (Array.isArray(child)) appendChildren(element, child);
    else if (child instanceof Node) element.appendChild(child);
    else element.appendChild(document.createTextNode(String(child)));
  }
}

function h(tag, props, ...children) {
  const element = document.createElement(tag);
  if (props) {
    for (const [key, value] of Object.entries(props)) {
      if (value === null || value === undefined || value === false) continue;
      if (key === "class") element.className = value;
      else if (key === "text") element.textContent = value;
      else if (key === "value") element.value = value;
      else if (key === "checked" || key === "disabled" || key === "selected") {
        element[key] = Boolean(value);
      } else if (key.startsWith("on") && typeof value === "function") {
        element.addEventListener(key.slice(2).toLowerCase(), value);
      } else {
        element.setAttribute(key, value === true ? "" : String(value));
      }
    }
  }
  appendChildren(element, children);
  return element;
}

function icon(name, size = 18) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", String(size));
  svg.setAttribute("height", String(size));
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1.7");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute("aria-hidden", "true");
  // Static icon path data only; no user or server data is interpolated here.
  svg.innerHTML = ICONS[name] || "";
  return svg;
}

// ---------- Formatting ----------

function pad2(value) {
  return String(value).padStart(2, "0");
}

function formatDay(ms) {
  if (!ms) return "—";
  const date = new Date(ms);
  return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())}`;
}

function formatDateTime(ms) {
  if (!ms) return "—";
  const date = new Date(ms);
  return `${formatDay(ms)} ${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
}

function formatIso(ms) {
  if (!ms) return "—";
  const date = new Date(ms);
  return `${formatDateTime(ms)}:${pad2(date.getSeconds())}`;
}

function remaining(expiresAt) {
  const seconds = Math.max(0, Math.ceil((expiresAt - Date.now()) / 1000));
  if (seconds <= 0) return "已过期";
  if (seconds < 60) return `${seconds}s`;
  return `${Math.ceil(seconds / 60)}m`;
}

function sqlPreview(sql) {
  const text = (sql || "").replace(/\s+/g, " ").trim();
  if (!text) return "—";
  return text.length > 90 ? `${text.slice(0, 90)}…` : text;
}

function shortId(value, keep = 10) {
  const text = String(value || "");
  if (text.length <= keep * 2 + 1) return text;
  return `${text.slice(0, keep)}…${text.slice(-4)}`;
}

function formattedJson(text) {
  if (!text) return "—";
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return String(text);
  }
}

function environmentLabel(value) {
  return ENVIRONMENT_LABELS[value] || value;
}

function statusLabel(value) {
  return STATUS_LABELS[value] || value;
}

function policyLabel(value) {
  return POLICY_LABELS[value] || value;
}

function purposeLabel(value) {
  return PURPOSE_LABELS[value] || value;
}

function decisionLabel(value) {
  return DECISION_LABELS[value] || value;
}

function sourceLabel(source) {
  return `${source.id} · ${environmentLabel(source.environment)}`;
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

// ---------- API ----------

async function api(path, options = {}) {
  const method = options.method || "GET";
  const headers = { ...(options.headers || {}) };
  if (options.body !== undefined) headers["content-type"] = "application/json";
  if (method !== "GET" && method !== "HEAD") {
    headers["x-mcpdb-csrf"] = options.csrf || state.csrf || "";
  }

  const response = await fetch(path, {
    method,
    credentials: "same-origin",
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });

  const text = await response.text();
  let data = {};
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = { error: text };
    }
  }

  if (
    response.status === 401 &&
    path !== "/admin/api/login" &&
    path !== "/admin/api/session" &&
    path !== "/admin/api/login-challenge"
  ) {
    await showLogin(true);
    throw new Error("登录已失效，请重新登录");
  }

  if (!response.ok) {
    const error = new Error(data.error || `HTTP ${response.status}`);
    error.status = response.status;
    throw error;
  }
  return data;
}

function pageLoginChallenge() {
  const meta = document.querySelector('meta[name="mcpdb-login-challenge"]');
  const value = meta ? meta.getAttribute("content") || "" : "";
  return value === "__MCPDB_LOGIN_CHALLENGE__" ? "" : value;
}

async function freshLoginChallenge() {
  const data = await api("/admin/api/login-challenge");
  state.loginChallenge = data.challenge || "";
  return state.loginChallenge;
}

async function loadSources() {
  const data = await api("/admin/api/sources");
  state.sources = data.sources || [];
}

async function loadTokens() {
  const data = await api("/admin/api/tokens");
  state.tokens = data.tokens || [];
}

async function loadApprovals() {
  const data = await api("/admin/api/approvals");
  state.approvals = data.approvals || [];
}

async function fetchAudit({ agent = "", source = "", decision = "", limit = 50, offset = 0 } = {}) {
  const params = new URLSearchParams();
  if (agent) params.set("agent", agent);
  if (source) params.set("source", source);
  if (decision) params.set("decision", decision);
  params.set("limit", String(limit));
  params.set("offset", String(offset));
  return api(`/admin/api/audit?${params.toString()}`);
}

async function loadAudit() {
  state.audit = await fetchAudit({
    ...state.auditFilters,
    limit: state.auditLimit,
    offset: state.auditOffset,
  });
}

// ---------- Boot and navigation ----------

function currentView() {
  const view = location.hash.replace(/^#\/?/, "");
  return VIEWS.includes(view) ? view : "overview";
}

async function boot() {
  let data = null;
  try {
    data = await api("/admin/api/session");
  } catch {
    data = null;
  }
  if (!data || !data.authenticated) {
    await showLogin(false);
    return;
  }
  state.session = true;
  state.csrf = data.csrf || "";
  state.loginChallenge = pageLoginChallenge();
  state.view = currentView();
  renderShell();
  await refreshView();
}

async function enterApp() {
  state.session = true;
  state.view = currentView();
  if (location.pathname === "/admin/login") {
    history.replaceState(null, "", `/admin#/${state.view}`);
  }
  renderShell();
  await refreshView();
}

window.addEventListener("hashchange", () => {
  if (!state.session) return;
  state.view = currentView();
  stopPolling();
  renderShell();
  void refreshView();
});

// ---------- Shell ----------

function renderShell() {
  const root = document.getElementById("root");
  const navItems = NAV.map((item) => {
    const active = state.view === item.id;
    return h(
      "a",
      {
        class: `nav-item${active ? " active" : ""}`,
        href: `#/${item.id}`,
        "aria-current": active ? "page" : null,
      },
      icon(item.icon),
      h("span", {}, item.label),
    );
  });

  root.replaceChildren(
    h(
      "div",
      { class: "app-shell" },
      h(
        "aside",
        { class: "sidebar" },
        h(
          "a",
          { class: "brand", href: "#/overview" },
          h("div", { class: "brand-mark" }, "db"),
          h("div", { class: "brand-name" }, "mcp-db"),
          h("span", { class: "brand-badge" }, "管理台"),
        ),
        h("nav", { class: "nav", "aria-label": "管理台导航" }, navItems),
        h(
          "div",
          { class: "sidebar-footer" },
          h(
            "div",
            { class: "account" },
            h("span", { class: "avatar" }, "A"),
            h("span", {}, "管理员"),
          ),
          h(
            "button",
            { class: "logout-btn", type: "button", onClick: () => void logout() },
            icon("logout", 16),
            h("span", {}, "退出登录"),
          ),
        ),
      ),
      h("main", { class: "main" }, h("div", { class: "content", id: "view-root" })),
    ),
  );
}

async function logout() {
  try {
    await api("/admin/api/logout", { method: "POST" });
  } catch {
    // The local session is cleared regardless.
  }
  state.session = false;
  state.csrf = "";
  stopPolling();
  closeModal();
  await showLogin(true);
}

// ---------- View loading ----------

async function refreshView() {
  const root = document.getElementById("view-root");
  if (!root) return;
  stopPolling();
  root.replaceChildren(loadingState());

  try {
    switch (state.view) {
      case "overview": {
        const data = await api("/admin/api/overview");
        const audit = await fetchAudit({ limit: 8, offset: 0 });
        const reservations = await api("/admin/api/reservations?status=quarantined");
        state.overview = data;
        state.overviewAudit = audit;
        state.reservations = reservations.reservations || [];
        renderOverview(root);
        break;
      }
      case "tokens": {
        await Promise.all([loadTokens(), loadSources()]);
        renderTokens(root);
        break;
      }
      case "approvals": {
        await loadApprovals();
        renderApprovals(root);
        startPolling();
        break;
      }
      case "audit": {
        await Promise.all([loadAudit(), loadSources()]);
        renderAudit(root);
        break;
      }
      case "sources": {
        await loadSources();
        renderSources(root);
        break;
      }
      default:
        state.view = "overview";
        await refreshView();
    }
  } catch (error) {
    if (errorMessage(error) === "登录已失效，请重新登录") return;
    renderError(root, error);
  }
}

function loadingState() {
  return h(
    "div",
    { class: "loading-state" },
    h("span", { class: "spinner", "aria-hidden": "true" }),
    h("span", {}, "加载中…"),
  );
}

function renderError(root, error) {
  root.replaceChildren(
    h(
      "div",
      { class: "error-state" },
      h("div", { class: "empty-icon" }, icon("alert", 20)),
      h("h2", { class: "empty-title" }, "加载失败"),
      h("p", { class: "empty-desc" }, errorMessage(error)),
      h(
        "button",
        { class: "btn btn-secondary", type: "button", onClick: () => void refreshView() },
        "重试",
      ),
    ),
  );
}

// ---------- Overview ----------

function renderOverview(root) {
  const data = state.overview || {
    sources: { total: 0, active: 0, archived: 0 },
    tokens: { total: 0, active: 0, revoked: 0 },
    pending_approvals: 0,
    audit_total: 0,
    quarantined_reservations: 0,
  };
  const stats = [
    {
      label: "数据源",
      value: data.sources.active,
      sub: `共 ${data.sources.total} 个 · ${data.sources.archived} 个已归档`,
    },
    {
      label: "活跃令牌",
      value: data.tokens.active,
      sub: `共 ${data.tokens.total} 个 · ${data.tokens.revoked} 个已撤销`,
    },
    {
      label: "待批请求",
      value: data.pending_approvals,
      sub: "需要管理员决定",
    },
    {
      label: "审计记录",
      value: data.audit_total,
      sub:
        data.quarantined_reservations > 0
          ? `${data.quarantined_reservations} 个隔离预留待核对`
          : "全部工具调用与决策",
    },
  ];
  const recent = state.overviewAudit.items || [];

  root.replaceChildren(
    h(
      "div",
      { class: "page" },
      h("h1", { class: "page-title" }, "概览"),
      h(
        "p",
        { class: "page-desc" },
        "本机管理面状态与最近活动。所有计数直接来自状态库；令牌明文只在签发时出现一次。",
      ),
      h(
        "div",
        { class: "stat-grid" },
        stats.map((stat) =>
          h(
            "div",
            { class: "stat-card" },
            h("div", { class: "stat-label" }, stat.label),
            h("div", { class: "stat-value" }, String(stat.value)),
            h("div", { class: "stat-sub" }, stat.sub),
          ),
        ),
      ),
      h(
        "div",
        { class: "section-head" },
        h("h2", { class: "section-title" }, "最近活动"),
        h("a", { class: "section-link", href: "#/audit" }, "查看全部 →"),
      ),
      recent.length === 0
        ? emptyState("audit", "还没有审计记录", "工具调用和审批决策会出现在这里。")
        : recentAuditTable(recent),
      reservationSection(),
    ),
  );
}

function recentAuditTable(rows) {
  return h(
    "div",
    { class: "table-card" },
    h(
      "div",
      { class: "table-scroll" },
      h(
        "table",
        {},
        h(
          "thead",
          {},
          h(
            "tr",
            {},
            h("th", {}, "时间"),
            h("th", {}, "Agent / 令牌"),
            h("th", {}, "工具"),
            h("th", {}, "决定"),
            h("th", {}, "事件"),
            h("th", { class: "th-actions" }, "操作"),
          ),
        ),
        h(
          "tbody",
          {},
          rows.map((record) =>
            h(
              "tr",
              {},
              h("td", { class: "cell-date" }, formatIso(record.ts)),
              h(
                "td",
                {},
                h("div", { class: "cell-title" }, record.agent || record.actor || "—"),
                h("div", { class: "cell-sub" }, record.token_id ? shortId(record.token_id) : "—"),
              ),
              h("td", {}, record.tool),
              h(
                "td",
                {},
                h(
                  "span",
                  { class: `badge decision-${record.decision}` },
                  decisionLabel(record.decision),
                ),
              ),
              h("td", { class: "cell-sub" }, record.event),
              h(
                "td",
                { class: "cell-actions" },
                h(
                  "button",
                  {
                    class: "btn btn-secondary btn-small",
                    type: "button",
                    onClick: () => openAuditModal(record),
                  },
                  "详情",
                ),
              ),
            ),
          ),
        ),
      ),
    ),
  );
}

function reservationSection() {
  const rows = state.reservations || [];
  return h(
    "div",
    { class: "detail-section" },
    h(
      "div",
      { class: "section-head" },
      h("h2", { class: "section-title" }, "隔离预留核对"),
      h(
        "button",
        {
          class: "btn btn-secondary btn-small",
          type: "button",
          onClick: () => void refreshView(),
        },
        icon("refresh", 14),
        "刷新",
      ),
    ),
    h(
      "p",
      { class: "page-desc" },
      "建库结果未知或部分成功时，预留会保持隔离。核对数据库实际状态后再选择释放或提交。",
    ),
    rows.length === 0
      ? h("div", { class: "callout" }, "没有隔离预留。")
      : h(
          "div",
          { class: "table-card" },
          h(
            "div",
            { class: "table-scroll" },
            h(
              "table",
              {},
              h(
                "thead",
                {},
                h(
                  "tr",
                  {},
                  h("th", {}, "ID"),
                  h("th", {}, "数据库"),
                  h("th", {}, "父数据源"),
                  h("th", {}, "令牌"),
                  h("th", {}, "原因"),
                  h("th", { class: "th-actions" }, "操作"),
                ),
              ),
              h(
                "tbody",
                {},
                rows.map((row) =>
                  h(
                    "tr",
                    {},
                    h("td", { class: "cell-date" }, `#${row.id}`),
                    h("td", {}, h("code", { class: "key" }, row.database_name)),
                    h("td", {}, row.parent_source_id),
                    h("td", {}, h("code", { class: "key" }, shortId(row.token_id))),
                    h("td", {}, row.reason_code || "—"),
                    h(
                      "td",
                      { class: "cell-actions cell-actions-wide" },
                      h(
                        "div",
                        { class: "actions" },
                        h(
                          "button",
                          {
                            class: "btn btn-secondary btn-small",
                            type: "button",
                            onClick: () => openSettleModal(row, "release"),
                          },
                          "释放",
                        ),
                        h(
                          "button",
                          {
                            class: "btn btn-primary btn-small",
                            type: "button",
                            onClick: () => openSettleModal(row, "commit"),
                          },
                          "提交",
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
  );
}

// ---------- Tokens ----------

function renderTokens(root) {
  const active = state.tokens.filter((token) => token.status === "active");
  const revoked = state.tokens.filter((token) => token.status !== "active");
  const visible = state.showRevoked ? state.tokens : active;

  root.replaceChildren(
    h(
      "div",
      { class: "page" },
      h("h1", { class: "page-title" }, "访问令牌"),
      h(
        "p",
        { class: "page-desc" },
        "令牌按「数据源 × 工具」精确授权，不再使用只读/读写模式；明文只在签发时显示一次，服务端只保存哈希。",
      ),
      h(
        "div",
        { class: "page-actions" },
        h(
          "button",
          {
            class: "btn btn-secondary",
            type: "button",
            onClick: () => {
              location.hash = "#/audit";
            },
          },
          icon("audit", 16),
          "查看审计",
        ),
        h(
          "button",
          { class: "btn btn-primary", type: "button", onClick: openCreateTokenModal },
          icon("plus", 16),
          "签发令牌",
        ),
      ),
      visible.length === 0
        ? emptyState(
            "key",
            active.length === 0 ? "还没有访问令牌" : "没有可显示的令牌",
            "签发第一个令牌，为它配置数据源与工具授权。",
            h(
              "button",
              { class: "btn btn-primary", type: "button", onClick: openCreateTokenModal },
              "签发令牌",
            ),
          )
        : tokenTable(visible),
      revoked.length > 0
        ? h(
            "label",
            { class: "toggle" },
            h("input", {
              type: "checkbox",
              checked: state.showRevoked,
              onChange: (event) => {
                state.showRevoked = event.target.checked;
                renderTokens(root);
              },
            }),
            `显示已撤销（${revoked.length}）`,
          )
        : null,
    ),
  );
}

function tokenTable(rows) {
  return h(
    "div",
    { class: "table-card" },
    h(
      "div",
      { class: "table-scroll" },
      h(
        "table",
        {},
        h(
          "thead",
          {},
          h(
            "tr",
            {},
            h("th", {}, "Agent / 令牌"),
            h("th", {}, "授权"),
            h("th", {}, "状态"),
            h("th", {}, "签发时间"),
            h("th", { class: "th-actions" }, "操作"),
          ),
        ),
        h("tbody", {}, rows.map(tokenRow)),
      ),
    ),
  );
}

function tokenRow(token) {
  const revoked = token.status !== "active";
  const grants = token.grants || [];
  return h(
    "tr",
    { class: revoked ? "row-revoked" : "" },
    h(
      "td",
      {},
      h(
        "div",
        { class: "cell-title-row" },
        h("span", { class: "cell-title" }, token.agent),
        revoked ? h("span", { class: "badge badge-revoked" }, "已撤销") : null,
      ),
      h("div", { class: "cell-sub mono" }, shortId(token.id, 12)),
    ),
    h(
      "td",
      {},
      h(
        "div",
        { class: "perm-badges" },
        grants.length === 0
          ? "—"
          : grants.map((grant) =>
              h(
                "span",
                { class: "badge tool-badge" },
                `${grant.source_id} · ${grant.tool} · ${policyLabel(grant.policy)}`,
              ),
            ),
      ),
    ),
    h(
      "td",
      {},
      h(
        "span",
        { class: `badge ${revoked ? "badge-revoked" : "decision-allowed"}` },
        revoked ? "已撤销" : "有效",
      ),
    ),
    h("td", { class: "cell-date" }, formatDay(token.created_at)),
    h(
      "td",
      { class: "cell-actions cell-actions-wide" },
      h(
        "div",
        { class: "actions" },
        h(
          "button",
          {
            class: "btn btn-secondary btn-small",
            type: "button",
            disabled: revoked,
            onClick: () => void openGrantsModal(token),
          },
          "管理授权",
        ),
        h(
          "button",
          {
            class: "icon-btn danger",
            type: "button",
            title: "撤销",
            "aria-label": `撤销 ${token.agent}`,
            disabled: revoked,
            onClick: () => openRevokeTokenModal(token),
          },
          icon("trash", 16),
        ),
      ),
    ),
  );
}

async function refreshTokensView() {
  await loadTokens();
  if (state.view === "tokens") {
    const root = document.getElementById("view-root");
    if (root) renderTokens(root);
  }
}

function openCreateTokenModal() {
  let busy = false;
  const agentInput = h("input", {
    class: "input",
    id: "new-token-agent",
    placeholder: "例如 gbrain",
    maxlength: "128",
    autocomplete: "off",
  });
  const errorEl = h("p", { class: "form-error hidden" });
  const submit = h("button", { class: "btn btn-primary", type: "submit" }, "签发令牌");

  function setError(message) {
    errorEl.textContent = message || "";
    errorEl.classList.toggle("hidden", !message);
  }

  const form = h(
    "form",
    {
      class: "modal-form",
      onSubmit: async (event) => {
        event.preventDefault();
        if (busy) return;
        setError("");
        const agent = agentInput.value.trim();
        if (!agent) {
          setError("请输入 Agent 名称");
          agentInput.focus();
          return;
        }
        busy = true;
        submit.disabled = true;
        submit.textContent = "签发中…";
        try {
          const data = await api("/admin/api/tokens", { method: "POST", body: { agent } });
          await loadTokens();
          showCreatedToken(data);
        } catch (error) {
          busy = false;
          submit.disabled = false;
          submit.textContent = "签发令牌";
          setError(errorMessage(error));
        }
      },
    },
    h(
      "div",
      { class: "modal-body" },
      h(
        "div",
        { class: "field" },
        h("label", { class: "field-label", for: "new-token-agent" }, "Agent"),
        agentInput,
        h(
          "div",
          { class: "field-hint" },
          "审计按 agent 分组；同一 agent 可以签发多个令牌。签发后不会自动获得任何数据源授权。",
        ),
      ),
      errorEl,
    ),
    h(
      "div",
      { class: "modal-foot" },
      h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "取消"),
      submit,
    ),
  );

  openModal(
    h(
      "div",
      { class: "modal" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, "签发访问令牌"),
        h(
          "p",
          { class: "modal-subtitle" },
          "令牌是 agent 的 Bearer 凭据。明文只显示一次，需立即保存。",
        ),
      ),
      form,
    ),
  );
  agentInput.focus();
}

function showCreatedToken(data) {
  const plaintext = data.plaintext;
  const token = data.token;
  const snippet = `Authorization: Bearer ${plaintext}`;

  openModal(
    h(
      "div",
      { class: "modal modal-wide" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, "令牌已签发"),
        h("p", { class: "modal-subtitle" }, `${token.agent} · ${shortId(token.id, 12)}`),
      ),
      h(
        "div",
        { class: "modal-body" },
        h(
          "div",
          { class: "callout callout-warning" },
          "这是唯一一次显示令牌明文。关闭后无法再次查看；请立即复制并保存到安全位置。",
        ),
        h(
          "div",
          { class: "detail-block" },
          h("div", { class: "detail-block-title" }, "令牌明文"),
          h("code", { class: "code-block" }, plaintext),
          h(
            "div",
            { class: "copy-row" },
            h(
              "button",
              {
                class: "btn btn-secondary btn-small",
                type: "button",
                onClick: () => void copyText(plaintext),
              },
              icon("copy", 15),
              "复制令牌",
            ),
          ),
        ),
        h(
          "div",
          { class: "detail-block" },
          h("div", { class: "detail-block-title" }, "Authorization 请求头"),
          h("code", { class: "code-block" }, snippet),
          h(
            "div",
            { class: "copy-row" },
            h(
              "button",
              {
                class: "btn btn-secondary btn-small",
                type: "button",
                onClick: () => void copyText(snippet),
              },
              icon("copy", 15),
              "复制请求头",
            ),
          ),
        ),
        h(
          "div",
          { class: "callout" },
          "MCP 地址由 MCPDB_ADDR 决定（默认 127.0.0.1:8787/mcp），与当前管理端口不同，请勿把管理端口当作 MCP 端点。",
        ),
      ),
      h(
        "div",
        { class: "modal-foot" },
        h(
          "button",
          {
            class: "btn btn-primary",
            type: "button",
            onClick: () => {
              closeModal();
              void refreshTokensView();
            },
          },
          "我已保存，关闭",
        ),
      ),
    ),
    { dismissible: false },
  );
}

async function openGrantsModal(token) {
  let data;
  try {
    data = await api(`/admin/api/tokens/${encodeURIComponent(token.id)}/grants`);
  } catch (error) {
    showToast(errorMessage(error), "error");
    return;
  }
  const grants = data.grants || [];
  if (state.sources.length === 0) await loadSources();
  const sources = state.sources.filter((source) => source.status === "active");

  const sourceSelect = h(
    "select",
    { class: "select", "aria-label": "数据源" },
    sources.map((source) => h("option", { value: source.id }, sourceLabel(source))),
  );
  const toolSelect = h("select", { class: "select", "aria-label": "工具" });
  const policySelect = h("select", { class: "select", "aria-label": "执行策略" });
  const addError = h("p", { class: "form-error hidden" });

  function selectedSource() {
    return sources.find((source) => source.id === sourceSelect.value);
  }

  function renderTools() {
    const source = selectedSource();
    const production = source && source.environment === "production";
    const tools = TOOLS.filter((tool) => !(production && (tool === "mutate_sql" || tool === "execute_ddl")));
    toolSelect.replaceChildren(
      ...tools.map((tool) => h("option", { value: tool }, TOOL_LABELS[tool])),
    );
    renderPolicies();
  }

  function renderPolicies() {
    const tool = toolSelect.value;
    let policies = ["direct"];
    if (tool === "mutate_sql" || tool === "execute_ddl" || tool === "create_database") {
      policies = ["approval", "direct"];
    }
    if (tool === "create_database") policies = ["approval"];
    policySelect.replaceChildren(
      ...policies.map((policy) => h("option", { value: policy }, policyLabel(policy))),
    );
    policySelect.value = policies[0];
  }

  sourceSelect.addEventListener("change", renderTools);
  toolSelect.addEventListener("change", renderPolicies);
  renderTools();

  const form = h(
    "form",
    {
      class: "modal-form",
      onSubmit: async (event) => {
        event.preventDefault();
        addError.classList.add("hidden");
        if (sources.length === 0) {
          addError.textContent = "请先创建处于启用状态的数据源";
          addError.classList.remove("hidden");
          return;
        }
        try {
          await api(`/admin/api/tokens/${encodeURIComponent(token.id)}/grants`, {
            method: "POST",
            body: {
              source_id: sourceSelect.value,
              tool: toolSelect.value,
              policy: policySelect.value,
            },
          });
          showToast("已添加授权");
          closeModal();
          await openGrantsModal(token);
        } catch (error) {
          addError.textContent = errorMessage(error);
          addError.classList.remove("hidden");
        }
      },
    },
    h(
      "div",
      { class: "modal-body" },
      h(
        "div",
        { class: "callout" },
        "授权表示「令牌 + 数据源 + 工具」的允许关系。每条授权精确绑定一个数据源和一个工具：查询权限不隐含写入，写入权限不隐含查询。生产数据源不能授予写入或结构变更。",
      ),
      grants.length === 0
        ? h("div", { class: "callout" }, "该令牌还没有授权，agent 只能看到 list_sources。")
        : h(
            "div",
            { class: "grant-list" },
            grants.map((grant) =>
              h(
                "div",
                { class: "grant-item" },
                h(
                  "div",
                  { class: "grant-main" },
                  h("span", { class: "badge tool-badge" }, grant.source_id),
                  h("span", { class: "badge tool-badge" }, grant.tool),
                  h(
                    "span",
                    { class: `badge policy-${grant.policy}` },
                    policyLabel(grant.policy),
                  ),
                ),
                h(
                  "button",
                  {
                    class: "icon-btn danger",
                    type: "button",
                    title: "移除授权",
                    "aria-label": `移除 ${grant.source_id}/${grant.tool}`,
                    onClick: async () => {
                      try {
                        const params = new URLSearchParams({
                          source: grant.source_id,
                          tool: grant.tool,
                        });
                        await api(
                          `/admin/api/tokens/${encodeURIComponent(token.id)}/grants?${params.toString()}`,
                          { method: "DELETE" },
                        );
                        showToast("已移除授权");
                        closeModal();
                        await openGrantsModal(token);
                      } catch (error) {
                        showToast(errorMessage(error), "error");
                      }
                    },
                  },
                  icon("trash", 16),
                ),
              ),
            ),
          ),
      h(
        "div",
        { class: "detail-section" },
        h("div", { class: "detail-block-title" }, "添加授权"),
        sources.length === 0
          ? h("div", { class: "callout callout-warning" }, "没有启用中的数据源。")
          : h(
              "div",
              { class: "perm-row" },
              h(
                "div",
                { class: "field" },
                h("label", { class: "field-label" }, "数据源"),
                sourceSelect,
              ),
              h(
                "div",
                { class: "field" },
                h("label", { class: "field-label" }, "工具"),
                toolSelect,
              ),
              h(
                "div",
                { class: "field" },
                h("label", { class: "field-label" }, "策略"),
                policySelect,
              ),
            ),
        h(
          "div",
          { class: "field-hint" },
          "工具还需要该数据源上存在用途匹配的工具路由才能被调用。list_sources 是所有有效令牌的基础发现工具，无需授权。",
        ),
        addError,
      ),
    ),
    h(
      "div",
      { class: "modal-foot" },
      h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "关闭"),
      h("button", { class: "btn btn-primary", type: "submit" }, "添加授权"),
    ),
  );

  openModal(
    h(
      "div",
      { class: "modal modal-wide" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, "管理授权"),
        h("p", { class: "modal-subtitle" }, `${token.agent} · ${shortId(token.id, 12)}`),
      ),
      form,
    ),
  );
  if (sources.length === 0) sourceSelect.disabled = true;
}

function openRevokeTokenModal(token) {
  const errorEl = h("p", { class: "form-error hidden" });
  const submit = h(
    "button",
    {
      class: "btn btn-danger",
      type: "button",
      onClick: async () => {
        submit.disabled = true;
        submit.textContent = "撤销中…";
        try {
          await api(`/admin/api/tokens/${encodeURIComponent(token.id)}/revoke`, {
            method: "POST",
          });
          closeModal();
          showToast("令牌已撤销");
          await refreshTokensView();
        } catch (error) {
          submit.disabled = false;
          submit.textContent = "撤销令牌";
          errorEl.textContent = errorMessage(error);
          errorEl.classList.remove("hidden");
        }
      },
    },
    "撤销令牌",
  );

  openModal(
    h(
      "div",
      { class: "modal" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, "撤销访问令牌"),
        h("p", { class: "modal-subtitle" }, token.agent),
      ),
      h(
        "div",
        { class: "modal-body" },
        h(
          "div",
          { class: "callout callout-danger" },
          "撤销后使用该令牌的客户端会立即失去访问，基于该令牌的待批请求也会失效。此操作不可恢复。",
        ),
        errorEl,
      ),
      h(
        "div",
        { class: "modal-foot" },
        h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "取消"),
        submit,
      ),
    ),
  );
}

// ---------- Approvals ----------

function renderApprovals(root) {
  const rows = state.approvals;
  root.replaceChildren(
    h(
      "div",
      { class: "page" },
      h("h1", { class: "page-title" }, "待批请求"),
      h(
        "p",
        { class: "page-desc" },
        "高危写入、审批模式的普通写入和建库请求会在这里等待决定。请先打开审查弹层核对完整 SQL、参数、连接配置与工具路由版本，再批准或拒绝。",
      ),
      h(
        "div",
        { class: "page-actions" },
        h(
          "span",
          { class: "page-count" },
          rows.length > 0 ? `${rows.length} 条待处理` : "暂无待处理",
        ),
        h(
          "button",
          { class: "btn btn-secondary", type: "button", onClick: () => void refreshApprovals() },
          icon("refresh", 16),
          "刷新",
        ),
      ),
      rows.length === 0
        ? emptyState("approval", "没有待批请求", "需要管理员决定的请求会出现在这里。")
        : approvalsTable(rows),
    ),
  );
}

function approvalsTable(rows) {
  return h(
    "div",
    { class: "table-card" },
    h(
      "div",
      { class: "table-scroll" },
      h(
        "table",
        {},
        h(
          "thead",
          {},
          h(
            "tr",
            {},
            h("th", {}, "ID"),
            h("th", {}, "Agent / 令牌"),
            h("th", {}, "数据源"),
            h("th", {}, "工具"),
            h("th", {}, "风险"),
            h("th", {}, "请求预览"),
            h("th", {}, "剩余"),
            h("th", { class: "th-actions" }, "操作"),
          ),
        ),
        h("tbody", {}, rows.map(approvalRow)),
      ),
    ),
  );
}

function approvalRow(record) {
  const preview = record.tool === "create_database" ? "建库规格" : sqlPreview(record.sql);
  return h(
    "tr",
    {},
    h("td", { class: "cell-date" }, `#${record.id}`),
    h(
      "td",
      {},
      h("div", { class: "cell-title" }, record.agent),
      h("div", { class: "cell-sub mono" }, shortId(record.token_id, 10)),
    ),
    h(
      "td",
      {},
      h("div", {}, record.source_id),
      h(
        "span",
        { class: `badge env-${record.source_environment}` },
        environmentLabel(record.source_environment),
      ),
    ),
    h("td", {}, h("span", { class: "badge tool-badge" }, record.tool)),
    h(
      "td",
      {},
      record.high_risk
        ? h("span", { class: "badge badge-risk" }, "高危")
        : h("span", { class: "badge" }, "常规"),
    ),
    h("td", {}, h("code", { class: "sql-preview" }, preview)),
    h("td", { class: "cell-date" }, remaining(record.expires_at)),
    h(
      "td",
      { class: "cell-actions" },
      h(
        "button",
        {
          class: "btn btn-secondary btn-small",
          type: "button",
          onClick: () => openApprovalModal(record),
        },
        "审查",
      ),
    ),
  );
}

async function refreshApprovals() {
  await loadApprovals();
  if (state.view === "approvals") {
    const root = document.getElementById("view-root");
    if (root) renderApprovals(root);
  }
}

async function decideApproval(id, approve) {
  try {
    await api(`/admin/api/approvals/${id}/${approve ? "approve" : "deny"}`, {
      method: "POST",
    });
    showToast(approve ? "已批准" : "已拒绝");
    closeModal();
    await refreshApprovals();
  } catch (error) {
    showToast(errorMessage(error), "error");
    await refreshApprovals().catch(() => undefined);
  }
}

function approvalSpec(record) {
  if (record.tool !== "create_database") return null;
  try {
    return JSON.parse(record.params_json);
  } catch {
    return null;
  }
}

function openApprovalModal(record) {
  const spec = approvalSpec(record);
  const specRows = spec
    ? [
        h("div", { class: "detail-label" }, "数据库名称"),
        h("div", { class: "detail-value mono" }, spec.database || "—"),
        h("div", { class: "detail-label" }, "环境"),
        h("div", { class: "detail-value" }, environmentLabel(spec.environment)),
        h("div", { class: "detail-label" }, "只读角色"),
        h("div", { class: "detail-value mono" }, spec.read_only_role || "—"),
        h("div", { class: "detail-label" }, "读写角色"),
        h("div", { class: "detail-value mono" }, spec.read_write_role || "—"),
        h(
          "div",
          { class: "detail-label" },
          "结果工具",
        ),
        h("div", { class: "detail-value mono" }, (spec.result_tools || []).join(", ") || "—"),
      ]
    : [];

  openModal(
    h(
      "div",
      { class: "modal modal-wide" },
      h(
        "div",
        { class: "modal-head" },
        h(
          "h2",
          { class: "modal-title" },
          record.tool === "create_database" ? `待批建库请求 #${record.id}` : `待批写请求 #${record.id}`,
        ),
        h(
          "p",
          { class: "modal-subtitle" },
          `${record.agent} · ${record.source_id} · ${environmentLabel(record.source_environment)}`,
        ),
      ),
      h(
        "div",
        { class: "modal-body" },
        record.high_risk
          ? h("div", { class: "callout callout-danger" }, "该请求被标记为高危，必须由管理员逐次决定。")
          : h(
              "div",
              { class: "callout" },
              "批准只对当前不可变请求摘要有效且只能消费一次；令牌、数据源、连接配置、工具路由或策略变化会使批准失效。",
            ),
        h(
          "div",
          { class: "detail-grid" },
          h("div", { class: "detail-label" }, "Agent"),
          h("div", { class: "detail-value" }, record.agent),
          h("div", { class: "detail-label" }, "令牌"),
          h("div", { class: "detail-value mono" }, record.token_id),
          h("div", { class: "detail-label" }, "数据源"),
          h("div", { class: "detail-value" }, record.source_id),
          h("div", { class: "detail-label" }, "工具"),
          h("div", { class: "detail-value mono" }, record.tool),
          h("div", { class: "detail-label" }, "连接配置"),
          h(
            "div",
            { class: "detail-value" },
            `#${record.profile_id} · v${record.profile_version}`,
          ),
          h("div", { class: "detail-label" }, "工具路由"),
          h(
            "div",
            { class: "detail-value" },
            `#${record.route_id} · v${record.route_version}`,
          ),
          h("div", { class: "detail-label" }, "授权策略"),
          h("div", { class: "detail-value" }, policyLabel(record.grant_policy)),
          h("div", { class: "detail-label" }, "路由最低策略"),
          h("div", { class: "detail-value" }, policyLabel(record.route_min_policy)),
          h("div", { class: "detail-label" }, "渠道"),
          h("div", { class: "detail-value" }, record.channel),
          h("div", { class: "detail-label" }, "创建时间"),
          h("div", { class: "detail-value" }, formatDateTime(record.created_at)),
          h("div", { class: "detail-label" }, "剩余"),
          h("div", { class: "detail-value" }, remaining(record.expires_at)),
          specRows,
        ),
        record.sql
          ? h(
              "div",
              { class: "detail-block" },
              h("div", { class: "detail-block-title" }, "精确 SQL"),
              h("code", { class: "code-block" }, record.sql),
            )
          : null,
        h(
          "div",
          { class: "detail-block" },
          h(
            "div",
            { class: "detail-block-title" },
            record.tool === "create_database" ? "建库规格参数（摘要绑定）" : "绑定参数",
          ),
          h("code", { class: "code-block" }, formattedJson(record.params_json)),
        ),
        h(
          "div",
          { class: "detail-block" },
          h("div", { class: "detail-block-title" }, "请求摘要"),
          h("code", { class: "code-block" }, record.request_digest),
        ),
      ),
      h(
        "div",
        { class: "modal-foot" },
        h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "关闭"),
        h(
          "button",
          {
            class: "btn btn-danger",
            type: "button",
            onClick: () => void decideApproval(record.id, false),
          },
          "拒绝",
        ),
        h(
          "button",
          {
            class: "btn btn-primary",
            type: "button",
            onClick: () => void decideApproval(record.id, true),
          },
          "批准",
        ),
      ),
    ),
  );
}

function startPolling() {
  stopPolling();
  state.pollTimer = window.setInterval(() => {
    if (state.view !== "approvals") {
      stopPolling();
      return;
    }
    const modalRoot = document.getElementById("modal-root");
    if (modalRoot && !modalRoot.hidden) return;
    void refreshApprovals().catch(() => undefined);
  }, 3000);
}

function stopPolling() {
  if (state.pollTimer !== null) {
    window.clearInterval(state.pollTimer);
    state.pollTimer = null;
  }
}

// ---------- Audit ----------

const DECISIONS = [
  "allowed",
  "rejected",
  "error",
  "unknown",
  "expired",
  "pending",
  "approved",
  "denied",
  "consumed",
];

function renderAudit(root) {
  const filters = state.auditFilters;
  const agentInput = h("input", {
    class: "filter-input",
    value: filters.agent,
    placeholder: "agent",
    "aria-label": "按 agent 过滤",
  });
  const sourceSelect = h(
    "select",
    { class: "filter-input", "aria-label": "按数据源过滤" },
    h("option", { value: "" }, "全部数据源"),
    state.sources.map((source) => h("option", { value: source.id }, sourceLabel(source))),
  );
  sourceSelect.value = filters.source;
  const decisionSelect = h(
    "select",
    { class: "filter-input", "aria-label": "按决定过滤" },
    h("option", { value: "" }, "全部决定"),
    DECISIONS.map((decision) => h("option", { value: decision }, decisionLabel(decision))),
  );
  decisionSelect.value = filters.decision;

  const form = h(
    "form",
    {
      class: "filter-bar",
      onSubmit: async (event) => {
        event.preventDefault();
        state.auditFilters = {
          agent: agentInput.value.trim(),
          source: sourceSelect.value,
          decision: decisionSelect.value,
        };
        state.auditOffset = 0;
        await loadAudit();
        renderAudit(root);
      },
    },
    h("div", { class: "filter-field" }, h("label", { class: "filter-label" }, "Agent"), agentInput),
    h(
      "div",
      { class: "filter-field" },
      h("label", { class: "filter-label" }, "数据源"),
      sourceSelect,
    ),
    h(
      "div",
      { class: "filter-field" },
      h("label", { class: "filter-label" }, "决定"),
      decisionSelect,
    ),
    h(
      "div",
      { class: "filter-actions" },
      h(
        "button",
        {
          class: "btn btn-secondary btn-small",
          type: "button",
          onClick: async () => {
            state.auditFilters = { agent: "", source: "", decision: "" };
            state.auditOffset = 0;
            await loadAudit();
            renderAudit(root);
          },
        },
        "重置",
      ),
      h("button", { class: "btn btn-primary btn-small", type: "submit" }, "查询"),
    ),
  );

  const { items, total } = state.audit;
  root.replaceChildren(
    h(
      "div",
      { class: "page" },
      h("h1", { class: "page-title" }, "审计日志"),
      h(
        "p",
        { class: "page-desc" },
        "记录工具调用、审批决策和管理操作。可按 agent、数据源和决定筛选。审计不保存令牌明文或凭证。",
      ),
      form,
      items.length === 0
        ? emptyState("audit", "没有匹配的记录", "调整过滤条件或等待新的工具调用。")
        : auditTable(items),
      h(
        "div",
        { class: "pager" },
        h("span", {}, `共 ${total} 条`),
        h(
          "div",
          { class: "pager-actions" },
          h(
            "button",
            {
              class: "btn btn-secondary btn-small",
              type: "button",
              disabled: state.auditOffset <= 0,
              onClick: () => void changeAuditPage(-1),
            },
            icon("chevronLeft", 14),
            "上一页",
          ),
          h(
            "button",
            {
              class: "btn btn-secondary btn-small",
              type: "button",
              disabled: state.auditOffset + items.length >= total,
              onClick: () => void changeAuditPage(1),
            },
            "下一页",
            icon("chevronRight", 14),
          ),
        ),
      ),
    ),
  );
}

function auditTable(rows) {
  return h(
    "div",
    { class: "table-card" },
    h(
      "div",
      { class: "table-scroll" },
      h(
        "table",
        {},
        h(
          "thead",
          {},
          h(
            "tr",
            {},
            h("th", {}, "时间"),
            h("th", {}, "事件"),
            h("th", {}, "Agent / 令牌"),
            h("th", {}, "数据源"),
            h("th", {}, "工具"),
            h("th", {}, "决定"),
            h("th", {}, "角色"),
            h("th", {}, "耗时"),
            h("th", {}, "行数"),
            h("th", { class: "th-actions" }, "操作"),
          ),
        ),
        h(
          "tbody",
          {},
          rows.map((record) =>
            h(
              "tr",
              {},
              h("td", { class: "cell-date" }, formatIso(record.ts)),
              h("td", { class: "cell-sub" }, record.event),
              h(
                "td",
                {},
                h("div", { class: "cell-title" }, record.agent || "—"),
                h("div", { class: "cell-sub mono" }, record.token_id ? shortId(record.token_id, 10) : "—"),
              ),
              h("td", {}, record.source_id || "—"),
              h("td", {}, h("span", { class: "badge tool-badge" }, record.tool)),
              h(
                "td",
                {},
                h(
                  "span",
                  { class: `badge decision-${record.decision}` },
                  decisionLabel(record.decision),
                ),
              ),
              h("td", { class: "cell-sub" }, record.actor),
              h(
                "td",
                { class: "cell-date" },
                record.duration_ms === null ? "—" : `${record.duration_ms} ms`,
              ),
              h(
                "td",
                { class: "cell-date" },
                record.row_count === null ? "—" : String(record.row_count),
              ),
              h(
                "td",
                { class: "cell-actions" },
                h(
                  "button",
                  {
                    class: "btn btn-secondary btn-small",
                    type: "button",
                    onClick: () => openAuditModal(record),
                  },
                  "详情",
                ),
              ),
            ),
          ),
        ),
      ),
    ),
  );
}

async function changeAuditPage(delta) {
  const next = state.auditOffset + delta * state.auditLimit;
  if (next < 0) return;
  state.auditOffset = next;
  await loadAudit();
  if (state.view === "audit") {
    const root = document.getElementById("view-root");
    if (root) renderAudit(root);
  }
}

function openAuditModal(record) {
  openModal(
    h(
      "div",
      { class: "modal modal-wide" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, `审计记录 #${record.event}`),
        h("p", { class: "modal-subtitle" }, formatIso(record.ts)),
      ),
      h(
        "div",
        { class: "modal-body" },
        h(
          "div",
          { class: "detail-grid" },
          h("div", { class: "detail-label" }, "事件"),
          h("div", { class: "detail-value mono" }, record.event),
          h("div", { class: "detail-label" }, "Agent"),
          h("div", { class: "detail-value" }, record.agent || "—"),
          h("div", { class: "detail-label" }, "令牌"),
          h("div", { class: "detail-value mono" }, record.token_id || "—"),
          h("div", { class: "detail-label" }, "数据源"),
          h("div", { class: "detail-value" }, record.source_id || "—"),
          h("div", { class: "detail-label" }, "工具"),
          h("div", { class: "detail-value mono" }, record.tool),
          h("div", { class: "detail-label" }, "决定"),
          h("div", { class: "detail-value" }, decisionLabel(record.decision)),
          h("div", { class: "detail-label" }, "角色"),
          h("div", { class: "detail-value" }, record.actor),
          h("div", { class: "detail-label" }, "渠道"),
          h("div", { class: "detail-value" }, record.channel || "—"),
          h(
            "div",
            { class: "detail-label" },
            "审批 ID",
          ),
          h(
            "div",
            { class: "detail-value" },
            record.approval_id === null ? "—" : `#${record.approval_id}`,
          ),
          h("div", { class: "detail-label" }, "连接配置"),
          h(
            "div",
            { class: "detail-value" },
            record.profile_id === null
              ? "—"
              : `#${record.profile_id} · v${record.profile_version}`,
          ),
          h("div", { class: "detail-label" }, "工具路由"),
          h(
            "div",
            { class: "detail-value" },
            record.route_id === null ? "—" : `#${record.route_id} · v${record.route_version}`,
          ),
          h("div", { class: "detail-label" }, "耗时"),
          h(
            "div",
            { class: "detail-value" },
            record.duration_ms === null ? "—" : `${record.duration_ms} ms`,
          ),
          h("div", { class: "detail-label" }, "行数"),
          h(
            "div",
            { class: "detail-value" },
            record.row_count === null ? "—" : String(record.row_count),
          ),
          h("div", { class: "detail-label" }, "原因码"),
          h("div", { class: "detail-value mono" }, record.reason_code || "—"),
          h("div", { class: "detail-label" }, "原因"),
          h("div", { class: "detail-value" }, record.reason || "—"),
        ),
        record.request_digest
          ? h(
              "div",
              { class: "detail-block" },
              h("div", { class: "detail-block-title" }, "请求摘要"),
              h("code", { class: "code-block" }, record.request_digest),
            )
          : null,
      ),
      h(
        "div",
        { class: "modal-foot" },
        h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "关闭"),
      ),
    ),
  );
}

// ---------- Sources ----------

function renderSources(root) {
  const rows = state.sources;
  root.replaceChildren(
    h(
      "div",
      { class: "page" },
      h("h1", { class: "page-title" }, "数据源"),
      h(
        "p",
        { class: "page-desc" },
        "数据源代表一个 PostgreSQL 数据库。连接配置直接填写数据库连接串，服务端加密保存，页面不回显。归档不会删除物理数据库。",
      ),
      h(
        "div",
        { class: "page-actions" },
        h(
          "button",
          { class: "btn btn-primary", type: "button", onClick: openCreateSourceModal },
          icon("plus", 16),
          "新增数据源",
        ),
      ),
      rows.length === 0
        ? emptyState(
            "source",
            "还没有配置数据源",
            "新增第一个数据源，再为它配置连接配置与工具路由。",
            h(
              "button",
              { class: "btn btn-primary", type: "button", onClick: openCreateSourceModal },
              "新增数据源",
            ),
          )
        : sourcesTable(rows),
    ),
  );
}

function sourcesTable(rows) {
  return h(
    "div",
    { class: "table-card" },
    h(
      "div",
      { class: "table-scroll" },
      h(
        "table",
        {},
        h(
          "thead",
          {},
          h(
            "tr",
            {},
            h("th", {}, "ID"),
            h("th", {}, "环境"),
            h("th", {}, "状态"),
            h("th", {}, "连接配置"),
            h("th", {}, "授权"),
            h("th", {}, "创建时间"),
            h("th", { class: "th-actions" }, "操作"),
          ),
        ),
        h(
          "tbody",
          {},
          rows.map((source) =>
            h(
              "tr",
              {},
              h("td", {}, h("code", { class: "key" }, source.id)),
              h(
                "td",
                {},
                h(
                  "span",
                  { class: `badge env-${source.environment}` },
                  environmentLabel(source.environment),
                ),
              ),
              h(
                "td",
                {},
                h(
                  "span",
                  { class: `badge ${source.status === "archived" ? "status-archived" : ""}` },
                  statusLabel(source.status),
                ),
              ),
              h("td", { class: "cell-date" }, String(source.profile_count)),
              h("td", { class: "cell-date" }, String(source.grant_count)),
              h("td", { class: "cell-date" }, formatDay(source.created_at)),
              h(
                "td",
                { class: "cell-actions cell-actions-wide" },
                h(
                  "div",
                  { class: "actions" },
                  h(
                    "button",
                    {
                      class: "btn btn-secondary btn-small",
                      type: "button",
                      onClick: () => void openSourceDetail(source.id),
                    },
                    "详情",
                  ),
                  source.status === "active"
                    ? h(
                        "button",
                        {
                          class: "icon-btn danger",
                          type: "button",
                          title: "归档",
                          "aria-label": `归档 ${source.id}`,
                          onClick: () => openArchiveModal(source),
                        },
                        icon("trash", 16),
                      )
                    : h(
                        "button",
                        {
                          class: "btn btn-secondary btn-small",
                          type: "button",
                          onClick: () => openRestoreModal(source),
                        },
                        "恢复",
                      ),
                ),
              ),
            ),
          ),
        ),
      ),
    ),
  );
}

async function reloadSourcesPage() {
  await loadSources();
  if (state.view === "sources") {
    const root = document.getElementById("view-root");
    if (root) renderSources(root);
  }
}

function openCreateSourceModal() {
  let busy = false;
  const idInput = h("input", {
    class: "input",
    id: "new-source-id",
    placeholder: "例如 analytics",
    maxlength: "64",
    autocomplete: "off",
  });
  const environmentSelect = h(
    "select",
    { class: "select", id: "new-source-environment" },
    h("option", { value: "nonproduction" }, "非生产"),
    h("option", { value: "production" }, "生产"),
  );
  const errorEl = h("p", { class: "form-error hidden" });
  const submit = h("button", { class: "btn btn-primary", type: "submit" }, "创建数据源");

  const form = h(
    "form",
    {
      class: "modal-form",
      onSubmit: async (event) => {
        event.preventDefault();
        if (busy) return;
        errorEl.classList.add("hidden");
        const id = idInput.value.trim();
        if (!id) {
          errorEl.textContent = "请输入数据源 ID";
          errorEl.classList.remove("hidden");
          idInput.focus();
          return;
        }
        busy = true;
        submit.disabled = true;
        submit.textContent = "创建中…";
        try {
          await api("/admin/api/sources", {
            method: "POST",
            body: { id, environment: environmentSelect.value },
          });
          closeModal();
          showToast("数据源已创建");
          await reloadSourcesPage();
          await openSourceDetail(id);
        } catch (error) {
          busy = false;
          submit.disabled = false;
          submit.textContent = "创建数据源";
          errorEl.textContent = errorMessage(error);
          errorEl.classList.remove("hidden");
        }
      },
    },
    h(
      "div",
      { class: "modal-body" },
      h(
        "div",
        { class: "form-grid" },
        h(
          "div",
          { class: "field" },
          h("label", { class: "field-label", for: "new-source-id" }, "ID"),
          idInput,
          h("div", { class: "field-hint" }, "小写字母开头，可含数字、_ 和 -。"),
        ),
        h(
          "div",
          { class: "field" },
          h("label", { class: "field-label", for: "new-source-environment" }, "环境"),
          environmentSelect,
          h(
            "div",
            { class: "field-hint" },
            "生产数据源禁止 mutate_sql 与 execute_ddl 授权。",
          ),
        ),
      ),
      h(
        "div",
        { class: "callout" },
        "创建后请添加用途匹配的连接配置（查询 / 写入 / 结构变更 / 建库），再配置工具路由。连接配置里直接填写数据库连接串即可，服务端会加密保存。",
      ),
      errorEl,
    ),
    h(
      "div",
      { class: "modal-foot" },
      h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "取消"),
      submit,
    ),
  );

  openModal(
    h(
      "div",
      { class: "modal" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, "新增数据源"),
        h("p", { class: "modal-subtitle" }, "驱动首版固定为 PostgreSQL。"),
      ),
      form,
    ),
  );
  idInput.focus();
}

async function openSourceDetail(id) {
  openModal(
    h(
      "div",
      { class: "modal modal-wide" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, `数据源 ${id}`),
      ),
      h("div", { class: "modal-body" }, loadingState()),
    ),
  );
  try {
    const data = await api(`/admin/api/sources/${encodeURIComponent(id)}`);
    openModal(sourceDetailModal(data));
  } catch (error) {
    closeModal();
    showToast(errorMessage(error), "error");
  }
}

function sourceDetailModal(data) {
  const source = data.source;
  const profiles = data.profiles || [];
  const routes = data.routes || [];
  const profileNames = new Map(profiles.map((profile) => [profile.id, profile.name]));

  return h(
    "div",
    { class: "modal modal-wide" },
    h(
      "div",
      { class: "modal-head" },
      h(
        "h2",
        { class: "modal-title" },
        h("code", { class: "key" }, source.id),
        " ",
        h(
          "span",
          { class: `badge env-${source.environment}` },
          environmentLabel(source.environment),
        ),
        " ",
        h(
          "span",
          { class: `badge ${source.status === "archived" ? "status-archived" : ""}` },
          statusLabel(source.status),
        ),
      ),
      h("p", { class: "modal-subtitle" }, "连接配置引用密钥环中的凭证；管理页不回显 DSN 或密码。"),
    ),
    h(
      "div",
      { class: "modal-body" },
      h(
        "div",
        { class: "detail-grid" },
        h("div", { class: "detail-label" }, "驱动"),
        h("div", { class: "detail-value" }, source.driver),
        h("div", { class: "detail-label" }, "父数据源"),
        h("div", { class: "detail-value" }, source.parent_source_id || "—"),
        h("div", { class: "detail-label" }, "创建时间"),
        h("div", { class: "detail-value" }, formatDateTime(source.created_at)),
        h("div", { class: "detail-label" }, "归档时间"),
        h("div", { class: "detail-value" }, formatDateTime(source.archived_at)),
        h("div", { class: "detail-label" }, "授权"),
        h("div", { class: "detail-value" }, String(source.grant_count)),
      ),
      h(
        "div",
        { class: "detail-section" },
        h(
          "div",
          { class: "section-head" },
          h("h3", { class: "section-title" }, "连接配置"),
          h(
            "button",
            {
              class: "btn btn-secondary btn-small",
              type: "button",
              onClick: () => openProfileModal(source.id),
            },
            icon("plus", 14),
            "添加连接配置",
          ),
        ),
        h("p", { class: "field-hint" }, "连接配置表示这个数据源使用哪套数据库账号。"),
        profiles.length === 0
          ? h("div", { class: "callout callout-warning" }, "还没有连接配置；恢复数据源前至少需要一个启用的连接配置。")
          : h(
              "div",
              { class: "table-card" },
              h(
                "div",
                { class: "table-scroll" },
                h(
                  "table",
                  {},
                  h(
                    "thead",
                    {},
                    h(
                      "tr",
                      {},
                      h("th", {}, "名称"),
                      h("th", {}, "用途"),
                      h("th", {}, "凭证"),
                      h("th", {}, "连接上限"),
                      h("th", {}, "语句超时"),
                      h("th", {}, "版本"),
                      h("th", { class: "th-actions" }, "操作"),
                    ),
                  ),
                  h(
                    "tbody",
                    {},
                    profiles.map((profile) =>
                      h(
                        "tr",
                        {},
                        h(
                          "td",
                          {},
                          h("div", { class: "cell-title" }, profile.name),
                          h(
                            "div",
                            { class: "cell-sub" },
                            profile.enabled ? "启用" : "停用",
                          ),
                        ),
                        h(
                          "td",
                          {},
                          h(
                            "span",
                            { class: "badge tool-badge" },
                            purposeLabel(profile.purpose),
                          ),
                        ),
                        h("td", {}, h("span", { class: "badge" }, "已加密配置")),
                        h(
                          "td",
                          { class: "cell-date" },
                          profile.max_connections === null ? "默认" : String(profile.max_connections),
                        ),
                        h(
                          "td",
                          { class: "cell-date" },
                          profile.statement_timeout_ms === null
                            ? "默认"
                            : `${profile.statement_timeout_ms} ms`,
                        ),
                        h("td", { class: "cell-date" }, `v${profile.version}`),
                        h(
                          "td",
                          {},
                          h(
                            "div",
                            { class: "cell-actions" },
                            h(
                              "button",
                              {
                                class: "btn btn-secondary btn-small",
                                type: "button",
                                onClick: () => openProfileModal(source.id, profile),
                              },
                              "编辑",
                            ),
                            h(
                              "button",
                              {
                                class: "btn btn-secondary btn-small",
                                type: "button",
                                onClick: () => testSavedProfile(source.id, profile.id),
                              },
                              "测试",
                            ),
                            h(
                              "button",
                              {
                                class: "btn btn-danger btn-small",
                                type: "button",
                                onClick: () => deleteProfile(source.id, profile),
                              },
                              "删除",
                            ),
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
      ),
      h(
        "div",
        { class: "detail-section" },
        h(
          "div",
          { class: "section-head" },
          h("h3", { class: "section-title" }, "工具路由"),
          h(
            "button",
            {
              class: "btn btn-secondary btn-small",
              type: "button",
              onClick: () => openRouteModal(source.id, profiles),
            },
            icon("plus", 14),
            "添加工具路由",
          ),
        ),
        h("p", { class: "field-hint" }, "工具路由表示这个工具使用哪套连接配置。"),
        routes.length === 0
          ? h("div", { class: "callout" }, "还没有工具路由；即使有授权，agent 也无法调用对应工具。")
          : h(
              "div",
              { class: "table-card" },
              h(
                "div",
                { class: "table-scroll" },
                h(
                  "table",
                  {},
                  h(
                    "thead",
                    {},
                    h(
                      "tr",
                      {},
                      h("th", {}, "工具"),
                      h("th", {}, "连接配置"),
                      h("th", {}, "最低策略"),
                      h("th", {}, "版本"),
                      h("th", { class: "th-actions" }, "操作"),
                    ),
                  ),
                  h(
                    "tbody",
                    {},
                    routes.map((route) =>
                      h(
                        "tr",
                        {},
                        h("td", {}, h("span", { class: "badge tool-badge" }, route.tool)),
                        h(
                          "td",
                          {},
                          route.profile_name || profileNames.get(route.profile_id) || `#${route.profile_id}`,
                        ),
                        h(
                          "td",
                          {},
                          h(
                            "span",
                            { class: `badge policy-${route.min_approval_policy}` },
                            policyLabel(route.min_approval_policy),
                          ),
                        ),
                        h("td", { class: "cell-date" }, `v${route.version}`),
                        h(
                          "td",
                          {},
                          h(
                            "div",
                            { class: "cell-actions" },
                            h(
                              "button",
                              {
                                class: "btn btn-secondary btn-small",
                                type: "button",
                                onClick: () => openRouteModal(source.id, profiles, route),
                              },
                              "编辑",
                            ),
                            h(
                              "button",
                              {
                                class: "btn btn-danger btn-small",
                                type: "button",
                                onClick: () => deleteRoute(source.id, route),
                              },
                              "删除",
                            ),
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
      ),
    ),
    h(
      "div",
      { class: "modal-foot" },
      source.status === "archived"
        ? h(
            "button",
            {
              class: "btn btn-secondary",
              type: "button",
              onClick: () => openEnvironmentModal(source),
            },
            "修改环境分类",
          )
        : null,
      source.status === "active"
        ? h(
            "button",
            {
              class: "btn btn-danger",
              type: "button",
              onClick: () => openArchiveModal(source),
            },
            "归档",
          )
        : h(
            "button",
            {
              class: "btn btn-primary",
              type: "button",
              onClick: () => openRestoreModal(source),
            },
            "恢复",
          ),
      h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "关闭"),
    ),
  );
}

async function testSavedProfile(sourceId, profileId) {
  try {
    const data = await api(
      `/admin/api/sources/${encodeURIComponent(sourceId)}/profiles/${profileId}/test`,
      { method: "POST" },
    );
    showToast(
      `连接成功：${data.database} / ${data.user} / PostgreSQL ${data.serverVersion}（${data.latencyMs} ms）`,
    );
  } catch (error) {
    showToast(errorMessage(error), "error");
  }
}

async function deleteProfile(sourceId, profile) {
  if (!window.confirm(`确认删除连接配置「${profile.name}」？仍被工具路由或未完成审批引用时会被拒绝。`)) {
    return;
  }
  try {
    await api(
      `/admin/api/sources/${encodeURIComponent(sourceId)}/profiles/${profile.id}`,
      { method: "DELETE" },
    );
    showToast("连接配置已删除");
    await reloadSourcesPage();
    await openSourceDetail(sourceId);
  } catch (error) {
    showToast(errorMessage(error), "error");
  }
}

async function deleteRoute(sourceId, route) {
  if (!window.confirm(`确认删除工具路由「${route.tool}」？删除后 agent 将无法调用该工具。`)) {
    return;
  }
  try {
    await api(
      `/admin/api/sources/${encodeURIComponent(sourceId)}/routes/${route.id}`,
      { method: "DELETE" },
    );
    showToast("工具路由已删除");
    await reloadSourcesPage();
    await openSourceDetail(sourceId);
  } catch (error) {
    showToast(errorMessage(error), "error");
  }
}

function openProfileModal(sourceId, profile) {
  const editing = Boolean(profile);
  let busy = false;
  const nameInput = h("input", {
    class: "input",
    id: "profile-name",
    placeholder: "例如 query_ro",
    maxlength: "64",
    autocomplete: "off",
    value: editing ? profile.name : "",
  });
  const purposeSelect = h(
    "select",
    { class: "select", id: "profile-purpose", disabled: editing },
    Object.entries(PURPOSE_LABELS).map(([value, label]) =>
      h(
        "option",
        { value, selected: editing && profile.purpose === value },
        label,
      ),
    ),
  );
  const dsnInput = h("input", {
    class: "input",
    id: "profile-dsn",
    type: "password",
    placeholder: editing
      ? "留空表示沿用现有连接串"
      : "postgres://用户名:密码@127.0.0.1:5432/数据库名",
    maxlength: "2048",
    autocomplete: "new-password",
  });
  const enabledInput = h("input", {
    type: "checkbox",
    id: "profile-enabled",
    checked: editing ? profile.enabled : true,
  });
  const maxInput = h("input", {
    class: "input",
    id: "profile-max-connections",
    inputmode: "numeric",
    placeholder: "默认 5",
    value: editing && profile.max_connections !== null ? String(profile.max_connections) : "",
  });
  const timeoutInput = h("input", {
    class: "input",
    id: "profile-timeout",
    inputmode: "numeric",
    placeholder: "默认 30000",
    value:
      editing && profile.statement_timeout_ms !== null
        ? String(profile.statement_timeout_ms)
        : "",
  });
  const errorEl = h("p", { class: "form-error hidden" });
  const testResultEl = h("p", { class: "field-hint hidden" });
  const testButton = h(
    "button",
    {
      class: "btn btn-secondary",
      type: "button",
      onClick: async () => {
        const dsn = dsnInput.value.trim();
        if (!editing && !/^postgres(ql)?:\/\//.test(dsn)) {
          errorEl.textContent = "请先填写数据库连接串，再测试连接";
          errorEl.classList.remove("hidden");
          return;
        }
        testButton.disabled = true;
        testResultEl.classList.add("hidden");
        try {
          const data = dsn
            ? await api("/admin/api/connection-test", { method: "POST", body: { dsn } })
            : await api(
                `/admin/api/sources/${encodeURIComponent(sourceId)}/profiles/${profile.id}/test`,
                { method: "POST" },
              );
          testResultEl.textContent = `连接成功：${data.database} / ${data.user} / PostgreSQL ${data.serverVersion}（${data.latencyMs} ms）`;
          testResultEl.classList.remove("hidden");
        } catch (error) {
          errorEl.textContent = errorMessage(error);
          errorEl.classList.remove("hidden");
        } finally {
          testButton.disabled = false;
        }
      },
    },
    "测试连接",
  );
  const submit = h(
    "button",
    { class: "btn btn-primary", type: "submit" },
    editing ? "保存修改" : "添加连接配置",
  );

  const form = h(
    "form",
    {
      class: "modal-form",
      onSubmit: async (event) => {
        event.preventDefault();
        if (busy) return;
        errorEl.classList.add("hidden");
        const name = nameInput.value.trim();
        const dsn = dsnInput.value.trim();
        if (!name) {
          errorEl.textContent = "请输入连接配置名称";
          errorEl.classList.remove("hidden");
          nameInput.focus();
          return;
        }
        if ((!editing && !dsn) || (dsn && !/^postgres(ql)?:\/\//.test(dsn))) {
          errorEl.textContent = "请输入 PostgreSQL 连接串，例如 postgres://用户:密码@主机:5432/数据库名";
          errorEl.classList.remove("hidden");
          dsnInput.focus();
          return;
        }
        const maxConnections = maxInput.value.trim();
        const timeout = timeoutInput.value.trim();
        if (maxConnections && !/^[0-9]+$/.test(maxConnections)) {
          errorEl.textContent = "连接上限必须是正整数";
          errorEl.classList.remove("hidden");
          return;
        }
        if (timeout && !/^[0-9]+$/.test(timeout)) {
          errorEl.textContent = "语句超时必须是正整数毫秒";
          errorEl.classList.remove("hidden");
          return;
        }
        busy = true;
        submit.disabled = true;
        submit.textContent = editing ? "保存中…" : "添加中…";
        try {
          const url = editing
            ? `/admin/api/sources/${encodeURIComponent(sourceId)}/profiles/${profile.id}`
            : `/admin/api/sources/${encodeURIComponent(sourceId)}/profiles`;
          await api(url, {
            method: editing ? "PUT" : "POST",
            body: {
              name,
              purpose: purposeSelect.value,
              enabled: enabledInput.checked,
              dsn: dsn || null,
              max_connections: maxConnections ? Number(maxConnections) : null,
              statement_timeout_ms: timeout ? Number(timeout) : null,
            },
          });
          closeModal();
          showToast(editing ? "连接配置已保存，版本号已递增" : "连接配置已添加");
          await reloadSourcesPage();
          await openSourceDetail(sourceId);
        } catch (error) {
          busy = false;
          submit.disabled = false;
          submit.textContent = editing ? "保存修改" : "添加连接配置";
          errorEl.textContent = errorMessage(error);
          errorEl.classList.remove("hidden");
        }
      },
    },
    h(
      "div",
      { class: "modal-body" },
      h(
        "div",
        { class: "form-grid" },
        h(
          "div",
          { class: "field" },
          h("label", { class: "field-label", for: "profile-name" }, "名称"),
          nameInput,
        ),
        h(
          "div",
          { class: "field" },
          h("label", { class: "field-label", for: "profile-purpose" }, "用途"),
          purposeSelect,
        ),
      ),
      h(
        "div",
        { class: "field" },
        h("label", { class: "field-label", for: "profile-dsn" }, "数据库连接串"),
        dsnInput,
        h(
          "div",
          { class: "field-hint" },
          editing
            ? "留空表示沿用现有加密凭证；填写新连接串会在保存时轮换凭证并递增版本号。"
            : "直接填写该数据源的 PostgreSQL 连接串；服务端会立即加密保存，页面不会回显。请使用与该用途匹配的账号（查询用只读账号，写入用读写账号，建库用具备 CREATEDB 权限的管理账号）。",
        ),
        testResultEl,
      ),
      h(
        "div",
        { class: "field" },
        h(
          "label",
          { class: "field-label", for: "profile-enabled" },
          h("span", {}, enabledInput),
          " 启用这个连接配置",
        ),
        h("div", { class: "field-hint" }, "停用后，使用该配置的工具路由会拒绝新调用。"),
      ),
      h(
        "div",
        { class: "form-grid" },
        h(
          "div",
          { class: "field" },
          h("label", { class: "field-label", for: "profile-max-connections" }, "最大连接数"),
          maxInput,
        ),
        h(
          "div",
          { class: "field" },
          h("label", { class: "field-label", for: "profile-timeout" }, "语句超时（ms）"),
          timeoutInput,
        ),
      ),
      h(
        "div",
        { class: "callout callout-warning" },
        "查询用途连接配置的实际只读性取决于数据库账号权限和连接只读设置；多个连接配置复用同一凭证不会产生数据库级隔离。",
      ),
      errorEl,
    ),
    h(
      "div",
      { class: "modal-foot" },
      testButton,
      h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "取消"),
      submit,
    ),
  );

  openModal(
    h(
      "div",
      { class: "modal" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, editing ? "编辑连接配置" : "添加连接配置"),
        h("p", { class: "modal-subtitle" }, sourceId),
      ),
      form,
    ),
  );
  nameInput.focus();
}

function openRouteModal(sourceId, profiles, route) {
  const editing = Boolean(route);
  let busy = false;
  const toolSelect = h(
    "select",
    { class: "select", id: "route-tool", disabled: editing },
    TOOLS.map((tool) =>
      h(
        "option",
        { value: tool, selected: editing && route.tool === tool },
        TOOL_LABELS[tool],
      ),
    ),
  );
  const profileSelect = h("select", { class: "select", id: "route-profile" });
  const policySelect = h(
    "select",
    { class: "select", id: "route-policy" },
    h("option", { value: "direct" }, "直接执行"),
    h("option", { value: "approval" }, "逐次审批"),
  );
  const hint = h("div", { class: "field-hint" });
  const errorEl = h("p", { class: "form-error hidden" });
  const submit = h(
    "button",
    { class: "btn btn-primary", type: "submit" },
    editing ? "保存修改" : "添加工具路由",
  );

  function renderProfiles() {
    const purpose = TOOL_PURPOSE[toolSelect.value];
    const matching = profiles.filter(
      (profile) =>
        profile.purpose === purpose &&
        (profile.enabled || (editing && route.profile_id === profile.id)),
    );
    profileSelect.replaceChildren(
      ...matching.map((profile) =>
        h(
          "option",
          { value: String(profile.id), selected: editing && route.profile_id === profile.id },
          `${profile.name} · #${profile.id}`,
        ),
      ),
    );
    profileSelect.disabled = matching.length === 0;
    hint.textContent =
      matching.length === 0
        ? `该工具需要「${purposeLabel(purpose)}」用途的连接配置，请先添加。`
        : `该工具固定使用「${purposeLabel(purpose)}」用途的连接配置。`;
    if (editing) {
      policySelect.value = route.min_approval_policy;
      policySelect.disabled = route.tool === "create_database";
      return;
    }
    const requiresApproval = ["mutate_sql", "execute_ddl", "create_database"].includes(
      toolSelect.value,
    );
    policySelect.value = requiresApproval ? "approval" : "direct";
    policySelect.disabled = toolSelect.value === "create_database";
    if (toolSelect.value === "create_database") policySelect.value = "approval";
  }

  toolSelect.addEventListener("change", renderProfiles);
  renderProfiles();

  const form = h(
    "form",
    {
      class: "modal-form",
      onSubmit: async (event) => {
        event.preventDefault();
        if (busy) return;
        errorEl.classList.add("hidden");
        if (!profileSelect.value) {
          errorEl.textContent = "没有用途匹配的连接配置";
          errorEl.classList.remove("hidden");
          return;
        }
        busy = true;
        submit.disabled = true;
        submit.textContent = editing ? "保存中…" : "添加中…";
        try {
          const url = editing
            ? `/admin/api/sources/${encodeURIComponent(sourceId)}/routes/${route.id}`
            : `/admin/api/sources/${encodeURIComponent(sourceId)}/routes`;
          await api(url, {
            method: editing ? "PUT" : "POST",
            body: {
              tool: toolSelect.value,
              profile_id: Number(profileSelect.value),
              min_approval_policy: policySelect.value,
            },
          });
          closeModal();
          showToast(editing ? "路由已保存，版本号已递增" : "路由已添加");
          await reloadSourcesPage();
          await openSourceDetail(sourceId);
        } catch (error) {
          busy = false;
          submit.disabled = false;
          submit.textContent = editing ? "保存修改" : "添加工具路由";
          errorEl.textContent = errorMessage(error);
          errorEl.classList.remove("hidden");
        }
      },
    },
    h(
      "div",
      { class: "modal-body" },
      h(
        "div",
        { class: "field" },
        h("label", { class: "field-label", for: "route-tool" }, "工具"),
        toolSelect,
      ),
      h(
        "div",
        { class: "field" },
        h("label", { class: "field-label", for: "route-profile" }, "连接配置"),
        profileSelect,
        hint,
      ),
      h(
        "div",
        { class: "field" },
        h("label", { class: "field-label", for: "route-policy" }, "最低审批策略"),
        policySelect,
        h(
          "div",
          { class: "field-hint" },
          "实际策略取授权策略、工具路由最低策略和内置高危规则中的更严格者；create_database 始终需要审批。",
        ),
      ),
      errorEl,
    ),
    h(
      "div",
      { class: "modal-foot" },
      h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "取消"),
      submit,
    ),
  );

  openModal(
    h(
      "div",
      { class: "modal" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, editing ? "编辑工具路由" : "添加工具路由"),
        h("p", { class: "modal-subtitle" }, sourceId),
      ),
      form,
    ),
  );
}

function openEnvironmentModal(source) {
  const select = h(
    "select",
    { class: "select", id: "source-environment" },
    h("option", { value: "nonproduction" }, "非生产"),
    h("option", { value: "production" }, "生产"),
  );
  select.value = source.environment;
  const errorEl = h("p", { class: "form-error hidden" });
  const submit = h("button", { class: "btn btn-primary", type: "button" }, "保存环境");
  submit.addEventListener("click", async () => {
    submit.disabled = true;
    submit.textContent = "保存中…";
    try {
      await api(`/admin/api/sources/${encodeURIComponent(source.id)}/environment`, {
        method: "POST",
        body: { environment: select.value },
      });
      closeModal();
      showToast("环境分类已更新");
      await reloadSourcesPage();
      await openSourceDetail(source.id);
    } catch (error) {
      submit.disabled = false;
      submit.textContent = "保存环境";
      errorEl.textContent = errorMessage(error);
      errorEl.classList.remove("hidden");
    }
  });

  openModal(
    h(
      "div",
      { class: "modal" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, "修改环境分类"),
        h("p", { class: "modal-subtitle" }, "只有已归档数据源可以修改环境分类。"),
      ),
      h(
        "div",
        { class: "modal-body" },
        h("div", { class: "field" }, h("label", { class: "field-label", for: "source-environment" }, "环境"), select),
        h(
          "div",
          { class: "callout callout-warning" },
          "标记为生产前必须先撤销该数据源上的 mutate_sql、execute_ddl 和 create_database 授权。",
        ),
        errorEl,
      ),
      h(
        "div",
        { class: "modal-foot" },
        h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "取消"),
        submit,
      ),
    ),
  );
}

function openArchiveModal(source) {
  const errorEl = h("p", { class: "form-error hidden" });
  const submit = h(
    "button",
    {
      class: "btn btn-danger",
      type: "button",
      onClick: async () => {
        submit.disabled = true;
        submit.textContent = "归档中…";
        try {
          await api(`/admin/api/sources/${encodeURIComponent(source.id)}/archive`, {
            method: "POST",
          });
          closeModal();
          showToast("数据源已归档");
          await reloadSourcesPage();
        } catch (error) {
          submit.disabled = false;
          submit.textContent = "归档数据源";
          errorEl.textContent = errorMessage(error);
          errorEl.classList.remove("hidden");
        }
      },
    },
    "归档数据源",
  );

  openModal(
    h(
      "div",
      { class: "modal" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, "归档数据源"),
        h("p", { class: "modal-subtitle" }, source.id),
      ),
      h(
        "div",
        { class: "modal-body" },
        h(
          "div",
          { class: "callout callout-danger" },
          "归档会阻止新请求、使待批请求失效，并从 agent 的数据源列表移除。配置、授权和审计保留，物理数据库不会被删除。",
        ),
        errorEl,
      ),
      h(
        "div",
        { class: "modal-foot" },
        h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "取消"),
        submit,
      ),
    ),
  );
}

function openRestoreModal(source) {
  const errorEl = h("p", { class: "form-error hidden" });
  const submit = h(
    "button",
    {
      class: "btn btn-primary",
      type: "button",
      onClick: async () => {
        submit.disabled = true;
        submit.textContent = "恢复中…";
        try {
          await api(`/admin/api/sources/${encodeURIComponent(source.id)}/restore`, {
            method: "POST",
          });
          closeModal();
          showToast("数据源已恢复");
          await reloadSourcesPage();
        } catch (error) {
          submit.disabled = false;
          submit.textContent = "恢复数据源";
          errorEl.textContent = errorMessage(error);
          errorEl.classList.remove("hidden");
        }
      },
    },
    "恢复数据源",
  );

  openModal(
    h(
      "div",
      { class: "modal" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, "恢复数据源"),
        h("p", { class: "modal-subtitle" }, source.id),
      ),
      h(
        "div",
        { class: "modal-body" },
        h(
          "div",
          { class: "callout" },
          "恢复前会复核启用的连接配置、授权和适用建库配额。任一条件不满足时恢复失败。",
        ),
        errorEl,
      ),
      h(
        "div",
        { class: "modal-foot" },
        h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "取消"),
        submit,
      ),
    ),
  );
}

function openSettleModal(reservation, target) {
  const commit = target === "commit";
  const errorEl = h("p", { class: "form-error hidden" });
  const submit = h(
    "button",
    {
      class: `btn ${commit ? "btn-primary" : "btn-danger"}`,
      type: "button",
      onClick: async () => {
        submit.disabled = true;
        submit.textContent = "处理中…";
        try {
          await api(`/admin/api/reservations/${reservation.id}/${target}`, { method: "POST" });
          closeModal();
          showToast(commit ? "预留已提交" : "预留已释放");
          await refreshView();
        } catch (error) {
          submit.disabled = false;
          submit.textContent = commit ? "提交预留" : "释放预留";
          errorEl.textContent = errorMessage(error);
          errorEl.classList.remove("hidden");
        }
      },
    },
    commit ? "提交预留" : "释放预留",
  );

  openModal(
    h(
      "div",
      { class: "modal" },
      h(
        "div",
        { class: "modal-head" },
        h("h2", { class: "modal-title" }, commit ? "提交隔离预留" : "释放隔离预留"),
        h("p", { class: "modal-subtitle" }, `#${reservation.id} · ${reservation.database_name}`),
      ),
      h(
        "div",
        { class: "modal-body" },
        h(
          "div",
          { class: "callout callout-warning" },
          commit
            ? "只有确认数据库已创建并完成本地登记时才提交；提交要求对应数据源处于启用状态。"
            : "只有确认数据库未创建或已由人工清理时才释放；释放会恢复该令牌 × 父数据源的配额。",
        ),
        errorEl,
      ),
      h(
        "div",
        { class: "modal-foot" },
        h("button", { class: "btn btn-secondary", type: "button", onClick: closeModal }, "取消"),
        submit,
      ),
    ),
  );
}

// ---------- Shared states ----------

function emptyState(iconName, title, description, action) {
  return h(
    "div",
    { class: "table-card" },
    h(
      "div",
      { class: "empty-state" },
      h("div", { class: "empty-icon" }, icon(iconName, 20)),
      h("h2", { class: "empty-title" }, title),
      h("p", { class: "empty-desc" }, description),
      action || null,
    ),
  );
}

function openModal(content, options = {}) {
  modalDismissible = options.dismissible !== false;
  const root = document.getElementById("modal-root");
  root.hidden = false;
  root.replaceChildren(
    h(
      "div",
      {
        class: "modal-overlay",
        onClick: (event) => {
          if (modalDismissible && event.target === event.currentTarget) closeModal();
        },
      },
      content,
    ),
  );
  document.body.classList.add("modal-open");
  const focusable = root.querySelector("input, select, textarea, button");
  if (focusable) focusable.focus();
}

function closeModal() {
  const root = document.getElementById("modal-root");
  if (!root) return;
  root.hidden = true;
  root.replaceChildren();
  document.body.classList.remove("modal-open");
  modalDismissible = true;
}

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && modalDismissible) {
    const root = document.getElementById("modal-root");
    if (root && !root.hidden) closeModal();
  }
});

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.className = "clipboard-area";
    document.body.appendChild(area);
    area.select();
    document.execCommand("copy");
    area.remove();
  }
  showToast("已复制到剪贴板");
}

function showToast(message, type = "ok") {
  const root = document.getElementById("toast-root");
  if (!root) return;
  const toast = h(
    "div",
    { class: `toast${type === "error" ? " error" : ""}`, role: "status" },
    message,
  );
  root.appendChild(toast);
  window.setTimeout(() => toast.remove(), 3200);
}

// ---------- Login ----------

async function showLogin(forceFreshChallenge) {
  state.session = false;
  state.csrf = "";
  stopPolling();
  closeModal();
  state.loginChallenge = pageLoginChallenge();
  if (forceFreshChallenge || !state.loginChallenge) {
    try {
      await freshLoginChallenge();
    } catch {
      state.loginChallenge = "";
    }
  }
  renderLogin();
}

function renderLogin() {
  stopPolling();
  const root = document.getElementById("root");
  const input = h("input", {
    class: "input",
    id: "admin-token",
    type: "password",
    autocomplete: "off",
    placeholder: "输入管理员密钥或一次性登录令牌",
  });
  const errorEl = h("p", { class: "form-error hidden" });
  const submit = h("button", { class: "btn btn-primary", type: "submit" }, "登录");

  const form = h(
    "form",
    {
      class: "login-form",
      onSubmit: async (event) => {
        event.preventDefault();
        errorEl.classList.add("hidden");
        const token = input.value.trim();
        if (!token) {
          errorEl.textContent = "请输入一次性登录令牌";
          errorEl.classList.remove("hidden");
          return;
        }
        if (!state.loginChallenge) {
          try {
            await freshLoginChallenge();
          } catch (error) {
            errorEl.textContent = errorMessage(error);
            errorEl.classList.remove("hidden");
            return;
          }
        }
        submit.disabled = true;
        submit.textContent = "登录中…";
        try {
          const data = await api("/admin/api/login", {
            method: "POST",
            csrf: state.loginChallenge,
            body: { token },
          });
          state.session = true;
          state.csrf = data.csrf || "";
          state.loginChallenge = "";
          await enterApp();
        } catch (error) {
          submit.disabled = false;
          submit.textContent = "登录";
          errorEl.textContent = errorMessage(error);
          errorEl.classList.remove("hidden");
          input.focus();
          input.select();
        }
      },
    },
    h("label", { class: "field-label", for: "admin-token" }, "管理员密钥或登录令牌"),
    input,
    errorEl,
    submit,
  );

  root.replaceChildren(
    h(
      "div",
      { class: "login-page" },
      h(
        "div",
        { class: "login-card" },
        h(
          "div",
          { class: "login-brand" },
          h("div", { class: "brand-mark" }, "db"),
          h("div", { class: "brand-name" }, "mcp-db"),
          h("span", { class: "brand-badge" }, "管理台"),
        ),
        h("h1", { class: "login-title" }, "管理员登录"),
        h(
          "p",
          { class: "login-desc" },
          "在服务所在主机运行 mcp-db admin-login-token 获取一次性令牌（5 分钟有效），或使用 mcp-db admin-key create 创建的永久管理员密钥登录。",
        ),
        form,
        h(
          "p",
          { class: "login-foot" },
          "管理面仅监听回环地址；agent 的 Bearer 令牌不能登录管理台。",
        ),
      ),
    ),
  );
  input.focus();
}

// ---------- Start ----------

void boot();
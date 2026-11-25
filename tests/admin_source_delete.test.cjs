const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const appPath = join(__dirname, "../src/transport/admin_assets/app.js");
// Drive the real delete dialog and sources table without starting the SPA's session check.
const source = readFileSync(appPath, "utf8").replace(/^void boot\(\);$/m, "");

let activeDocument;
let confirmCalls = 0;

class Element {
  constructor(tag, text = "") {
    this.tag = tag;
    this.text = text;
    this.children = [];
    this.attrs = {};
    this.listeners = {};
    this.className = "";
    this.hidden = false;
    this.disabled = false;
    this.classList = {
      add: (name) => this.classList.toggle(name, true),
      remove: (name) => this.classList.toggle(name, false),
      toggle: (name, on) => {
        const names = new Set(this.className.split(/\s+/).filter(Boolean));
        if (on) names.add(name);
        else names.delete(name);
        this.className = [...names].join(" ");
      },
    };
  }
  get isConnected() {
    return this.tag === "body" || Boolean(this.parent?.isConnected);
  }
  get textContent() {
    return this.text + this.children.map((child) => child.textContent).join("");
  }
  set textContent(text) {
    this.replaceChildren();
    this.text = text;
  }
  setAttribute(name, value) { this.attrs[name] = value; }
  appendChild(child) { this.children.push(child); child.parent = this; }
  append(...nodes) { for (const node of nodes) this.appendChild(node); }
  replaceChildren(...nodes) {
    for (const child of this.children) child.parent = undefined;
    this.children = [];
    this.text = "";
    for (const node of nodes) {
      if (node instanceof Element) this.appendChild(node);
      else this.appendChild(new Element("#text", String(node)));
    }
  }
  find(predicate) {
    if (predicate(this)) return this;
    for (const child of this.children) {
      const found = child.find(predicate);
      if (found) return found;
    }
  }
  querySelector(selectors) {
    const tags = selectors.split(/,\s*/);
    return this.find((node) => tags.includes(node.tag));
  }
  querySelectorAll(selectors) {
    const tags = selectors.split(/,\s*/);
    const found = [];
    const walk = (node) => {
      if (tags.includes(node.tag)) found.push(node);
      for (const child of node.children) walk(child);
    };
    walk(this);
    return found;
  }
  addEventListener(type, handler) { this.listeners[type] = handler; }
  fire(type, fields) {
    if (type === "click" && this.disabled) return;
    return this.listeners[type]?.({ target: this, currentTarget: this, preventDefault() {}, ...fields });
  }
  focus() { activeDocument.activeElement = this; }
  getClientRects() { return []; }
}

function fixture({ sources }) {
  const body = new Element("body");
  const modal = new Element("div");
  modal.hidden = true;
  for (const [id, node] of [
    ["modal-root", modal],
    ["toast-root", new Element("div")],
    ["view-root", new Element("div")],
  ]) {
    node.setAttribute("id", id);
    body.appendChild(node);
  }
  const document = Object.assign(new Element("#document"), {
    body,
    activeElement: undefined,
    getElementById: (id) => body.find((node) => node.attrs.id === id),
    createElement: (tag) => new Element(tag),
    createElementNS: (_ns, tag) => new Element(tag),
    createTextNode: (text) => new Element("#text", text),
    querySelector: () => null,
    addEventListener: () => {},
  });
  activeDocument = document;
  const requests = [];
  const app = vm.createContext({
    document,
    Node: Element,
    location: { hash: "", pathname: "/admin" },
    history: { replaceState() {} },
    window: {
      addEventListener() {},
      setTimeout() {},
      confirm: () => {
        confirmCalls += 1;
        return false;
      },
    },
    navigator: { clipboard: { writeText: async () => {} } },
    fetch: async (path, options = {}) => {
      requests.push(`${options.method || "GET"} ${path}`);
      if (path === "/admin/api/sources" && (options.method || "GET") === "GET") {
        return { status: 200, ok: true, text: async () => JSON.stringify({ sources }) };
      }
      if (path === "/admin/api/sources/leaf" && options.method === "DELETE") {
        return { status: 200, ok: true, text: async () => JSON.stringify({ deleted: true, warnings: [] }) };
      }
      assert.fail(`unexpected request ${options.method || "GET"} ${path}`);
    },
  });
  vm.runInContext(source, app, { filename: appPath });
  return { app, body, document, modal, requests };
}

const active = (id, environment = "nonproduction") => ({
  id,
  environment,
  status: "active",
  profile_count: 1,
  grant_count: 1,
  created_at: 0,
  grant_tools: [],
});
const archived = (id, environment = "nonproduction") => ({ ...active(id, environment), status: "archived" });

const deleteButtonsOf = (root) =>
  root.querySelectorAll("button").filter((node) => node.textContent === "删除");

test("the sources table renders a delete button for active and archived sources", async () => {
  const { app, body, requests } = fixture({ sources: [active("alpha"), archived("gamma")] });
  await app.loadSources();
  app.renderSources(body.find((node) => node.attrs.id === "view-root"));

  const deletes = deleteButtonsOf(body);
  assert.equal(deletes.length, 2, "both an active and an archived source get a delete button");
  assert.deepEqual(
    requests.filter((request) => request.startsWith("DELETE")),
    [],
    "rendering must not delete anything",
  );
  assert.equal(confirmCalls, 0, "the table must not rely on window.confirm");

  const archivedRow = body
    .querySelectorAll("tr")
    .find((row) => row.textContent.includes("gamma"));
  assert.ok(archivedRow, "archived source renders a row");
  const rowButtons = archivedRow.querySelectorAll("button").map((node) => node.textContent);
  assert.ok(rowButtons.includes("恢复"), "archived source keeps its restore button");
  assert.ok(rowButtons.includes("删除"), "archived source also gets a delete button");

  const activeRow = body.querySelectorAll("tr").find((row) => row.textContent.includes("alpha"));
  const activeButtons = activeRow.querySelectorAll("button");
  assert.ok(
    activeButtons.some(
      (node) => node.attrs["aria-label"] === "归档 alpha" || node.textContent === "归档",
    ),
    "active source keeps its archive button",
  );
  assert.ok(
    activeButtons.some((node) => node.textContent === "删除"),
    "active source also gets a delete button",
  );
});

test("the delete dialog stays disabled until the exact id is typed", async () => {
  const { app, body } = fixture({ sources: [active("leaf")] });
  const leaf = active("leaf");
  await app.openDeleteSourceModal(leaf);

  const modal = body.find((node) => node.attrs.id === "modal-root");
  assert.equal(modal.hidden, false, "the dialog opens");
  const input = body.find((node) => node.attrs.id === "delete-source-confirm");
  assert.ok(input, "the confirmation input exists");
  const submit = body
    .querySelectorAll("button")
    .find((node) => node.textContent === "删除数据源");
  assert.ok(submit, "the confirm button exists");
  assert.equal(submit.disabled, true, "an empty input keeps the button disabled");
  assert.ok(
    modal.textContent.includes("物理数据库不会被删除"),
    "the dialog states the physical database is untouched",
  );

  input.value = "leaf2";
  input.fire("input");
  assert.equal(submit.disabled, true, "a wrong id keeps the button disabled");

  input.value = "leaf";
  input.fire("input");
  assert.equal(submit.disabled, false, "the exact id enables the button");

  input.value = " leaf ";
  input.fire("input");
  assert.equal(submit.disabled, false, "the id check trims whitespace");
});

test("confirming sends exactly one DELETE and closes the dialog", async () => {
  const { app, body, modal, requests } = fixture({ sources: [active("leaf")] });
  await app.openDeleteSourceModal(active("leaf"));

  const input = body.find((node) => node.attrs.id === "delete-source-confirm");
  const submit = body
    .querySelectorAll("button")
    .find((node) => node.textContent === "删除数据源");

  input.value = "leaf";
  input.fire("input");
  assert.equal(submit.disabled, false);
  await submit.fire("click");
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(
    requests,
    [
      "DELETE /admin/api/sources/leaf",
      // Success reloads the sources list so the table drops the deleted row.
      "GET /admin/api/sources",
    ],
    "exactly one DELETE plus the list reload, nothing else",
  );
  assert.equal(modal.hidden, true, "success closes the dialog");
  assert.equal(body.find((node) => node.attrs.id === "modal-root").children.length, 0,
    "the modal content is cleared");
});

test("a failed delete keeps the dialog open and shows the error", async () => {
  const body = new Element("body");
  const modal = new Element("div");
  modal.hidden = true;
  for (const [id, node] of [
    ["modal-root", modal],
    ["toast-root", new Element("div")],
    ["view-root", new Element("div")],
  ]) {
    node.setAttribute("id", id);
    body.appendChild(node);
  }
  const document = Object.assign(new Element("#document"), {
    body,
    activeElement: undefined,
    getElementById: (id) => body.find((node) => node.attrs.id === id),
    createElement: (tag) => new Element(tag),
    createElementNS: (_ns, tag) => new Element(tag),
    createTextNode: (text) => new Element("#text", text),
    querySelector: () => null,
    addEventListener: () => {},
  });
  activeDocument = document;
  const app = vm.createContext({
    document,
    Node: Element,
    location: { hash: "", pathname: "/admin" },
    history: { replaceState() {} },
    window: { addEventListener() {}, setTimeout() {}, confirm: () => false },
    navigator: { clipboard: { writeText: async () => {} } },
    fetch: async () => ({
      status: 409,
      ok: false,
      text: async () => JSON.stringify({ error: "这个数据源还有 1 个子数据源，请先删除子数据源。" }),
    }),
  });
  vm.runInContext(source, app, { filename: appPath });

  await app.openDeleteSourceModal(active("blocked"));
  const input = body.find((node) => node.attrs.id === "delete-source-confirm");
  const submit = body
    .querySelectorAll("button")
    .find((node) => node.textContent === "删除数据源");
  input.value = "blocked";
  input.fire("input");
  await submit.fire("click");
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(modal.hidden, false, "a failure keeps the dialog open");
  assert.equal(submit.textContent, "删除数据源", "the button label is restored");
  assert.equal(submit.disabled, false, "the button is clickable again");
  const error = body.find((node) => typeof node.className === "string" && node.className.includes("form-error"));
  assert.ok(error && !error.className.includes("hidden"), "the error is revealed");
  assert.ok(
    error.textContent.includes("子数据源"),
    "the server's rejection reason is shown verbatim",
  );
});

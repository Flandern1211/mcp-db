const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const appPath = join(__dirname, "../src/transport/admin_assets/app.js");
// Drive the real grants dialog without starting the SPA's session check.
const source = readFileSync(appPath, "utf8").replace(/^void boot\(\);$/m, "");
const token = { id: "mock-grants-token-id-000000000001", agent: "mock-agent", status: "active" };
const response = (data) => ({ status: 200, ok: true, text: async () => JSON.stringify(data) });

let activeDocument;

class Element {
  constructor(tag, text = "") {
    this.tag = tag;
    this.text = text;
    this.children = [];
    this.attrs = {};
    this.listeners = {};
    this.className = "";
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
  // Mirrors HTMLSelectElement: only option descendants are exposed.
  get options() {
    return this.children.filter((child) => child.tag === "option");
  }
  get textContent() {
    return this.text + this.children.map((child) => child.textContent).join("");
  }
  set textContent(text) {
    this.replaceChildren();
    this.text = text;
  }
  // Mirrors HTMLSelectElement.value / HTMLOptionElement.value: real properties,
  // not attrs, so tests cannot pass on an `undefined` read.
  get value() {
    if (this.tag === "select") {
      const options = this.options;
      const chosen = options.find((option) => option.selected === true) ?? options[0];
      return chosen ? chosen.value : "";
    }
    return this._value ?? "";
  }
  set value(value) {
    const text = String(value);
    if (this.tag === "select") {
      for (const option of this.options) option.selected = option.value === text;
      return;
    }
    this._value = text;
  }
  setAttribute(name, value) { this.attrs[name] = value; }
  appendChild(child) { this.children.push(child); child.parent = this; }
  append(...nodes) { for (const node of nodes) this.appendChild(node); }
  // Faithful to the DOM IDL `(Node or DOMString)...`: anything that is not a
  // node is stringified into a text node instead of being flattened.
  replaceChildren(...nodes) {
    for (const child of this.children) child.parent = undefined;
    this.children = [];
    this.text = "";
    for (const node of nodes) {
      if (node instanceof Element) this.appendChild(node);
      else this.appendChild(new Element("#text", String(node)));
    }
  }
  remove() {
    if (!this.parent) return;
    this.parent.children.splice(this.parent.children.indexOf(this), 1);
    this.parent = undefined;
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

function fixture({ sources, grants }) {
  const body = new Element("body");
  const modal = new Element("div");
  modal.hidden = true;
  for (const [id, node] of [["modal-root", modal], ["toast-root", new Element("div")], ["view-root", new Element("div")]]) {
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
  });
  activeDocument = document;
  const requests = [];
  const app = vm.createContext({
    document,
    Node: Element,
    window: { addEventListener() {}, setTimeout() {} },
    navigator: { clipboard: { writeText: async () => {} } },
    fetch: async (path, options = {}) => {
      requests.push(`${options.method || "GET"} ${path}`);
      if (path === "/admin/api/sources") return response({ sources });
      if (path === `/admin/api/tokens/${token.id}/grants`) return response({ grants });
      assert.fail(`unexpected request ${path}`);
    },
  });
  vm.runInContext(source, app, { filename: appPath });
  return { app, body, document, modal, requests };
}

const pickerOf = (body) => {
  const node = body.find((item) => item.attrs.id === "new-grant-source");
  assert.ok(node, "the grants dialog renders the new-grant source picker");
  return node;
};
const addButton = (body) => body.find((node) => node.tag === "button" && node.textContent === "选择数据源");

const active = (id, environment) => ({ id, environment, status: "active", grant_tools: [] });
const archived = (id, environment) => ({ id, environment, status: "archived", grant_tools: [] });

test("two enabled sources yield a placeholder option plus one option each", async () => {
  const { app, body } = fixture({
    sources: [active("alpha", "production"), active("beta", "nonproduction"), archived("gamma", "production")],
    grants: [],
  });
  await app.openGrantsModal(token);

  const picker = pickerOf(body);
  assert.equal(picker.options.length, 3, "1 placeholder option plus one option per enabled source");
  assert.deepEqual(picker.options.map((option) => option.value), ["", "alpha", "beta"]);
  assert.equal(picker.options[0].textContent, "请选择未授权的数据源", "the placeholder stays first");
  assert.deepEqual(picker.options.slice(1).map((option) => option.textContent), ["alpha · 生产", "beta · 非生产"]);
  assert.ok(!picker.textContent.includes("[object"), "no option may be stringified into the picker");
  assert.equal(picker.disabled, false, "two choices enable the picker");
  assert.equal(addButton(body).disabled, true, "adding still requires selecting a source");
});

test("only the placeholder keeps the picker and the add button disabled", async () => {
  const { app, body } = fixture({
    sources: [archived("alpha", "production")],
    grants: [],
  });
  await app.openGrantsModal(token);

  const picker = pickerOf(body);
  assert.equal(picker.options.length, 1, "no enabled source leaves only the placeholder option");
  assert.deepEqual(picker.options.map((option) => option.value), [""]);
  assert.equal(picker.options[0].textContent, "没有其他启用中的未授权数据源");
  assert.equal(picker.disabled, true, "a picker with fewer than two options stays disabled");
  assert.equal(addButton(body).disabled, true, "a disabled picker disables the add button");
});

test("one enabled source enables the picker but not the add button", async () => {
  const { app, body } = fixture({
    sources: [active("alpha", "production"), archived("beta", "production")],
    grants: [],
  });
  await app.openGrantsModal(token);

  const picker = pickerOf(body);
  assert.equal(picker.options.length, 2, "1 placeholder option plus one option for the single enabled source");
  assert.deepEqual(picker.options.map((option) => option.value), ["", "alpha"]);
  assert.equal(picker.disabled, false, "the gate counts total options, so one choice already enables the picker");
  assert.equal(addButton(body).disabled, true, "adding still requires selecting a source");
});

test("selecting a source enables the add button and clicking adds a card without writing", async () => {
  const { app, body, requests } = fixture({
    sources: [active("beta", "nonproduction"), active("alpha", "production")],
    grants: [],
  });
  await app.openGrantsModal(token);

  const picker = pickerOf(body);
  const add = addButton(body);
  assert.equal(picker.value, "", "the placeholder is selected initially");
  assert.equal(add.disabled, true);

  picker.value = "alpha";
  assert.equal(picker.value, "alpha", "the picker exposes the selected option value");
  assert.deepEqual(picker.options.filter((option) => option.selected).map((option) => option.value), ["alpha"],
    "assigning select.value selects the matching option instead of storing a shadow value");
  picker.fire("change");
  assert.equal(add.disabled, false, "choosing a source enables the add button");
  assert.equal(body.querySelectorAll("details").length, 0, "selection alone adds no card");

  await add.fire("click");
  const cards = body.querySelectorAll("details");
  assert.equal(cards.length, 1, "clicking adds exactly one grant card");
  assert.equal(cards[0].attrs.open, "", "the new card starts expanded");
  assert.equal(cards[0].querySelector("summary")?.querySelector("span")?.textContent, "alpha");
  assert.deepEqual(requests, [
    `GET /admin/api/tokens/${token.id}/grants`,
    "GET /admin/api/sources",
  ], "adding a card makes no request beyond the initial reads");
  assert.equal(picker.value, "", "a rebuilt option list derives value from its options, not a stale shadow field");
});

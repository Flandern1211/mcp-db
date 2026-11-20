const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const { join } = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const appPath = join(__dirname, "../src/transport/admin_assets/app.js");
// The tests drive the actual dialogs without starting the SPA's session check.
const source = readFileSync(appPath, "utf8").replace(/^void boot\(\);$/m, "");
const sample = "mock-only-issued-token";
const token = { id: "mock-id-for-a-listed-token-000000001", agent: "mock-agent", status: "active" };
const flush = () => new Promise(setImmediate);
const response = (status, data) => ({ status, ok: status < 400, text: async () => JSON.stringify(data) });

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

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
    for (const node of nodes) this.appendChild(node);
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
  addEventListener(type, handler) { this.listeners[type] = handler; }
  fire(type, fields) {
    if (type === "click" && this.disabled) return;
    return this.listeners[type]?.({ target: this, currentTarget: this, preventDefault() {}, ...fields });
  }
  focus() { activeDocument.activeElement = this; }
  select() {
    this.selected = true;
    activeDocument.activeElement = this;
  }
}

function fixture({ writeText, fetch = () => assert.fail("unexpected network call") } = {}) {
  const body = new Element("body");
  const modal = new Element("div");
  const toasts = new Element("div");
  const view = new Element("div");
  modal.hidden = true;
  for (const [id, node] of [["modal-root", modal], ["toast-root", toasts], ["view-root", view]]) {
    node.setAttribute("id", id);
    body.appendChild(node);
  }
  const document = Object.assign(new Element("#document"), {
    body,
    getElementById: (id) => body.find((node) => node.attrs.id === id),
    createElement: (tag) => new Element(tag),
    createElementNS: (_ns, tag) => new Element(tag),
    createTextNode: (text) => new Element("#text", text),
  });
  activeDocument = document;
  const app = vm.createContext({
    document,
    Node: Element,
    window: { addEventListener() {}, setTimeout() {} },
    navigator: { clipboard: { writeText } },
    fetch,
  });
  vm.runInContext(source, app, { filename: appPath });
  return { app, body, document, modal, toasts, view };
}

function button(root, label) {
  const node = root.find((node) => node.tag === "button" && node.textContent === label);
  assert.ok(node, `missing button: ${label}`);
  return node;
}

function showsIssuedText(root) {
  return root.isConnected && (
    root.textContent.includes(sample) ||
    Boolean(root.find((node) => node.isConnected && typeof node.value === "string" && node.value.includes(sample)))
  );
}

const tempAreas = (body) => body.children.filter((node) => node.tag === "textarea");

for (const [name, primary, fallback] of [
  ["clipboard succeeds", true, false],
  ["execCommand succeeds after clipboard rejection", false, true],
  ["execCommand returns false after clipboard rejection", false, false],
  ["execCommand throws after clipboard rejection", false, "throw"],
]) {
  test(name, async () => {
    const writes = [];
    const selections = [];
    const { app, body, document, modal, toasts } = fixture({
      writeText: async (text) => {
        writes.push(text === sample);
        if (!primary) throw new Error("mock clipboard rejection");
      },
    });
    app.showCreatedToken({ token, plaintext: sample });
    const field = modal.querySelector("textarea");
    assert.equal(field.textContent, sample, "the read-only field holds the complete token");
    assert.equal(field.attrs.readonly, "", "the field is read-only, not editable");
    document.execCommand = (command) => {
      const [area] = tempAreas(body);
      selections.push(command === "copy" && area?.value === sample && area.selected === true);
      if (fallback === "throw") throw new Error("mock fallback rejection");
      return fallback;
    };

    const [outcome] = await Promise.allSettled([app.copyText(sample, field)]);
    assert.deepEqual(writes, [true], "primary must receive the complete text");
    assert.deepEqual(selections, primary ? [] : [true], "fallback must select the complete text");
    assert.equal(tempAreas(body).length, 0, "temporary textarea must be removed");
    assert.equal(outcome.status, "fulfilled", "copy failures are handled by the dialog");
    assert.equal(activeDocument.activeElement, field, "focus stays on the read-only field");
    const copied = primary || fallback === true;
    assert.equal(field.selected === true, !copied, "a failed copy selects the field for manual copying");
    assert.equal(toasts.textContent.includes("已复制到剪贴板"), copied, "copy success feedback must be accurate");
    assert.equal(/手动.*复制|选中.*复制/.test(toasts.textContent), !copied, "failed copy must offer manual copying");
  });
}

function issuance({ failList = false, failPost = false } = {}) {
  const requests = [];
  const pending = [];
  const reply = ({ method, path }) => {
    if (path === "/admin/api/tokens") {
      if (method === "POST") {
        return failPost
          ? response(400, { error: "mock issuance rejected" })
          : response(201, { token, plaintext: sample });
      }
      return failList ? response(500, { error: "mock list rejected" }) : response(200, { tokens: [token] });
    }
    if (path.endsWith("/grants")) return response(200, { grants: [] });
    if (path === "/admin/api/sources") return response(200, { sources: [] });
    assert.fail(`unexpected request ${method} ${path}`);
  };
  const ui = fixture({
    writeText: async () => {},
    fetch: (path, { method }) => {
      const gate = deferred();
      requests.push(`${method} ${path}`);
      pending.push({ method, path, gate });
      return gate.promise;
    },
  });
  vm.runInContext(`state.view = "tokens"; state.session = true; state.tokens = ${JSON.stringify([token])};`, ui.app);
  ui.app.renderTokens(ui.view);
  const create = button(ui.view, "签发令牌");

  function openForm() {
    create.fire("click");
    const form = ui.modal.querySelector("form");
    assert.ok(form && !ui.modal.hidden, "the issuance form opens");
    form.querySelector("input").value = token.agent;
    return form;
  }

  async function settle(index = 0) {
    const [entry] = pending.splice(index, 1);
    assert.ok(entry, `no pending request at index ${index}`);
    entry.gate.resolve(reply(entry));
    await flush();
    return entry;
  }

  return { ...ui, create, openForm, requests, settle };
}

for (const failList of [false, true]) {
  test(`issuance shows the one-time result before list refresh${failList ? " even when refresh fails" : ""}`, async () => {
    const { app, body, document, modal, toasts, view, openForm, settle, requests } = issuance({ failList });
    const submit = openForm().fire("submit");
    await settle();
    await submit;

    assert.ok(!modal.hidden && modal.textContent.includes("令牌已签发"), "successful issuance must show its result");
    assert.deepEqual(requests, ["POST /admin/api/tokens"], "render the result without making any list request");
    assert.ok(showsIssuedText(modal), "result contains the issued text");
    document.fire("keydown", { key: "Escape" });
    modal.children[0].fire("click");
    assert.ok(showsIssuedText(modal), "one-time result cannot be dismissed accidentally");

    if (!failList) {
      button(modal, "复制令牌").fire("click");
      button(modal, "复制请求头").fire("click");
      await flush();
      assert.ok(modal.textContent.includes(`Authorization: Bearer ${sample}`), "both field values stay complete");
    }

    button(modal, "我已保存，关闭").fire("click");
    assert.ok(modal.hidden && modal.children.length === 0, "closing clears the result immediately");
    await settle();
    assert.deepEqual(
      requests,
      ["POST /admin/api/tokens", "GET /admin/api/tokens"],
      "closing refreshes once and never reissues",
    );
    assert.ok(!body.textContent.includes(sample), "list refresh cannot bring back the result");
    if (failList) {
      assert.ok(toasts.textContent.includes("刷新"), "list failure is reported separately");
    } else {
      assert.ok(
        view.textContent.includes(app.shortId(token.id, 12)) && !view.textContent.includes(token.id),
        "refreshed list shows only the short identifier",
      );
    }
  });
}

test("pending issuance ignores cancellation, accidental dismissal and duplicate submit", async () => {
  const { create, document, modal, openForm, settle, requests } = issuance();
  const form = openForm();
  const submit = form.fire("submit");
  button(form, "取消").fire("click");
  document.fire("keydown", { key: "Escape" });
  if (!modal.hidden) modal.children[0].fire("click");
  create.fire("click");
  await form.fire("submit");
  const stayedOpen = modal.querySelector("form") === form;
  await settle();
  await submit;
  assert.ok(stayedOpen, "pending form cannot be dismissed or replaced by another issuance form");
  assert.deepEqual(requests, ["POST /admin/api/tokens"], "duplicate submission makes no additional request");
});

test("a rejected issuance restores cancellation without refreshing or reissuing", async () => {
  const { document, modal, openForm, settle, requests } = issuance({ failPost: true });
  const form = openForm();
  const submit = form.fire("submit");
  await settle();
  await submit;
  assert.ok(form.textContent.includes("mock issuance rejected"), "POST failure is reported in the form");
  assert.ok(!button(form, "签发令牌").disabled && !button(form, "取消").disabled, "form controls are restored");
  document.fire("keydown", { key: "Escape" });
  assert.ok(modal.hidden, "passive dismissal is restored");
  assert.deepEqual(requests, ["POST /admin/api/tokens"]);
});

for (const grantsFirst of [true, false]) {
  test(`a slow grants response does not cover the issuance ${grantsFirst ? "form" : "result"}`, async () => {
    const { app, modal, openForm, view, settle, requests } = issuance();
    // With a cached source, the legacy dialog needs only the grants response to render.
    vm.runInContext('state.sources = [{ id: "cached-source", status: "archived", grant_tools: [] }];', app);
    button(view, "管理授权").fire("click");
    const reads = [`GET /admin/api/tokens/${token.id}/grants`];
    if (!modal.hidden) {
      reads.push("GET /admin/api/sources");
      assert.equal(modal.querySelector("h2")?.textContent, "管理授权", "the grouped dialog opens while reads are pending");
      button(modal, "关闭").fire("click");
      assert.ok(modal.hidden && modal.children.length === 0, "close the grouped dialog before either response arrives");
    }
    assert.deepEqual(requests, reads, "clicking manage grants starts the expected reads before issuance");

    const form = openForm();
    const submit = form.fire("submit");
    const expectedRequests = [...reads, "POST /admin/api/tokens"];
    assert.deepEqual(requests, expectedRequests);

    function assertIssued() {
      assert.ok(!modal.hidden && modal.querySelector("h2")?.textContent === "令牌已签发" && showsIssuedText(modal),
        "the issuance result stays visible with its one-time text");
    }

    if (!grantsFirst) {
      await settle(reads.length);
      await submit;
      assertIssued();
    }
    const activeOverlay = modal.children[0];
    for (const request of reads) {
      const { method, path } = await settle();
      assert.equal(`${method} ${path}`, request);
      assert.ok(!modal.hidden && modal.children[0] === activeOverlay, "a stale grants read cannot replace the active dialog");
      if (grantsFirst) {
        assert.ok(modal.querySelector("form") === form, "the pending issuance form survives");
      } else {
        assertIssued();
      }
    }
    if (grantsFirst) {
      await settle();
      await submit;
    }
    assertIssued();
    assert.deepEqual(requests, expectedRequests, "stale reads do not start more requests");
    assert.equal(vm.runInContext("state.sources[0]?.id", app), "cached-source", "closed reads do not apply stale sources");
  });
}

test("only the newest list response is applied after closing the result", async () => {
  const { app, modal, openForm, view, settle } = issuance();
  const submit = openForm().fire("submit");
  await settle();
  await submit;
  button(modal, "我已保存，关闭").fire("click");
  void app.refreshTokensView();
  await settle(1);
  assert.ok(view.textContent.includes(app.shortId(token.id, 12)), "the newest list response is rendered");
  await settle(0);
  const count = vm.runInContext("state.tokens.length", app);
  assert.equal(count, 1, "an older list response cannot overwrite the newest list");
  assert.ok(view.textContent.includes(app.shortId(token.id, 12)), "the newest rows stay rendered");
});

test("a clipboard rejection after close never puts the token back into the document", async () => {
  let rejectWrite;
  const { app, body, document, modal } = fixture({
    writeText: () => new Promise((_resolve, reject) => { rejectWrite = reject; }),
    fetch: async () => response(200, { tokens: [] }),
  });
  let fallbackCalls = 0;
  document.execCommand = () => { fallbackCalls += 1; return true; };
  app.showCreatedToken({ token, plaintext: sample });
  const field = modal.querySelector("textarea");
  button(modal, "复制令牌").fire("click");
  button(modal, "我已保存，关闭").fire("click");
  rejectWrite(new Error("mock delayed clipboard rejection"));
  await flush();
  assert.equal(fallbackCalls, 0, "closing the dialog cancels any not-yet-started fallback");
  assert.ok(modal.hidden && !body.textContent.includes(sample));
  assert.equal(field.selected, undefined, "a closed field is never selected after the fact");
});
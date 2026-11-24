// Injected at the top of every tailnet page by the service worker. It covers
// the things a service worker cannot do for a page:
//
// - WebSocket: replaced with a class that tunnels through the app's tailnet
//   node. Sockets opened inside Web Workers are not covered.
// - document.cookie: the site's cookies live in the node, not in the browser,
//   so reads and writes are redirected there.
//
// - The address: the app cannot read this frame's location, so the page
//   reports it on load and whenever a script or a "#" link changes it.
// - The title: reported whenever it changes, so the app can show it as the
//   tab's title.
//
// All of them talk to the app page (our top window).
// Needs codec.js loaded first.
(() => {
  const info = globalThis.TailnetCodec?.fromSiteLocation(location);
  const Native = globalThis.WebSocket;
  const initialCookies = globalThis.__tailnetCookies;
  delete globalThis.__tailnetCookies;
  if (!info?.upstream || !Native || window.top === window) return;
  const upstream = new URL(info.upstream);

  // --- Address --------------------------------------------------------------
  //
  // Only the page shown in the app's frame reports, not frames inside it.
  // "push" and "replace" say what happened to this frame's history; "move"
  // is a step through it or a change of the part after "#".
  if (window.parent === window.top) {
    const tell = (how) => top.postMessage(
      { type: "tailnet-location", path: location.pathname + location.search + location.hash, how },
      info.appOrigin);
    for (const [name, how] of [["pushState", "push"], ["replaceState", "replace"]]) {
      const native = History.prototype[name];
      History.prototype[name] = function (...args) {
        const result = native.apply(this, args);
        if (this === history) tell(how);
        return result;
      };
    }
    addEventListener("popstate", () => tell("move"));
    addEventListener("hashchange", () => tell("move"));
    tell("replace");

    // The title, whenever it changes. This script runs before the page's own
    // <title> is parsed, so the first report comes from the watcher.
    let toldTitle = "";
    const tellTitle = () => {
      const title = document.title;
      if (title === toldTitle) return;
      toldTitle = title;
      top.postMessage({ type: "tailnet-title", title }, info.appOrigin);
    };
    const watchTitle = () => {
      if (!document.head) return;
      new MutationObserver(tellTitle).observe(document.head, { childList: true, subtree: true, characterData: true });
      tellTitle();
    };
    if (document.head) watchTitle(); else addEventListener("DOMContentLoaded", watchTitle);
    // The app reloads through us when setting the frame's address would only
    // scroll to the same "#" part.
    addEventListener("message", (ev) => {
      if (ev.source === top && ev.origin === info.appOrigin && ev.data?.type === "tailnet-reload") location.reload();
    });
  }

  // --- document.cookie ------------------------------------------------------
  //
  // document.cookie is synchronous but the node is a message away, so we keep
  // a local copy: seeded by the service worker with the page, updated at once
  // on writes, and refreshed from the node whenever it says cookies changed.
  // HttpOnly cookies never leave the node.
  {
    const parse = (text) => {
      const map = new Map();
      for (const part of String(text).split("; ")) {
        if (!part) continue;
        const eq = part.indexOf("=");
        map.set(eq < 0 ? "" : part.slice(0, eq), eq < 0 ? part : part.slice(eq + 1));
      }
      return map;
    };
    let jar = parse(initialCookies ?? "");
    let lastId = 0;  // only the answer to our latest message is applied
    const channel = new MessageChannel();
    const port = channel.port1;
    const refresh = () => port.postMessage({ t: "get", path: location.pathname, id: ++lastId });
    port.onmessage = (ev) => {
      if (ev.data.t === "cookies" && ev.data.id === lastId) jar = parse(ev.data.value);
      else if (ev.data.t === "changed") refresh();
    };
    top.postMessage({ type: "tailnet-cookies" }, info.appOrigin, [channel.port2]);
    if (initialCookies === undefined) refresh();
    addEventListener("pagehide", () => port.postMessage({ t: "bye" }));

    const native = Object.getOwnPropertyDescriptor(Document.prototype, "cookie");
    Object.defineProperty(Document.prototype, "cookie", {
      configurable: true,
      enumerable: true,
      get() {
        if (this !== document) return native.get.call(this);
        return [...jar].map(([name, value]) => (name ? name + "=" + value : value)).join("; ");
      },
      set(text) {
        if (this !== document) { native.set.call(this, text); return; }
        text = String(text);
        const [pair, ...attributes] = text.split(";");
        const eq = pair.indexOf("=");
        const name = (eq < 0 ? "" : pair.slice(0, eq)).trim();
        const value = (eq < 0 ? pair : pair.slice(eq + 1)).trim();
        // Update the local copy now so an immediate read sees the write; the
        // node applies the full rules and its answer replaces this guess.
        let remove = false, visible = true;
        for (const attribute of attributes) {
          const cut = attribute.indexOf("=");
          const key = (cut < 0 ? attribute : attribute.slice(0, cut)).trim().toLowerCase();
          const val = cut < 0 ? "" : attribute.slice(cut + 1).trim();
          if (key === "httponly") return;  // scripts cannot set HttpOnly cookies
          if (key === "max-age" && Number(val) <= 0) remove = true;
          if (key === "expires" && Date.parse(val) <= Date.now()) remove = true;
          if (key === "path" && val && !location.pathname.startsWith(val)) visible = false;
        }
        if (remove) jar.delete(name); else if (visible) jar.set(name, value);
        port.postMessage({ t: "set", path: location.pathname, cookie: text, id: ++lastId });
      },
    });
  }

  // --- WebSocket ------------------------------------------------------------
  const live = new Set();

  class TailnetWebSocket extends EventTarget {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;
    CONNECTING = 0;
    OPEN = 1;
    CLOSING = 2;
    CLOSED = 3;

    readyState = 0;
    binaryType = "blob";
    bufferedAmount = 0;
    extensions = "";
    protocol = "";
    url = "";
    onopen = null;
    onmessage = null;
    onerror = null;
    onclose = null;

    #port = null;
    #sends = Promise.resolve();  // keeps order when a Blob has to be read first

    constructor(url, protocols = []) {
      super();
      let u;
      try { u = new URL(url, location.href); } catch { throw new DOMException(`The URL '${url}' is invalid.`, "SyntaxError"); }
      if (u.protocol === "http:") u.protocol = "ws:";
      if (u.protocol === "https:") u.protocol = "wss:";
      if (u.protocol !== "ws:" && u.protocol !== "wss:") {
        throw new DOMException(`The URL's scheme must be 'ws', 'wss', 'http' or 'https'.`, "SyntaxError");
      }
      // Only this site's own host goes through the node; anything else is
      // the browser's business.
      if (u.host !== location.host && u.host !== upstream.host) return new Native(url, protocols);

      this.url = u.href;
      const target = (upstream.protocol === "https:" ? "wss://" : "ws://") + upstream.host + u.pathname + u.search;
      const channel = new MessageChannel();
      this.#port = channel.port1;
      this.#port.onmessage = (ev) => this.#receive(ev.data);
      live.add(this);
      top.postMessage(
        { type: "tailnet-websocket", url: target, protocols: [].concat(protocols).map(String) },
        info.appOrigin, [channel.port2]);
    }

    #emit(event) {
      const handler = this["on" + event.type];
      if (typeof handler === "function") handler.call(this, event);
      this.dispatchEvent(event);
    }

    #receive(m) {
      if (m.t === "open" && this.readyState === 0) {
        this.readyState = 1;
        this.protocol = m.protocol ?? "";
        this.#emit(new Event("open"));
      } else if (m.t === "message" && this.readyState === 1) {
        let data = m.data;
        if (typeof data !== "string") data = this.binaryType === "arraybuffer" ? data.buffer : new Blob([data]);
        this.#emit(new MessageEvent("message", { data, origin: new URL(this.url).origin }));
      } else if (m.t === "error") {
        this.#emit(new Event("error"));
      } else if (m.t === "close" && this.readyState !== 3) {
        this.readyState = 3;
        live.delete(this);
        this.#port.close();
        this.#emit(new CloseEvent("close", { code: m.code, reason: m.reason, wasClean: m.wasClean }));
      }
    }

    #post(message) {
      this.#sends = this.#sends.then(message).then((m) => this.#port.postMessage(m)).catch(() => {});
    }

    send(data) {
      if (this.readyState === 0) throw new DOMException("Still in CONNECTING state.", "InvalidStateError");
      if (this.readyState !== 1) return;
      if (data instanceof Blob) this.#post(async () => ({ t: "send", data: new Uint8Array(await data.arrayBuffer()) }));
      else if (ArrayBuffer.isView(data)) {
        const copy = new Uint8Array(data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength));
        this.#post(() => ({ t: "send", data: copy }));
      } else if (data instanceof ArrayBuffer) {
        const copy = new Uint8Array(data.slice(0));
        this.#post(() => ({ t: "send", data: copy }));
      } else {
        const text = String(data);
        this.#post(() => ({ t: "send", data: text }));
      }
    }

    close(code, reason) {
      if (code !== undefined && code !== 1000 && !(code >= 3000 && code <= 4999)) {
        throw new DOMException("The close code must be either 1000, or between 3000 and 4999.", "InvalidAccessError");
      }
      this.#close(code, reason);
    }

    #close(code, reason) {
      if (this.readyState >= 2) return;
      this.readyState = 2;
      this.#post(() => ({ t: "close", code, reason: reason === undefined ? undefined : String(reason) }));
    }

    // Used when the page goes away; a MessagePort has no close event, so the
    // app would otherwise keep the connection open.
    static closeAll() {
      for (const socket of live) socket.#close(1001, "");
    }
  }

  addEventListener("pagehide", () => TailnetWebSocket.closeAll());
  globalThis.WebSocket = TailnetWebSocket;
})();

// App side of the bridge between tailnet sites and the node.
//
// Every tailnet site is shown on its own subdomain (see __tailnet/codec.js).
// For each one we keep a hidden relay iframe on that subdomain; its service
// worker sends us "tailnet-request" messages through it, and we answer them
// with the node. Needs __tailnet/codec.js loaded first.

const Codec = globalThis.TailnetCodec;

// "host:8080/x" or "http://host/x" -> URL
export function parseAddress(input) {
  let text = input.trim();
  if (!/^https?:\/\//i.test(text)) text = "http://" + text;
  return new URL(text);
}

// Earlier versions ran a service worker on the app's own origin; remove it.
export async function removeAppWorker() {
  if (!("serviceWorker" in navigator)) return;
  for (const reg of await navigator.serviceWorker.getRegistrations()) await reg.unregister();
}

// frame:       the visible iframe
// request:     ({url, method, headers, body, from, topLevel}) =>
//                Promise<{status, statusText, url, headers, read(), cancel()}>
//              read() resolves to the next body chunk (Uint8Array) or null at the end.
//              from is the tailnet origin of the page that caused the request (or
//              another website's origin, or null if unknown) and topLevel says it
//              loads a page as a tab would; the node uses both to decide which
//              SameSite cookies to send.
//              A socket has from too, when its page is framed by another site.
// websocket:   ({url, protocols, headers, from, onOpen, onMessage, onClose, onError}) => {send, close}
// cookies:     { get(url) => string, set(url, text) }: what document.cookie reads and writes
// onNavigated: (tailnetUrl, how) => void. how is "load" after a page navigation
//              got a final response (the address then has no "#" part), or what
//              the page then reported: "load" for a page reached by a link,
//              "replace" with its full address once loaded, and "push", "replace" or "move" when it changes its own
//              address without loading (history.pushState, a "#" link, back)
// onTitle:     (title) => void, when the page in the frame gets or changes its
//              title. A new page has none until it reports one.
// onCookiesChanged: () => void, after a response or a page script changed a cookie
// onError:     (message) => void
// onSuperseded: () => void, when Porthole is opened in another tab and this one stops
export function createBridge({ frame, request, websocket, cookies, onNavigated, onTitle, onCookiesChanged, onError, onSuperseded }) {
  const relays = new Map();  // site origin -> { el, upstreamOrigin, pages, ready, resolve, reject }
  let current = "";  // the address the frame's page last reported; "" while one loads
  let shown = null;  // the relay of the site in the frame
  let announced = null;  // a page we reported as loading, until it reports itself
  let asked = null;  // { relay, from }: someone other than the user sent the frame to this site
  // Pages, and their workers, that are in a frame on another site's page:
  // the service worker's id for each -> that site's origin, or null. Entered
  // when such a page is asked for (see serve) and when one says so (page.js).
  const framed = new Map();

  // Only one tab browses at a time. A site's service worker is shared by
  // every tab and cannot tell which tab a request came from, so two active
  // tabs would get each other's requests. The newest tab takes over: it
  // announces itself, and older tabs remove their relays and stop.
  // The number also goes on each relay's address so the worker can pick the
  // newest tab's relay while an older one is still closing.
  const instance = Date.now() * 1000 + Math.floor(Math.random() * 1000);
  let active = true;
  const tabs = "BroadcastChannel" in globalThis ? new BroadcastChannel("porthole-active-tab") : null;
  if (tabs) {
    tabs.onmessage = (ev) => {
      if (!active || ev.data?.type !== "claim" || !(ev.data.instance > instance)) return;
      active = false;
      for (const relay of relays.values()) {
        relay.reject(new Error("Porthole is open in another tab."));
        relay.el.remove();
      }
      relays.clear();
      frame.src = "about:blank";
      tabs.close();
      onSuperseded?.();
    };
    tabs.postMessage({ type: "claim", instance });
  }

  function relayFor(upstream) {
    const site = Codec.siteOrigin(upstream, location);
    let relay = relays.get(site);
    if (!relay) {
      const el = document.createElement("iframe");
      el.hidden = true;
      el.title = "Tailnet relay";
      relay = { el, upstreamOrigin: upstream.origin, pages: new Set() };
      relay.ready = new Promise((resolve, reject) => {
        relay.resolve = resolve;
        relay.reject = reject;
        setTimeout(() => reject(new Error(`This browser could not reach ${new URL(site).host}, the address Porthole uses to show that site.`)), 15000);
      });
      relay.ready.catch(() => { el.remove(); if (relays.get(site) === relay) relays.delete(site); });
      relays.set(site, relay);
      el.src = site + "/__tailnet/relay.html?i=" + instance;
      document.body.append(el);
    }
    return relay.ready.then(() => site);
  }

  // Show a tailnet URL in the frame. Leave from out when the user asked.
  // Otherwise it is the origin of the page that did, or null if all that is
  // known is that the user did not: a link followed from somewhere else.
  async function go(input, from) {
    if (!active) throw new Error("Porthole is open in another tab.");
    const upstream = typeof input === "string" ? parseAddress(input) : input;
    const site = await relayFor(upstream);
    if (!active) throw new Error("Porthole is open in another tab.");
    shown = relays.get(site);
    asked = from !== undefined ? { relay: shown, from } : null;
    // Setting the frame to the address it is already on, "#" part included,
    // would not load anything, so ask the page to reload itself.
    if (upstream.hash && upstream.href === current) frame.contentWindow.postMessage({ type: "tailnet-reload" }, site);
    else frame.src = site + upstream.pathname + upstream.search + upstream.hash;
  }

  function setFramed(client, by) {
    if (framed.size >= 1000) framed.delete(framed.keys().next().value);
    framed.set(client, by);
  }

  async function serve(relay, data, port) {
    try {
      // A site may only reach its own tailnet origin; this is what keeps
      // sites isolated from each other.
      if (new URL(data.req.url).origin !== relay.upstreamOrigin) throw new Error("Blocked a request outside this site's own origin.");
      const req = data.req;
      const safe = req.method === "GET" || req.method === "HEAD";
      let recheck = false;
      // Whether this is a page for our frame, whose address we show. Only a
      // page we put there ourselves certainly is.
      const ours = req.from === location.origin;
      // A page asked for while our frame shows another site is a frame on
      // that site's page: its address must never be shown as the page's.
      const theirs = data.nav && !req.topLevel && !ours && shown !== relay;
      // A page opened in a browser tab of its own is not in our frame either.
      const tab = data.nav && req.topLevel === true && !ours;
      if (req.from === location.origin) {
        // We loaded this page into the frame, which is the tab as far as the
        // site is concerned: for the user, unless a page of another site asked.
        req.from = asked?.relay === relay ? asked.from : relay.upstreamOrigin;
        req.topLevel = true;
      } else if (data.nav && !req.topLevel && shown === relay) {
        // A page going into a frame while our frame shows this site: ours,
        // moving on. If nothing says who asked, a page that hides its
        // referrer followed one of its own links. If another site did, it
        // sent our frame here, as a link in a tab would.
        if (req.from == null) { if (safe) req.from = relay.upstreamOrigin; }
        else if (req.from !== relay.upstreamOrigin) req.topLevel = true;
      } else if (data.nav && !req.topLevel) {
        // A page going into a frame while our frame shows another site: a
        // frame on that site's page, whatever the request says of itself
        // (a link followed inside such a frame names this site). It goes
        // as that site's doing, and so does all the page then asks for.
        // Our own frame arriving here by a route we cannot see, such as the
        // browser's back button, looks the same; the page tells us if so
        // (recheck, see page.js) and loads again.
        if (req.from == null || req.from === relay.upstreamOrigin) req.from = shown ? shown.upstreamOrigin : null;
        if (typeof data.next === "string" && data.next) setFramed(data.next, req.from);
        recheck = safe;
      } else if (!data.nav && data.blank && shown !== relay && typeof data.client === "string" && !framed.has(data.client)) {
        // Asked for by a blank frame (about:blank or srcdoc content) that a
        // page of this site made, while our frame shows another site: the
        // page is in a frame on that site's page, and its blank frames with
        // it. They run none of our script, so they cannot say so themselves.
        req.from = shown ? shown.upstreamOrigin : null;
        req.topLevel = false;
        setFramed(data.client, req.from);
      } else if (!data.nav && framed.has(data.client)) {
        // Asked for by a page that another site's page shows in a frame. A
        // worker such a page starts is in the same position.
        req.from = framed.get(data.client);
        req.topLevel = false;
        if (typeof data.next === "string" && data.next) setFramed(data.next, req.from);
      }
      const r = await request(req);
      const res = { status: r.status, statusText: r.statusText, url: r.url, headers: r.headers };
      if (recheck) res.recheck = true;
      else if (data.doc && !(!data.nav && framed.has(data.client))) res.cookies = cookies.get(data.req.url);
      if (r.headers.some(([k]) => k.toLowerCase() === "set-cookie")) cookiesChanged(relay);
      if (data.nav && !(r.status >= 300 && r.status < 400)) {
        if (asked?.relay === relay) asked = null;  // kept until here so a redirect on the way counts the same
        // Say where our frame now is. A page that runs our script says so
        // itself, and it alone knows whether it is in our frame or in a
        // frame on the page there, so leave that to it unless we put the
        // page there. Other kinds of page (an image, a download) cannot say.
        const html = r.headers.some(([k, v]) => k.toLowerCase() === "content-type" && /^text\/html\b/i.test(v));
        if (!theirs && !tab && (ours || !html)) {
          current = "";
          announced = html ? data.req.url : null;
          onNavigated?.(data.req.url, "load");
        }
      }
      port.postMessage({ ok: true, res });
      streamBody(r, port);
    } catch (e) {
      port.postMessage({ ok: false, error: String(e?.message ?? e) });
    }
  }

  // After the headers, the worker pulls the body one chunk at a time:
  // it sends {pull} and gets {chunk}, {done} or {error}; {cancel} stops early.
  // Pulling (rather than pushing) means a slow page slows the node's reads.
  function streamBody(r, port) {
    let finished = false, reading = false;
    port.onmessage = async (ev) => {
      if (ev.data?.cancel) {
        if (!finished) { finished = true; r.cancel(); }
        port.close();
        return;
      }
      if (!ev.data?.pull || finished || reading) return;
      reading = true;
      try {
        const chunk = await r.read();
        if (finished) return;  // cancelled while reading
        if (chunk === null) { finished = true; port.postMessage({ done: true }); port.close(); }
        else port.postMessage({ chunk }, [chunk.buffer]);
      } catch (e) {
        if (!finished) { finished = true; port.postMessage({ error: String(e?.message ?? e) }); port.close(); }
      } finally {
        reading = false;
      }
    };
  }

  // Tell a site's open pages to re-read their cookies.
  function cookiesChanged(relay, except) {
    onCookiesChanged?.();
    for (const page of relay.pages) if (page !== except) page.postMessage({ t: "changed" });
  }

  // A page's document.cookie replacement (__tailnet/page.js) hands us a port.
  // It only ever names a path; the origin is fixed by which site it is on.
  function openCookies(relay, port) {
    relay.pages.add(port);
    port.onmessage = (ev) => {
      const m = ev.data;
      if (m?.t === "bye") { relay.pages.delete(port); port.close(); return; }
      if (m?.t !== "get" && m?.t !== "set") return;
      let url;
      try { url = new URL(String(m.path), relay.upstreamOrigin); } catch { return; }
      if (url.origin !== relay.upstreamOrigin) return;
      let value = "";
      try {
        if (m.t === "set") { cookies.set(url.href, String(m.cookie)); cookiesChanged(relay, port); }
        value = cookies.get(url.href);
      } catch (e) {
        onError?.(`cookies ${url.href}: ${e?.message ?? e}`);
      }
      port.postMessage({ t: "cookies", value, id: m.id });
    };
  }

  // A page's WebSocket replacement (__tailnet/page.js) asks for a socket and
  // hands us one end of a MessageChannel for it.
  function openSocket(relay, data, port) {
    const fail = (message) => {
      onError?.(`websocket ${data.url}: ${message}`);
      port.postMessage({ t: "error" });
      port.postMessage({ t: "close", code: 1006, reason: "", wasClean: false });
      port.close();
    };
    let target;
    try { target = new URL(String(data.url)); } catch { return fail("invalid URL"); }
    if (target.protocol !== "ws:" && target.protocol !== "wss:") return fail("invalid URL");
    const asHttp = (target.protocol === "wss:" ? "https://" : "http://") + target.host;
    if (asHttp !== relay.upstreamOrigin) return fail("blocked a socket outside this site's own origin");
    let socket;
    try {
      socket = websocket({
        url: target.href,
        protocols: Array.isArray(data.protocols) ? data.protocols.map(String) : [],
        headers: [["Origin", relay.upstreamOrigin], ["User-Agent", navigator.userAgent]],
        // Only a page framed by another site sends this.
        ...("from" in data ? { from: typeof data.from === "string" ? data.from : null } : {}),
        onOpen: (protocol) => port.postMessage({ t: "open", protocol }),
        onMessage: (d) => port.postMessage({ t: "message", data: d }, typeof d === "string" ? [] : [d.buffer]),
        onError: (message) => { onError?.(`websocket ${target.href}: ${message}`); port.postMessage({ t: "error" }); },
        onClose: (code, reason, wasClean) => { port.postMessage({ t: "close", code, reason, wasClean }); port.close(); },
      });
    } catch (e) {
      return fail(String(e?.message ?? e));
    }
    port.onmessage = (ev) => {
      if (ev.data?.t === "send") socket.send(ev.data.data);
      else if (ev.data?.t === "close") socket.close(ev.data.code, ev.data.reason);
    };
  }

  window.addEventListener("message", (ev) => {
    const relay = relays.get(ev.origin);
    const data = ev.data;
    if (!relay || !data || typeof data.type !== "string") return;
    // Any window on a site's origin (the page, or a frame inside it) may open
    // sockets and use cookies; both are confined to that site's own tailnet host.
    if (data.type === "tailnet-websocket" && ev.ports[0]) { openSocket(relay, data, ev.ports[0]); return; }
    if (data.type === "tailnet-cookies" && ev.ports[0]) { openCookies(relay, ev.ports[0]); return; }
    if (ev.source === relay.el.contentWindow) {
      if (data.type === "tailnet-relay-ready") relay.resolve();
      else if (data.type === "tailnet-relay-failed") relay.reject(new Error(String(data.error)));
      else if (data.type === "tailnet-request" && ev.ports[0]) serve(relay, data, ev.ports[0]);
      else if (data.type === "tailnet-framed" && typeof data.client === "string") {
        if (data.clear) framed.delete(data.client);
        else setFramed(data.client, typeof data.by === "string" ? data.by : null);
      }
    } else if (ev.source === frame.contentWindow) {
      // Sent by pages our service worker generates.
      if (data.type === "tailnet-navigate") go(String(data.url), relay.upstreamOrigin).catch((e) => onError?.(String(e?.message ?? e)));
      else if (data.type === "tailnet-error") onError?.(`${data.url}: ${data.message}`);
      else if (data.type === "tailnet-location") {
        // Sent by __tailnet/page.js. It only names a path; the origin is
        // fixed by which site the page is on.
        let url;
        try { url = new URL(String(data.path), relay.upstreamOrigin); } catch { return; }
        if (url.origin !== relay.upstreamOrigin) return;
        current = url.href;
        shown = relay;
        if (data.how === "load") {
          // A new page in our frame. If we reported it when it was asked
          // for, this only adds the "#" part.
          const bare = url.href.split("#")[0];
          const known = announced === bare;
          announced = null;
          if (!known) onNavigated?.(bare, "load");
          if (known || url.href !== bare) onNavigated?.(url.href, "replace");
          return;
        }
        onNavigated?.(url.href, ["push", "replace", "move"].includes(data.how) ? data.how : "move");
      } else if (data.type === "tailnet-title") {
        onTitle?.(String(data.title ?? "").trim().slice(0, 300));
      }
    }
  });

  return { go };
}

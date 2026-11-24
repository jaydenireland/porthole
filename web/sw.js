// Service worker for one tailnet site (also injects the WebSocket tunnel script).
//
// Each tailnet origin is shown on its own subdomain of the app (see
// __tailnet/codec.js), so the site sits at "/" of its own origin and is
// isolated from other sites. This worker serves every request on that
// subdomain by asking the tailnet node for the same path on the real host.
//
// The node runs in the app page, on a different origin. Requests reach it
// through the hidden relay page (__tailnet/relay.html) the app keeps open on
// this origin.

importScripts("/__tailnet/codec.js");

const { upstream: UPSTREAM, appOrigin: APP_ORIGIN } = TailnetCodec.fromSiteLocation(self.location);

const REDIRECTS = new Set([301, 302, 303, 307, 308]);
const NULL_BODY = new Set([101, 204, 205, 304]);
// "origin" names this subdomain or whoever sent the browser here; the request
// goes out with the tailnet's name for them instead (see askedBy).
const DROP_REQUEST_HEADERS = new Set(["host", "accept-encoding", "connection", "content-length", "origin"]);
const DROP_RESPONSE_HEADERS = new Set([
  // The node already decoded the body (and removed Content-Length if it did).
  "content-encoding", "transfer-encoding", "connection", "keep-alive",
  // These would stop the page rendering in the app's iframe on this origin.
  // What they say about frames is applied by page.js instead (see framingOf).
  "content-security-policy", "content-security-policy-report-only", "x-frame-options",
  "cross-origin-opener-policy", "cross-origin-embedder-policy", "cross-origin-resource-policy",
  "strict-transport-security",
]);

// Added to every HTML document so page.js runs before the page's own scripts.
const PAGE_SCRIPTS = '<script src="/__tailnet/codec.js"></script><script src="/__tailnet/page.js"></script>';
const DOCUMENT_DESTINATIONS = new Set(["document", "iframe", "frame"]);

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

function isHttp(u) { return u.protocol === "http:" || u.protocol === "https:"; }

// Hosts that only the node can reach: MagicDNS names, Tailscale and private
// IPs, and single-label names.
function isTailnetHost(hostname) {
  if (hostname === "localhost") return false;
  if (hostname.endsWith(".ts.net")) return true;
  const ip = /^(\d+)\.(\d+)\.\d+\.\d+$/.exec(hostname);
  if (ip) {
    const a = +ip[1], b = +ip[2];
    return (a === 100 && b >= 64 && b <= 127) || a === 10 || (a === 192 && b === 168) || (a === 172 && b >= 16 && b <= 31);
  }
  return !hostname.includes(".");
}

// A URL on this subdomain -> the real tailnet URL, or null.
function toUpstream(localHref) {
  let u;
  try { u = new URL(localHref); } catch { return null; }
  return u.origin === self.location.origin ? UPSTREAM + u.pathname + u.search : null;
}

// The origin of the page that caused a request, as a browser on the tailnet
// would name it, or null when nothing says. The site uses it to refuse
// requests another site forged, and the node to decide which cookies go.
//
// - This site's own pages: its tailnet origin.
// - The app, which loads pages into its frame: the app's origin. The app
//   knows whether the user or another site asked, and fills that in.
// - Another tailnet site, seen here on its own subdomain: its tailnet origin.
// - Any other website: its origin, unchanged.
//
// Only page loads can come from elsewhere; everything else is asked for by
// a page this worker controls. A page load says who caused it in "Origin"
// when it posts a form and in the referrer otherwise.
function askedBy(req, nav) {
  if (!nav) return UPSTREAM;
  let origin = req.headers.get("origin");
  if (!origin && req.referrer) {
    try { origin = new URL(req.referrer).origin; } catch { /* not an address */ }
  }
  if (!origin || origin === "null") return null;
  if (origin === self.location.origin) return UPSTREAM;
  if (origin === APP_ORIGIN) return APP_ORIGIN;
  try {
    const u = new URL(origin);
    const [label, ...rest] = u.host.split(".");
    if (`${u.protocol}//${rest.join(".")}` === APP_ORIGIN) return TailnetCodec.decodeLabel(label) ?? origin;
  } catch { /* not an address */ }
  return origin;
}

// A page says it is inside a frame on another site's page (see page.js). The
// app remembers that, so the page's requests go out as that site's doing.
self.addEventListener("message", (event) => {
  if (event.data?.type !== "tailnet-framed" || !event.source?.id) return;
  event.waitUntil(findRelay().then((relay) => relay?.postMessage({ type: "tailnet-framed", client: event.source.id, by: event.data.by })));
});

self.addEventListener("fetch", (event) => {
  if (!UPSTREAM) return;  // not a site subdomain (e.g. the app's own origin)
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin) return;       // third-party resources load directly
  if (url.pathname.startsWith("/__tailnet/")) return;    // our own helper files
  event.respondWith(proxy(event.request, event.clientId));
});

// client: the id of the page or worker asking, for requests that are not page loads.
async function proxy(req, client) {
  const nav = req.mode === "navigate";
  let method = req.method;
  let body = method === "GET" || method === "HEAD" ? null : new Uint8Array(await req.arrayBuffer());
  let headers = [];
  for (const [k, v] of req.headers) {
    if (!DROP_REQUEST_HEADERS.has(k.toLowerCase())) headers.push([k, v]);
  }
  headers.push(["User-Agent", self.navigator.userAgent]);
  const referer = toUpstream(req.referrer);
  if (referer) headers.push(["Referer", referer]);

  const from = askedBy(req, nav);
  // A page loaded into a tab of its own, not into a frame.
  const topLevel = req.destination === "document";

  const isDocument = nav || DOCUMENT_DESTINATIONS.has(req.destination);
  let cur = new URL(toUpstream(req.url));
  for (let hop = 0; hop < 10; hop++) {
    const h = headers.slice();
    if (method !== "GET" && method !== "HEAD") h.push(["Origin", from ?? "null"]);
    let res;
    try {
      res = await nodeRequest({ url: cur.href, method, headers: h, body, from, topLevel }, nav, isDocument, client);
    } catch (e) {
      return errorResponse(cur.href, e, nav);
    }

    const location = res.headers.find(([k]) => k.toLowerCase() === "location")?.[1];
    let next = null;
    if (REDIRECTS.has(res.status) && location) {
      try { next = new URL(location, cur); } catch { /* not a usable redirect */ }
    }
    if (!next) {
      try {
        return await toResponse(res, method, isDocument);
      } catch (e) {
        return errorResponse(cur.href, e, nav);
      }
    }
    res.body.cancel();  // a redirect's body is never shown
    if (next.origin === cur.origin) {
      if (nav) {
        // Let the browser follow it so the frame's address is right.
        return new Response(null, { status: res.status, headers: { Location: next.pathname + next.search + next.hash } });
      }
      if (res.status === 303 || ((res.status === 301 || res.status === 302) && method === "POST")) {
        method = "GET";
        body = null;
        headers = headers.filter(([k]) => k.toLowerCase() !== "content-type");
      }
      cur = next;
      continue;
    }
    if (isHttp(next) && isTailnetHost(next.hostname)) {
      // Another tailnet site (or this one on another scheme or port) is shown on
      // a different subdomain, which only the app can set up.
      if (nav) return bounceResponse(next.href);
      return errorResponse(next.href, new Error("Redirects to another tailnet site are only supported for page navigations."), false);
    }
    // Public internet: the browser can go there itself.
    return new Response(null, { status: res.status, headers: { Location: next.href } });
  }
  return errorResponse(cur.href, new Error("Too many redirects"), nav);
}

// res.body is a stream. It is passed straight through, except for HTML
// documents, which are read whole so the page scripts can be spliced in.
async function toResponse(res, method, isDocument) {
  const headers = new Headers();
  for (const [k, v] of res.headers) {
    if (DROP_RESPONSE_HEADERS.has(k.toLowerCase())) continue;
    try { headers.append(k, v); } catch { /* not a valid header for a Response */ }
  }
  const status = res.status >= 200 && res.status <= 599 ? res.status : 502;
  if (NULL_BODY.has(status) || method === "HEAD") {
    res.body.cancel();
    return new Response(null, { status, statusText: res.statusText, headers });
  }
  let body = res.body;
  if (isDocument && /^text\/html\b/i.test(headers.get("content-type") ?? "")) {
    const whole = new Uint8Array(await new Response(res.body).arrayBuffer());
    body = injectPageScripts(whole, res.cookies, framingOf(res.headers));
    headers.delete("content-length");
  }
  return new Response(body, { status, statusText: res.statusText, headers });
}

// What a response says about being shown in frames: the source lists of
// every Content-Security-Policy "frame-ancestors" and the X-Frame-Options
// values, or null if it says nothing. The browser cannot apply either: both
// headers are dropped so the app's frame can show the page, and the pages
// are not at their real addresses. page.js applies them to any frame between
// the page and the app.
function framingOf(headers) {
  const ancestors = [], options = [];
  for (const [k, v] of headers) {
    const name = k.toLowerCase();
    if (name === "content-security-policy") {
      for (const policy of v.split(",")) {
        for (const directive of policy.split(";")) {
          const [word, ...sources] = directive.trim().split(/\s+/);
          if (word.toLowerCase() === "frame-ancestors") { ancestors.push(sources); break; }
        }
      }
    } else if (name === "x-frame-options") {
      options.push(...v.split(",").map((x) => x.trim().toUpperCase()).filter(Boolean));
    }
  }
  return ancestors.length || options.length ? { ancestors, options } : null;
}

// Splices PAGE_SCRIPTS into the HTML bytes right after <head>, so they run
// first without pushing the doctype out of place, along with the page's
// starting document.cookie value and what it says about frames. The search treats bytes as Latin-1
// characters, which keeps offsets equal to byte positions.
function injectPageScripts(body, cookies, framing) {
  const values = [];
  if (typeof cookies === "string") values.push(`self.__tailnetCookies=${JSON.stringify(cookies)};`);
  if (framing) values.push(`self.__tailnetFraming=${JSON.stringify(framing)};`);
  const seed = values.length ? `<script>${values.join("").replace(/</g, "\\u003c")}<\/script>` : "";
  const inject = new TextEncoder().encode(seed + PAGE_SCRIPTS);
  const start = new TextDecoder("latin1").decode(body.subarray(0, 65536));
  const m = /<head(\s[^>]*)?>/i.exec(start) ?? /<html(\s[^>]*)?>/i.exec(start) ?? /<!doctype[^>]*>/i.exec(start);
  const at = m ? m.index + m[0].length : 0;
  const out = new Uint8Array(body.length + inject.length);
  out.set(body.subarray(0, at));
  out.set(inject, at);
  out.set(body.subarray(at), at + inject.length);
  return out;
}

const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const PAGE_STYLE = `<style>body{font:15px/1.5 system-ui,sans-serif;margin:48px auto;max-width:560px;padding:0 16px;color-scheme:light dark}
code{word-break:break-all}h1{font-size:20px}</style>`;

// Tells the app (our top window) something; JSON is safe inside <script> once "<" is escaped.
function tellApp(message) {
  const json = JSON.stringify(message).replace(/</g, "\\u003c");
  return `<script>if (top !== window) top.postMessage(${json}, ${JSON.stringify(APP_ORIGIN)});<\/script>`;
}

function htmlResponse(status, html) {
  return new Response(html, { status, headers: { "Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store" } });
}

function errorResponse(url, err, nav) {
  const message = String(err?.message ?? err);
  if (!nav) return new Response(message, { status: 502, headers: { "Content-Type": "text/plain; charset=utf-8" } });
  return htmlResponse(502, `<!doctype html><meta charset="utf-8"><title>Can't reach this page</title>${PAGE_STYLE}
<h1>Can't reach this page</h1><p><code>${esc(url)}</code></p><p>${esc(message)}</p>
${tellApp({ type: "tailnet-error", url, message })}`);
}

function bounceResponse(url) {
  return htmlResponse(200, `<!doctype html><meta charset="utf-8"><title>Redirecting…</title>${PAGE_STYLE}
<p>Redirecting to <code>${esc(url)}</code>…</p>
${tellApp({ type: "tailnet-navigate", url })}`);
}

// Ask the app page, through this origin's relay page, to run the request on
// the tailnet node. The reply comes back on a port the relay passes along:
// first the status and headers, then the body, which we pull chunk by chunk
// (see streamBody in bridge.js) and expose as a ReadableStream in res.body.
// doc: the response is an HTML document, so the app adds its cookies.
async function nodeRequest(req, nav, doc, client) {
  const relay = await findRelay();
  if (!relay) throw new Error("Open this site from Porthole.");
  return new Promise((resolve, reject) => {
    const channel = new MessageChannel();
    const timer = setTimeout(() => reject(new Error("Timed out waiting for the tailnet node.")), 60000);
    const port = channel.port1;
    port.onmessage = (ev) => {
      clearTimeout(timer);
      if (!ev.data.ok) { port.close(); reject(new Error(ev.data.error)); return; }
      const res = ev.data.res;
      res.body = new ReadableStream({
        pull(controller) {
          return new Promise((next, fail) => {
            port.onmessage = (e) => {
              if (e.data.chunk) { controller.enqueue(e.data.chunk); next(); }
              else if (e.data.done) { port.close(); controller.close(); next(); }
              else { port.close(); fail(new Error(e.data.error ?? "The response was cut off.")); }
            };
            port.postMessage({ pull: true });
          });
        },
        cancel() {
          port.postMessage({ cancel: true });
          port.close();
        },
      });
      resolve(res);
    };
    relay.postMessage({ type: "tailnet-request", req, nav, doc, client }, [channel.port2]);
  });
}

// The app's relay page on this origin, or null if Porthole is not open.
async function findRelay() {
  const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  // Every Porthole tab that has this site open has a relay here. Only the
  // newest tab is active (see bridge.js), and its relay carries the highest
  // number in "?i=". Some hosts redirect relay.html to its extensionless
  // address, so both spellings are accepted.
  let relay = null, newest = -1;
  for (const client of windows) {
    const url = new URL(client.url);
    if (!/^\/__tailnet\/relay(\.html)?$/.test(url.pathname)) continue;
    const i = Number(url.searchParams.get("i")) || 0;
    if (i > newest) { newest = i; relay = client; }
  }
  return relay;
}

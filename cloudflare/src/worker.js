// Porthole's Worker. Cloudflare serves the static files in dist/ by itself;
// this code only runs for requests that are not a file.
//
// /node/main.wasm  The Tailscale client is over Cloudflare's 25 MiB file
//                  limit, so build.mjs stores it Brotli-compressed as
//                  /node/main.wasm.br. Here it is sent as it is with
//                  "Content-Encoding: br", and the browser unpacks it.
// /test/report     The test pages call this with their result when opened
//                  with ?report. It is written to the Worker's log.

const CLIENT_PATH = "/node/main.wasm";

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === CLIENT_PATH) return client(request, env);
    if (url.pathname === "/test/report") {
      console.log("test report", JSON.stringify(Object.fromEntries(url.searchParams)));
      return new Response(null, { status: 204 });
    }
    return env.ASSETS.fetch(request);  // the 404 page
  },
};

async function client(request, env) {
  if (request.method !== "GET" && request.method !== "HEAD") {
    return new Response("Method not allowed", { status: 405, headers: { Allow: "GET, HEAD" } });
  }
  // Cloudflare rewrites Accept-Encoding before a Worker sees it; the
  // browser's own value is in request.cf.
  const accepts = request.cf?.clientAcceptEncoding ?? request.headers.get("Accept-Encoding") ?? "";
  if (!/\bbr\b/.test(accepts)) {
    return new Response("This browser cannot run Porthole: it does not accept Brotli-compressed files.", {
      status: 406, headers: { "Content-Type": "text/plain; charset=utf-8", Vary: "Accept-Encoding" },
    });
  }

  const headers = new Headers();
  const ifNoneMatch = request.headers.get("If-None-Match");
  if (ifNoneMatch) headers.set("If-None-Match", ifNoneMatch);
  const stored = await env.ASSETS.fetch(new Request(new URL(CLIENT_PATH + ".br", request.url), { method: request.method, headers }));
  if (stored.status !== 200 && stored.status !== 304) {
    return new Response("The Tailscale client is missing from this deployment.", { status: 502 });
  }

  const out = new Headers({
    "Content-Type": "application/wasm",
    "Content-Encoding": "br",
    // Always check for a new version, so the client never goes out of step
    // with wasm_exec.js; an unchanged file costs one small 304 response.
    "Cache-Control": "public, max-age=0, must-revalidate",
    Vary: "Accept-Encoding",
  });
  const etag = stored.headers.get("ETag");
  if (etag) out.set("ETag", etag);
  if (stored.status === 304) return new Response(null, { status: 304, headers: out });
  // encodeBody "manual": the body is already compressed, do not compress again.
  return new Response(stored.body, { status: 200, headers: out, encodeBody: "manual" });
}

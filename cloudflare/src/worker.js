// Porthole's Worker. Cloudflare serves the static files in dist/ by itself;
// this code only runs for requests that are not a file.
//
// /test/report     The test pages call this with their result when opened
//                  with ?report. It is written to the Worker's log.

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/test/report") {
      console.log("test report", JSON.stringify(Object.fromEntries(url.searchParams)));
      return new Response(null, { status: 204 });
    }
    return env.ASSETS.fetch(request);  // the 404 page
  },
};

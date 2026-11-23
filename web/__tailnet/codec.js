// Maps a tailnet origin to a single DNS label and back, so each tailnet site
// gets its own origin under the app's host:
//
//   http://nas.tail1a2b3c.ts.net       <-> nas--tail1a2b3c--ts--net--h
//   https://nas:8443                          <-> nas--s8443
//
// Dots become "--"; the last part is the scheme (h or s) plus any port.
// Loaded as a classic script by the page, the relay and the service worker.
(function (g) {
  function encodeLabel(u) {
    const host = u.hostname.toLowerCase();
    if (!/^[a-z0-9]([a-z0-9.-]*[a-z0-9])?$/.test(host) || host.includes("--") || host.includes("..")) {
      throw new Error(`"${u.hostname}" is not a host name Porthole can open.`);
    }
    const label = host.split(".").join("--") + "--" + (u.protocol === "https:" ? "s" : "h") + u.port;
    if (label.length > 63) throw new Error(`"${u.hostname}" is too long to map to a subdomain.`);
    return label;
  }

  // Returns the upstream origin ("http://host:port"), or null if not a site label.
  function decodeLabel(label) {
    const parts = label.split("--");
    const m = /^([hs])(\d*)$/.exec(parts.pop() ?? "");
    if (!m || !parts.length || parts.some((p) => !p)) return null;
    return (m[1] === "s" ? "https" : "http") + "://" + parts.join(".") + (m[2] ? ":" + m[2] : "");
  }

  // Origin a tailnet URL is shown on, given the app's location.
  function siteOrigin(upstream, appLocation) {
    return `${appLocation.protocol}//${encodeLabel(upstream)}.${appLocation.host}`;
  }

  // From inside a site origin: the upstream origin and the app's origin.
  function fromSiteLocation(loc) {
    const [label, ...rest] = loc.host.split(".");
    return { upstream: decodeLabel(label), appOrigin: `${loc.protocol}//${rest.join(".")}` };
  }

  g.TailnetCodec = { encodeLabel, decodeLabel, siteOrigin, fromSiteLocation };
})(globalThis);

// Builds dist/ from ../web for Cloudflare.
//
// Copies the site and leaves out files that are only for development.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const web = path.join(here, "..", "web");
const dist = path.join(here, "dist");
const LIMIT = 25 * 1024 * 1024;

// Paths under web/ that are not part of the published site.
const SKIP = new Set(["README.md"]);

function copy(dir) {
  for (const entry of fs.readdirSync(path.join(web, dir), { withFileTypes: true })) {
    const rel = path.posix.join(dir, entry.name);
    if (SKIP.has(rel) || entry.name.startsWith(".")) continue;
    if (entry.isDirectory()) { fs.mkdirSync(path.join(dist, rel), { recursive: true }); copy(rel); }
    else fs.copyFileSync(path.join(web, rel), path.join(dist, rel));
  }
}

fs.rmSync(dist, { recursive: true, force: true });
fs.mkdirSync(dist, { recursive: true });
copy("");

// A plain page for addresses that do not exist.
fs.writeFileSync(path.join(dist, "404.html"), `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Page not found | Porthole</title><link rel="icon" href="/assets/icon.svg" type="image/svg+xml">
<link rel="stylesheet" href="/assets/theme.css"></head>
<body style="display:grid;place-items:center;min-height:100vh;padding:24px;text-align:center">
<main><h1 style="font:600 28px/1.2 var(--serif);margin:0 0 10px">Page not found</h1>
<p style="color:var(--muted);margin-bottom:22px">There is nothing at this address.</p>
<a class="btn primary" href="/">Go to Porthole</a></main></body></html>
`);

// The client is built separately (../client/build.sh) and is not in git.
for (const file of ["client/porthole_client.js", "client/porthole_client_bg.wasm", "client/THIRD-PARTY-LICENSES.txt"]) {
  if (!fs.existsSync(path.join(dist, file))) {
    console.error(`web/${file} is missing. Run client/build.sh first.`);
    process.exit(1);
  }
}

// Report, and stop if anything is still too large for Cloudflare.
let files = 0, total = 0, tooLarge = [];
(function walk(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) { walk(full); continue; }
    const size = fs.statSync(full).size;
    files++; total += size;
    if (size > LIMIT) tooLarge.push(`${path.relative(dist, full)} (${(size / 1048576).toFixed(1)} MB)`);
  }
})(dist);
const client = fs.statSync(path.join(dist, "client", "porthole_client_bg.wasm")).size;
console.log(`dist/: ${files} files, ${(total / 1048576).toFixed(1)} MB. Tailscale client: ${(client / 1048576).toFixed(1)} MB before Cloudflare compresses it.`);
if (tooLarge.length) {
  console.error("Over Cloudflare's 25 MiB limit per file:\n  " + tooLarge.join("\n  "));
  process.exit(1);
}

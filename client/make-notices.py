#!/usr/bin/env python3
"""Writes the licence notices for everything compiled into the client.

    ./make-notices.py <output file>

The client is published as a compiled file, and nearly every library in it
(BSD, MIT, Apache, ISC and others) allows that only if its copyright notice
and licence text go along. build.sh runs this after each build, so the list
always matches Cargo.lock.

The libraries are the ones cargo links into the browser build, with the test
pages' extra features on so one file covers both builds
(`cargo tree --target wasm32-unknown-unknown -e normal --all-features`). Each one's licence
files are read from its source folder; identical texts are printed once.
"""
import hashlib
import json
import pathlib
import re
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
TARGET = "wasm32-unknown-unknown"
# The files a crate keeps its licence and notices in.
NOTICE = re.compile(r"^(LICEN[SC]E|COPYING|COPYRIGHT|NOTICE|PATENTS|UNLICENSE)", re.I)


def cargo(*args):
    return subprocess.run(["cargo", *args], cwd=HERE, check=True, capture_output=True, text=True).stdout


def notice_files(folder):
    """Licence files in a crate's folder, or failing that in the nearest
    folder above it that has any (a workspace keeps one copy at its root)."""
    for where in [folder, *folder.parents][:5]:
        found = sorted(p for p in where.iterdir() if p.is_file() and NOTICE.match(p.name))
        if found:
            return found
        if (where / ".git").exists() or where.name.startswith("index.crates.io-"):
            break
    return []


def main(out):
    linked = set()
    for line in cargo("tree", "--locked", "--target", TARGET, "-e", "normal", "--all-features",
                      "--prefix", "none", "--format", "{p}").splitlines():
        name, version = line.split()[:2]
        linked.add((name, version.lstrip("v")))

    meta = json.loads(cargo("metadata", "--locked", "--all-features", "--format-version", "1",
                            "--filter-platform", TARGET))
    crates = sorted((p for p in meta["packages"] if (p["name"], p["version"]) in linked
                     and p["name"] != "porthole-client"), key=lambda p: (p["name"].lower(), p["version"]))
    missing = linked - {(p["name"], p["version"]) for p in crates} - {("porthole-client", "0.1.0")}
    if missing:
        sys.exit(f"make-notices: no metadata for {sorted(missing)}")

    texts = {}   # hash -> (text, [crate labels])
    index = []
    no_file = []
    for p in crates:
        label = f"{p['name']} {p['version']}"
        licence = p.get("license") or "see licence file"
        index.append(f"{label}  ({licence})  {p.get('repository') or ''}".rstrip())
        files = notice_files(pathlib.Path(p["manifest_path"]).parent)
        if not files:
            no_file.append(f"{label}: {licence}. Source: {p.get('repository') or 'https://crates.io/crates/' + p['name']}")
        for f in files:
            text = f.read_text(errors="replace").strip()
            key = hashlib.sha256(" ".join(text.split()).encode()).hexdigest()
            texts.setdefault(key, (text, []))[1].append(label)

    rule = "=" * 78
    parts = [
        "Third-party software in Porthole",
        "",
        "Porthole's client (client/porthole_client_bg.wasm and porthole_client.js) is",
        "compiled from the open-source libraries below. Each is used under the licence",
        "shown, and the licence texts and copyright notices follow the list.",
        "",
        "Porthole's own code is under the BSD 3-Clause licence, copyright (c) 2026",
        "Jayden Ireland. Source, including the exact version of every library",
        "(client/Cargo.lock) and our changes to tailscale-rs",
        "(client/tailscale-rs.patch): https://github.com/jaydenireland/porthole",
        "",
        "tailscale-rs (the ts_* libraries) is copyright (c) Tailscale Inc &",
        "contributors and is used in a modified form. dyn-eq is under the Mozilla",
        "Public License 2.0 and is used unmodified; its source is at the address in",
        "the list. The certificate authority list in webpki-roots is Mozilla's, under",
        "CDLA-Permissive-2.0.",
        "",
        rule, f"Libraries ({len(crates)})", rule, "",
        *index,
    ]
    if no_file:
        parts += ["", rule, "Libraries that ship no licence file; the licence named applies", rule, "", *no_file]
    for text, users in sorted(texts.values(), key=lambda t: (-len(t[1]), t[1][0].lower())):
        parts += ["", rule, "Applies to: " + ", ".join(users), rule, "", text]
    pathlib.Path(out).write_text("\n".join(parts) + "\n")
    print(f"Wrote {out}: {len(crates)} libraries, {len(texts)} licence texts, {len(no_file)} without a file")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])

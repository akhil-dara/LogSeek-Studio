"""Writes licenses/THIRD-PARTY-LICENSES.txt: every library built into the
Windows program, with its version, licence and authors, followed by the
licence texts those libraries ship.

Run it from the repository root after changing dependencies:

    python tools/third_party_licenses.py

It reads `cargo metadata` and the library sources cargo has already
downloaded; it does not use the network.
"""

import hashlib
import json
import os
import re
import subprocess
import sys

TARGET = "x86_64-pc-windows-msvc"
OUT = os.path.join("licenses", "THIRD-PARTY-LICENSES.txt")
LICENCE_FILE = re.compile(r"^(licen[sc]e|copying|notice|unlicense|ofl|ufl)|licen[sc]e", re.I)
# Folders of a package that never hold its licence.
SKIP = ("src", "tests", "benches", "examples", "target")
ADDRESS = re.compile(r"\s*<[^>]*>|\s*\S+@\S+")


def linked_packages(meta):
    """The packages the program is built from: normal dependencies only,
    not the ones used only to build or to test."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    ours = set(meta["workspace_members"])
    seen, stack = set(), list(ours)
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    return sorted(
        (packages[i] for i in seen if i not in ours),
        key=lambda p: (p["name"].lower(), p["version"]),
    )


def licence_texts(package):
    """(file name, text) for each licence file in the package's source: in
    its own folder, and one level down (bundled fonts keep theirs beside them)."""
    folder = os.path.dirname(package["manifest_path"])
    places = [folder] + sorted(
        os.path.join(folder, d)
        for d in os.listdir(folder)
        if os.path.isdir(os.path.join(folder, d)) and d not in SKIP
    )
    found = []
    for place in places:
        inner = place != folder
        for name in sorted(os.listdir(place)):
            path = os.path.join(place, name)
            if not os.path.isfile(path):
                continue
            # beside bundled fonts every text file is a licence or a notice
            is_text = name.lower().endswith(".txt")
            if LICENCE_FILE.search(name) or (inner and is_text and os.path.basename(place) == "fonts"):
                shown = f"{os.path.basename(place)}/{name}" if inner else name
                with open(path, encoding="utf-8", errors="replace") as f:
                    found.append((shown, f.read().replace("\r\n", "\n").strip()))
    return found


def main():
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--filter-platform", TARGET],
            check=True,
            capture_output=True,
            text=True,
            encoding="utf-8",
        ).stdout
    )
    packages = linked_packages(meta)

    out = [
        "LogSeek Studio: third-party licences",
        "=" * 36,
        "",
        "LogSeek Studio is built from the libraries listed here. Each is used",
        "under its own licence. Where a library offers a choice of licences,",
        "LogSeek Studio uses it under the first one listed that is compatible",
        "with the GNU Affero General Public License, version 3 or later.",
        "The licence texts the libraries ship follow the list.",
        "",
        f"{len(packages)} libraries",
        "",
    ]

    texts = {}  # digest -> (text, [who ships it])
    without = []
    for p in packages:
        label = f"{p['name']} {p['version']}"
        out.append(label)
        out.append(f"    Licence: {p.get('license') or 'see the licence file'}")
        # names only: an address in a notice is of no use to a reader
        names = [ADDRESS.sub("", a).strip() for a in p.get("authors") or []]
        names = [n for n in names if n]
        if names:
            out.append("    Authors: " + "; ".join(names))
        if p.get("repository"):
            out.append(f"    Source:  {p['repository']}")
        found = licence_texts(p)
        if not found:
            without.append(label)
            out.append(
                "    (its package holds no licence file; the text of its licence is among those below)"
            )
        for name, text in found:
            digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
            texts.setdefault(digest, (text, []))[1].append(f"{label} ({name})")
        out.append("")

    out += ["", "Licence texts", "=" * 13]
    for text, users in sorted(texts.values(), key=lambda t: t[1][0].lower()):
        out += ["", "-" * 72, "Shipped with: " + ", ".join(users), "-" * 72, "", text]
    out.append("")

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(out))
    print(f"{OUT}: {len(packages)} libraries, {len(texts)} licence texts")
    if without:
        print("no licence file in the package of: " + ", ".join(without), file=sys.stderr)


if __name__ == "__main__":
    main()

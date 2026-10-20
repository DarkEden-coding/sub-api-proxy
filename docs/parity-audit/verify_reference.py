#!/usr/bin/env python3
"""Check the frozen inventory against a read-only Go clone, without checking it out.

Usage: python3 docs/parity-audit/verify_reference.py /path/to/CLIProxyAPI

Derived from Go and compared both ways: test-suite files and their declarations,
literal router registration call sites (method and path suffix; a registration
inside a method loop accepts any method, and group and constant prefixes are not
resolved), YAML config leaves, CLI flags, plugin RPC methods and
added plugin API fields. Retained non-suite rows keep their text; suite rows are
checked by filename and declarations, and the listed rewrites are exempt. Not
checked: Rust behavior, fixture assertion coverage, whether each item's prose
(handler, type, default, meaning) is accurate, and routes served without a literal
path registration (the WebUI OAuth forwarder, the AI Studio relay route and plugin
routes).
"""

from collections import Counter
from functools import cache
from pathlib import Path
import re
import subprocess
import sys

import audit

OLD = "6fecc6e5567912661654a4eaf9b8f5436facd1c2"
NEW = "ae3ee3be303fb07fb8173279923413005bf32177"
BASE = "d1261f2"  # The Rust tree whose checklist holds the retained IDs.
TEST = re.compile(r"^func ((?:Test|Fuzz|Example|Benchmark)\w*)\(", re.M)
LINK = re.compile(r"https://github.com/router-for-me/CLIProxyAPI/blob/([a-f0-9]+)/([^#)]+)#L(\d+)(?:-L(\d+))?")
# Added Go test files with no declaration, excluded like the original support helpers.
SUPPORT_ONLY = {"internal/cache/issue6332_export_test.go"}
# Retained IDs whose contract text deliberately changed (reference-v8.0.35.md).
MOVED = {"M1-0027", "M1-0048", "M1-0049", "M1-0050", "M1-0051", "M1-0052", "M1-0053", "M1-0054", "M1-0055",
         "M1-0056", "M3-0137", "M3-0138", "M3-0139", "M3-0140", "M3-0141", "M3-0142", "M4-0133", "M4-0134"}
REWRITTEN = MOVED | {"M1-0042", "M1-0062", "M1-0063", "M2-0014", "M3-0031", "M4-0230",
                     "M5-0042", "M5-0043", "M5-0065", "M5-0066"}
# Route items Go serves without a literal path registration: the WebUI OAuth
# forwarder (a bare http.Server handler) and the AI Studio relay route.
DYNAMIC_ROUTES = {"M5-0020", "M5-0021"}
CONFIG_DIRS = ("internal/config/", "internal/registry/", "internal/pluginstore/", "sdk/pluginstore/", "sdk/config/")
ROUTE_SECTIONS = ("1. Public", "Temporary OAuth", "/v8/management", "/v0/management")
REGISTRATION = re.compile(r'\b\w+\.(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|Any|HandleFunc|Handle)\('
                          r'\s*(?:(\w+),\s*)?(?:\w+\s*\+\s*)?"(/[^"]*)"')
CITATION = re.compile(r"\b((?:cmd|internal|sdk)/[\w./\-]+\.go):(\d+)(?:-(\d+))?")
STRUCT = re.compile(r"^type (\w+) struct \{\n(.*?)^\}", re.M | re.S)


def check(ok, message):
    # Explicit failures: `assert` disappears under `python -O`.
    if not ok:
        sys.exit(f"FAIL: {message}")


def normalized(text):
    """Contract text without Go line numbers or LOC counts, which follow the tree."""
    text = audit.LINK.sub(r"\1", text)
    text = re.sub(r"(\.(?:go|md|json|yaml)):\d+(?:-\d+)?", r"\1", text)
    return re.sub(r"\d+ LOC", "N LOC", text)


def main():
    clone = sys.argv[1]

    def git(*args):
        return subprocess.check_output(["git", "-C", clone, *args], text=True)

    @cache
    def source(ref, path):
        return git("show", f"{ref}:{path}")

    @cache
    def tree(ref):
        return git("ls-tree", "-r", "--name-only", ref).splitlines()

    def go_files(ref, prefixes):
        return [p for p in tree(ref) if p.startswith(prefixes) and p.endswith(".go") and not p.endswith("_test.go")]

    def test_files(ref):
        return {p for p in tree(ref) if p.endswith("_test.go")}

    check(git("rev-parse", "v8.0.35^{commit}").strip() == NEW, "v8.0.35 does not match the frozen commit")
    rows = audit.items()
    by_id = {r["id"]: r for r in rows}
    check(len(by_id) == len(rows), "duplicate checklist IDs")

    # Stable IDs: retained IDs keep their milestone and section. The text checks below
    # have explicit exceptions: suite rows and the listed rewrites.
    original = subprocess.check_output(
        ["git", "-C", audit.ROOT, "show", f"{BASE}:docs/parity-audit/checklist.md"], text=True)
    old, section = {}, None
    for line in original.splitlines():
        if m := re.match(r"^(#{2,3}) (.*)", line):
            section = m[2].strip()
        if m := audit.ITEM.match(line):
            old[m[2]] = {"ms": m[1], "section": section, "text": audit.LINK.sub(r"\1", m[3].strip())}
    check(old.keys() <= by_id.keys(), f"existing IDs removed: {sorted(old.keys() - by_id.keys())}")
    changed = set()
    for i, before in old.items():
        now = by_id[i]
        check((now["ms"], now["section"]) == (before["ms"], before["section"]), f"{i} moved milestone or section")
        suite = audit.test_file(before["text"])
        if suite:
            # Declaration lists follow Go and are checked against it below.
            check(audit.test_file(now["text"]) == suite, f"suite ID changed its file: {i}")
        elif normalized(now["text"]) != normalized(before["text"]):
            changed.add(i)
    check(changed == REWRITTEN, f"retained contracts rewritten without a listed reason: {sorted(changed ^ REWRITTEN)}")
    aliases = set(re.findall(r'\{"(oauth\.providers\.[^"]+)", "(upstream\.[^"]+)"\}',
                             source(NEW, "internal/config/config_v8.go")))
    for i in MOVED:
        canonical = re.match(r"`([^`]+)` \(previous v8 alias `([^`]+)`", by_id[i]["text"])
        check(canonical and old[i]["text"].startswith(f"`{canonical[2]}`")
              and (canonical[2], canonical[1]) in aliases, f"{i} is not a Go upstream.* move")
    new_ids = by_id.keys() - old.keys()
    check(len(new_ids) == 264, f"expected 264 additions, got {len(new_ids)}")
    # The freeze recorded every addition as partial or missing; porting work may since
    # have covered some, so only a judgment with evidence is required now.
    manual = audit.load_manual()
    check(all(manual.get(i, {}).get("status") in ("covered", "partial", "missing")
              and manual[i]["evidence"] for i in new_ids),
          "every addition needs a judgment and evidence")

    # Test suites and their declarations.
    suites = {audit.test_file(r["text"]): r for r in rows if audit.test_file(r["text"])}
    check(len(suites) == 989, f"expected 989 suite files, got {len(suites)}")
    check(sum(bool(audit.test_file(r["text"])) for r in rows) == len(suites), "duplicate suite files")
    added = test_files(NEW) - test_files(OLD)
    check(len(added) == 228, f"expected 228 added Go test files, got {len(added)}")
    check(added - SUPPORT_ONLY <= suites.keys(), f"added suites missing: {sorted(added - SUPPORT_ONLY - suites.keys())}")
    check(not SUPPORT_ONLY & suites.keys(), "support-only files are not suites")
    check(not any(TEST.search(source(NEW, p)) for p in SUPPORT_ONLY), "a support-only file declares tests")
    check(not test_files(OLD) - test_files(NEW), "removed Go test files need explicit retirement")
    retired = 0
    for path, row in suites.items():
        text = source(NEW, path)
        declarations = [(m[1], text.count("\n", 0, m.start()) + 1) for m in TEST.finditer(text)]
        listed = [(name, int(line)) for name, line in re.findall(r"`((?:Test|Fuzz|Benchmark|Example)\w*)` \(L(\d+)\)", row["text"])]
        check(listed == declarations, f"stale declarations or line numbers: {row['id']} {path}")
        note = re.search(r"Removed upstream declarations: (.*?) \(historical, not active requirements\)", row["text"])
        if note:
            names = re.findall(r"`([^`]+)`", note[1])
            retired += len(names)
            check(not set(names) & set(audit.test_cases(row["text"])), f"retired cases still active: {path}")
    check(retired == 43, f"expected 43 retired declarations, got {retired}")

    page = Path(audit.HERE, "checklist.md").read_text()
    citations = LINK.findall(page)
    for ref, path, first, last in citations:
        check(ref == NEW, f"stale reference: {path}")
        lines = source(ref, path).splitlines()
        check(1 <= int(first) <= int(last or first) <= len(lines), f"out-of-bounds citation: {path}:{first}-{last}")
    cited = {}  # Go path -> [(first, last, id)]
    for row in rows:
        for path, first, last in CITATION.findall(row["text"]):
            cited.setdefault(path, []).append((int(first), int(last or first), row["id"]))

    def citing(path, line):
        return {i for first, last, i in cited.get(path, ()) if first <= line <= last}

    # Routes: every literal registration is an item citing it, and every route item cites one.
    registrations = []
    for path in go_files(NEW, ("internal/", "sdk/")):
        text = source(NEW, path)
        if "Handle" not in text and ".GET(" not in text and ".POST(" not in text:
            continue
        for number, line in enumerate(text.splitlines(), 1):
            if m := REGISTRATION.search(line):
                method = "ANY" if m[1] in ("Any", "HandleFunc") or (m[1] == "Handle" and not m[2]) else \
                    "*" if m[1] == "Handle" else m[1]
                registrations.append((path, number, method, m[3]))
    route_items = {}
    for row in rows:
        if row["section"].startswith(ROUTE_SECTIONS) and (m := re.search(r"`([A-Z]+) (/[^`\s]*)`", row["text"])):
            route_items[row["id"]] = (m[1], m[2])

    def matches(item, registration):
        method, path = route_items[item]
        return registration[2] in (method, "*") and path.endswith(registration[3])

    for registration in registrations:
        check(any(matches(i, registration) for i in citing(*registration[:2]) if i in route_items),
              f"route registration missing from the checklist: {registration}")
    for item in route_items.keys() - DYNAMIC_ROUTES:
        check(any(item in citing(*r[:2]) and matches(item, r) for r in registrations),
              f"{item} does not cite a matching Go registration")

    # Config leaves: YAML paths derived from the Config struct equal the checklist's legacy paths.
    def leaves(ref):
        structs, alias = {}, {}
        for path in go_files(ref, CONFIG_DIRS):
            text = source(ref, path)
            structs.update((m[1], m[2]) for m in STRUCT.finditer(text))
            alias.update((m[1], m[2]) for m in re.finditer(r"^type (\w+) = (?:\w+\.)?(\w+)\s*$", text, re.M)
                         if m[1] != m[2])
        out = set()

        def walk(name, prefix, seen):
            for field, kind, tag in re.findall(r'^[ \t]*(\w+)\s+([^`\n]*?)\s*`[^`]*yaml:"([^"]*)"', structs[name], re.M):
                key, *options = tag.split(",")
                kind = kind or field  # Embedded struct.
                if key == "-" or (not key and "inline" not in options):
                    continue
                suffix = ""
                while True:
                    kind = kind.lstrip("*")
                    if kind.startswith("[]"):
                        suffix, kind = suffix + "[]", kind[2:]
                    elif m := re.match(r"map\[[\w.]+\](.*)", kind):
                        suffix, kind = suffix + ".{}", m[1]
                    else:
                        break
                base = kind.split(".")[-1]
                base = alias.get(base, base)
                path = prefix + key
                if base in structs and base not in seen:
                    walk(base, path + suffix + "." if key else prefix, seen | {base})
                else:
                    out.add(path)

        walk("Config", "", {"Config"})
        return out

    config_rows = [r for r in rows if r["section"].startswith("5. Config")]
    listed = Counter()
    for row in config_rows:
        if m := re.search(r"legacy `([^`]+)`\)", row["text"]):
            listed[re.sub(r"\{[^}]*\}", "{}", m[1])] += 1
    go_leaves, old_leaves = leaves(NEW), leaves(OLD)
    check(max(listed.values()) == 1, "duplicate config leaves")
    check(set(listed) == go_leaves, f"config leaves differ from Go: {sorted(set(listed) ^ go_leaves)}")
    added_leaves = {re.sub(r"\{[^}]*\}", "{}", re.search(r"legacy `([^`]+)`\)", r["text"])[1])
                    for r in config_rows if r["section"] == "5. Config: v8.0.35 additions"}
    check(added_leaves == go_leaves - old_leaves and old_leaves <= go_leaves, "config additions differ from Go")

    # CLI flags, including the discover subcommand's flag set.
    main_go = source(NEW, "cmd/server/main.go")
    flags = {f"--{n}" for n in re.findall(r'^\s*flag\.\w+\((?:&\w+, )?"([^"]+)"', main_go, re.M)}
    flags |= {f"discover --{n}" for n in re.findall(r'\bdiscoverFlags\.\w+\((?:&\w+, )?"([^"]+)"', main_go, re.M)}
    listed_flags = [m[1] for r in rows if r["section"] == "CLI flags" and (m := re.match(r"`((?:discover )?--[\w-]+)`", r["text"]))]
    check(len(listed_flags) == len(set(listed_flags)) and set(listed_flags) == flags, "CLI flag inventory differs from Go")

    # Plugin ABI: RPC methods, and every exported API field added since the old reference is cited.
    methods = set(re.findall(r'^\s*Method\w+\s*=\s*"([^"]+)"', source(NEW, "sdk/pluginabi/types.go"), re.M))
    listed_methods = [m[1] for r in rows if (m := re.match(r"RPC `([^`]+)`", r["text"]))]
    check(len(listed_methods) == len(set(listed_methods)) and set(listed_methods) == methods, "RPC method inventory differs from Go")

    def api_fields(ref):
        out = {}
        for path in go_files(ref, ("sdk/pluginapi/", "sdk/pluginabi/")):
            text = source(ref, path)
            for m in STRUCT.finditer(text):
                for f in re.finditer(r"^\t([A-Z]\w*)\s+\S", m[2], re.M):
                    out[f"{m[1]}.{f[1]}"] = (path, text.count("\n", 0, m.start(2) + f.start()) + 1)
        return out

    fields, old_fields = api_fields(NEW), api_fields(OLD)
    check(old_fields.keys() <= fields.keys(), "plugin API fields removed")
    added_fields = fields.keys() - old_fields.keys()
    for name in added_fields:
        path, line = fields[name]
        whole = len(source(NEW, path).splitlines())
        # A whole-file citation (a suite's source list) does not inventory a field.
        check(any(first <= line <= last and (first, last) != (1, whole) for first, last, _ in cited.get(path, ())),
              f"added plugin API field not cited: {name}")
    for row in rows:
        if row["text"].startswith("Plugin data contract"):
            for name in re.findall(r"\b([A-Z]\w*\.[A-Z]\w*)\b", row["text"].split(". ")[0]):
                check(name in added_fields and row["id"] in citing(*fields[name]), f"{row['id']} names {name} wrongly")

    totals = Counter(r["ms"] for r in rows)
    check(totals == {"M1": 151, "M2": 189, "M3": 424, "M4": 589, "M5": 331, "M6": 267}, totals)
    print(f"OK: {len(rows)} items, {len(old)} retained IDs (non-suite text compared; {len(changed)} listed "
          f"rewrites exempt, the {len(MOVED)} moves checked by alias only); "
          f"{len(added) - len(SUPPORT_ONLY)} added / {len(suites)} total suites; {retired} retired declarations; "
          f"{len(registrations)} literal route call sites (method and path suffix; method-loop registrations accept any method); {len(go_leaves)} config leaves "
          f"({len(go_leaves - old_leaves)} added); {len(flags)} CLI flags; {len(methods)} RPC methods; "
          f"{len(added_fields)} added plugin API fields; {len(citations)} current, in-bounds citations")
    print("Not checked: Rust behavior, item prose (handlers, types, defaults, meanings), suite-row text "
          "beyond filename and declarations, the remaining text of listed rewrites, route group and constant "
          "prefixes, plugin capabilities and pre-existing data contracts, and routes without a literal path "
          f"({', '.join(sorted(DYNAMIC_ROUTES))} and plugin routes)")


if __name__ == "__main__":
    main()

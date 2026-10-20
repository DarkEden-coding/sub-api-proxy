#!/usr/bin/env python3
"""Check the frozen inventory against a read-only Go clone, without checking it out.

Usage: python3 docs/parity-audit/verify_reference.py /path/to/CLIProxyAPI
This checks source inventory, not Rust behavior or fixture assertion coverage.
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
TEST = re.compile(r"^func ((?:Test|Fuzz|Example|Benchmark)\w*)\(", re.M)
LINK = re.compile(r"https://github.com/router-for-me/CLIProxyAPI/blob/([a-f0-9]+)/([^#)]+)#L(\d+)(?:-L(\d+))?")


def main():
    clone = sys.argv[1]

    def git(*args):
        return subprocess.check_output(["git", "-C", clone, *args], text=True)

    @cache
    def source(ref, path):
        return git("show", f"{ref}:{path}")

    def test_files(ref):
        return {p for p in git("ls-tree", "-r", "--name-only", ref).splitlines() if p.endswith("_test.go")}

    assert git("rev-parse", "v8.0.35^{commit}").strip() == NEW, "v8.0.35 does not match the frozen commit"
    rows = audit.items()
    ids = [r["id"] for r in rows]
    assert len(ids) == len(set(ids)), "duplicate checklist IDs"
    original = subprocess.check_output(
        ["git", "-C", audit.ROOT, "show", "d1261f2:docs/parity-audit/checklist.md"], text=True)
    old_items = {m[2]: audit.LINK.sub(r"\1", m[3]) for line in original.splitlines() if (m := audit.ITEM.match(line))}
    old_ids = set(old_items)
    assert old_ids <= set(ids), "an existing ID was removed"
    for row in rows:
        if row["id"] in old_items:
            assert audit.test_file(old_items[row["id"]]) == audit.test_file(row["text"]), f"suite ID changed its file: {row['id']}"
    new_ids = set(ids) - old_ids
    assert len(new_ids) == 265, f"expected 265 additions, got {len(new_ids)}"
    manual = audit.load_manual()
    assert all(manual[i]["status"] in ("partial", "missing") and manual[i]["evidence"] for i in new_ids), \
        "every addition needs a conservative judgment and evidence"

    suites = {audit.test_file(r["text"]): r for r in rows if audit.test_file(r["text"])}
    assert len(suites) == 990, f"expected 990 suite files, got {len(suites)}"
    assert sum(bool(audit.test_file(r["text"])) for r in rows) == len(suites), "duplicate suite files"
    added = test_files(NEW) - test_files(OLD)
    assert len(added) == 228 and added <= suites.keys(), "added test-file inventory differs from Go"
    assert not test_files(OLD) - test_files(NEW), "removed Go test files need explicit retirement"
    retired = 0
    for path, row in suites.items():
        text = source(NEW, path)
        declarations = [(m[1], text.count("\n", 0, m.start()) + 1) for m in TEST.finditer(text)]
        listed = [(name, int(line)) for name, line in re.findall(r"`((?:Test|Fuzz|Benchmark|Example)\w*)` \(L(\d+)\)", row["text"])]
        assert listed == declarations, f"stale declarations or line numbers: {row['id']} {path}"
        note = re.search(r"Removed upstream declarations: (.*?) \(historical, not active requirements\)", row["text"])
        if note:
            names = re.findall(r"`([^`]+)`", note[1])
            retired += len(names)
            assert not set(names) & set(audit.test_cases(row["text"])), f"retired cases still active: {path}"
    assert retired == 43, f"expected 43 retired declarations, got {retired}"

    page = Path(audit.HERE, "checklist.md").read_text()
    citations = LINK.findall(page)
    for ref, path, first, last in citations:
        assert ref == NEW, f"stale reference: {path}"
        lines = source(ref, path).splitlines()
        assert 1 <= int(first) <= int(last or first) <= len(lines), f"out-of-bounds citation: {path}:{first}-{last}"
    methods = set(re.findall(r'^\s*Method\w+\s*=\s*"([^"]+)"', source(NEW, "sdk/pluginabi/types.go"), re.M))
    listed_methods = [m[1] for r in rows if (m := re.match(r"RPC `([^`]+)`", r["text"]))]
    assert len(listed_methods) == len(set(listed_methods)) and set(listed_methods) == methods, "RPC method inventory differs from Go"
    totals = Counter(r["ms"] for r in rows)
    assert totals == {"M1": 151, "M2": 189, "M3": 424, "M4": 590, "M5": 331, "M6": 267}, totals
    print(f"OK: {len(ids)} stable-ID items; {len(added)} added / {len(suites)} total suites; "
          f"{retired} retired declarations; {len(methods)} RPC methods; {len(citations)} current, in-bounds citations")


if __name__ == "__main__":
    main()

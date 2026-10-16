#!/usr/bin/env python3
"""Print the CHANGELOG.md body for one version heading (Keep a Changelog)."""
import re
import sys
from pathlib import Path

if len(sys.argv) != 2:
    sys.exit("usage: changelog-notes.py VERSION")

version = sys.argv[1]
text = Path("CHANGELOG.md").read_text()
matches = list(re.finditer(r"^## \[([^\]]+)\][^\n]*\n", text, re.M))
for i, match in enumerate(matches):
    if match.group(1) == version:
        start = match.end()
        end = matches[i + 1].start() if i + 1 < len(matches) else len(text)
        body = text[start:end].strip()
        if not body:
            sys.exit(f"empty CHANGELOG section for {version}")
        print(body)
        raise SystemExit(0)
sys.exit(f"CHANGELOG.md has no ## [{version}] section")

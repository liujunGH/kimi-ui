#!/usr/bin/env python3
"""Write latest.json — the manifest tauri-plugin-updater reads.

Kept as a separate script so the updater's contract (version, per-platform
artifact URL, minisign signature, notes) is readable in one place instead of
buried in a shell heredoc.

Inputs (env): VERSION, GITHUB_REPOSITORY (optional).
Reads: notes.md, kimi-ui-macos-arm64.app.tar.gz.sig
Writes: latest.json
"""
import datetime
import json
import os
import sys

ARTIFACT = "kimi-ui-macos-arm64.app.tar.gz"
SIG_FILE = ARTIFACT + ".sig"


def main() -> int:
    version = os.environ.get("VERSION", "").lstrip("v")
    if not version:
        print("error: VERSION env var required", file=sys.stderr)
        return 1

    try:
        signature = open(SIG_FILE).read().strip()
    except OSError as err:
        print(f"error: cannot read {SIG_FILE}: {err}", file=sys.stderr)
        return 1
    if not signature:
        print(f"error: {SIG_FILE} is empty — signing failed?", file=sys.stderr)
        return 1

    notes = ""
    try:
        notes = open("notes.md").read().strip()
    except OSError:
        pass

    repo = os.environ.get("GITHUB_REPOSITORY", "liujunGH/kimi-ui")
    url = (
        f"https://github.com/{repo}/releases/download/"
        f"v{version}/{ARTIFACT}"
    )

    manifest = {
        "version": version,
        "notes": notes,
        "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        ),
        "platforms": {
            "darwin-aarch64": {"signature": signature, "url": url},
        },
    }
    with open("latest.json", "w") as fh:
        json.dump(manifest, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    print(f"wrote latest.json for v{version} (darwin-aarch64)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

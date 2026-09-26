#!/usr/bin/env python3
"""Reproducible lexical unsafe-boundary inventory; this is NOT a Rust AST audit.

Compare a local Git revision with the working tree without checking out files.
Comments/strings count in the rg-compatible lexical figures. A small tokenizer
masks comments and Rust string/character literals only for structural ranges and
the separate code-token figure. Exact #[cfg(test)]/#[test] items and statements
are classified as source tests; #[cfg(test)] external modules inherit that class.
No macro expansion, general cfg evaluation, type resolution, or safety scoring.
"""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

VERSION = "1.0"
GROUPS = ("src_production", "src_test", "integration_tests", "examples", "build_script")
WORD = re.compile(r"\bunsafe\b")
TEST_ATTR = re.compile(r"#\s*\[\s*(?:cfg\s*\(\s*test\s*\)|test)\s*\]")
DECODE = re.compile(r"\b(?:lparam|payload)\s*\.\s*0\s+as\s+\*(?:const|mut)\s+(\w+)")
MESSAGE_TYPES = {"CREATESTRUCTW", "RECT", "DRAWITEMSTRUCT", "NMHDR", "NMCUSTOMDRAW"}
HOOK_TYPES = {"MSLLHOOKSTRUCT", "KBDLLHOOKSTRUCT"}
OWNER_ROLES = {
    "ScreenDc": "desktop_dc", "DesktopDc": "desktop_dc", "WindowDc": "window_dc",
    "MemoryDc": "memory_dc", "OwnedBitmap": "bitmap", "OwnedFont": "font",
    "Font": "font", "FrostedPanel": "combined_backdrop_dc_bitmap",
}
BOUNDARY_CALLS = (
    "SelectObject", "ReleaseDC", "DeleteDC", "DeleteObject", "BeginPaint", "EndPaint",
    "DestroyWindow", "SetWindowLongPtrW", "GetWindowLongPtrW", "SetWindowSubclass",
    "CloseHandle", "GdiFlush",
)


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args]).decode("utf-8")


def mask_noncode(source: str) -> str:
    """Preserve positions/newlines while masking nested comments and literals."""
    out = list(source)
    cursor = 0
    while cursor < len(source):
        end = cursor
        if source.startswith("//", cursor):
            end = source.find("\n", cursor)
            if end < 0:
                end = len(source)
        elif source.startswith("/*", cursor):
            end, depth = cursor + 2, 1
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
        else:
            raw = re.match(r'(?:br|rb|r)(#*)"', source[cursor:])
            if raw and (cursor == 0 or not (source[cursor - 1].isalnum() or source[cursor - 1] == "_")):
                closer = '"' + raw[1]
                close = source.find(closer, cursor + raw.end())
                if close < 0:
                    raise ValueError("Unclosed Rust raw string")
                end = close + len(closer)
            elif source[cursor] == '"':
                end = cursor + 1
                while end < len(source):
                    if source[end] == "\\":
                        end += 2
                    elif source[end] == '"':
                        end += 1
                        break
                    else:
                        end += 1
            elif source[cursor] == "'":
                # A lifetime ('a or '_) has no closing quote and remains code.
                char = re.match(r"'(?:\\(?:u\{[0-9A-Fa-f_]+\}|x[0-9A-Fa-f]{2}|.)|[^'\\\n])'", source[cursor:])
                if char:
                    end = cursor + char.end()
        if end > cursor:
            for index in range(cursor, end):
                if out[index] != "\n":
                    out[index] = " "
            cursor = end
        else:
            cursor += 1
    return "".join(out)


def matching(code: str, start: int, opener: str = "{", closer: str = "}") -> int:
    depth = 1
    cursor = start + 1
    while cursor < len(code):
        if code[cursor] == opener:
            depth += 1
        elif code[cursor] == closer:
            depth -= 1
            if depth == 0:
                return cursor + 1
        cursor += 1
    raise ValueError(f"Unbalanced {opener} at offset {start}")


def attributed_extent(code: str, start: int) -> int:
    """Return end of the following item, statement, or macro invocation."""
    cursor = start
    while cursor < len(code):
        if code[cursor].isspace():
            cursor += 1
        elif code.startswith("#[", cursor):
            cursor = matching(code, cursor + 1, "[", "]")
        elif code[cursor] == "(":
            cursor = matching(code, cursor, "(", ")")
        elif code[cursor] == "[":
            cursor = matching(code, cursor, "[", "]")
        elif code[cursor] == "{":
            return matching(code, cursor)
        elif code[cursor] == ";":
            return cursor + 1
        else:
            cursor += 1
    raise ValueError("Test attribute has no following item/statement")


def source_files(root: Path, revision: str | None) -> dict[str, str]:
    if revision:
        paths = git(root, "ls-tree", "-r", "--name-only", revision, "--", "src", "tests", "examples", "build.rs").splitlines()
        return {path: git(root, "show", f"{revision}:{path}") for path in paths if path.endswith(".rs")}
    paths = [path for folder in ("src", "tests", "examples") for path in (root / folder).rglob("*.rs")]
    paths.append(root / "build.rs")
    return {path.relative_to(root).as_posix(): path.read_text(encoding="utf-8-sig") for path in sorted(paths) if path.exists()}


def inventory(files: dict[str, str]) -> dict:
    masked = {path: mask_noncode(source) for path, source in files.items()}
    ranges = {
        path: [(match.start(), attributed_extent(code, match.end())) for match in TEST_ATTR.finditer(code)]
        for path, code in masked.items()
    }
    external_tests: set[str] = set()
    for path, code in masked.items():
        for start, end in ranges[path]:
            declaration = re.search(r"\bmod\s+(\w+)\s*;", code[start:end])
            if declaration:
                parent = Path(path).parent
                if Path(path).name not in ("mod.rs", "lib.rs", "main.rs"):
                    parent /= Path(path).stem
                for candidate in (parent / f"{declaration[1]}.rs", parent / declaration[1] / "mod.rs"):
                    if candidate.as_posix() in files:
                        external_tests.add(candidate.as_posix())

    def group_at(path: str, position: int) -> str:
        if path.startswith("tests/"):
            return "integration_tests"
        if path.startswith("examples/"):
            return "examples"
        if path == "build.rs":
            return "build_script"
        if path in external_tests or any(start <= position < end for start, end in ranges[path]):
            return "src_test"
        return "src_production"

    def site(path: str, position: int, **extra: object) -> dict:
        return {"file": path, "line": files[path].count("\n", 0, position) + 1,
                "group": group_at(path, position), **extra}

    totals = {group: {"lexical_unsafe_lines": 0, "lexical_unsafe_occurrences": 0, "code_unsafe_tokens": 0} for group in GROUPS}
    by_file = {}
    owners, messages, hooks, slices, unsafe_lexical = [], [], [], [], []
    calls = {name: [] for name in BOUNDARY_CALLS}
    for path, source in files.items():
        code = masked[path]
        counts = defaultdict(Counter)
        lines = defaultdict(set)
        for match in WORD.finditer(source):
            group = group_at(path, match.start())
            line = source.count("\n", 0, match.start()) + 1
            lines[group].add(line)
            counts[group]["lexical_unsafe_occurrences"] += 1
            unsafe_lexical.append(site(path, match.start()))
        for match in WORD.finditer(code):
            counts[group_at(path, match.start())]["code_unsafe_tokens"] += 1
        for group, locations in lines.items():
            counts[group]["lexical_unsafe_lines"] = len(locations)
        by_file[path] = dict(counts)
        for group, metrics in counts.items():
            for metric, count in metrics.items():
                totals[group][metric] += count
        for match in re.finditer(r"\bimpl\s+Drop\s+for\s+(\w+)", code):
            if match[1] in OWNER_ROLES and path.startswith("src/"):
                owners.append(site(path, match.start(), type=match[1], role=OWNER_ROLES[match[1]]))
        for match in DECODE.finditer(code):
            if match[1] in MESSAGE_TYPES:
                messages.append(site(path, match.start(), type=match[1]))
            elif match[1] in HOOK_TYPES:
                hooks.append(site(path, match.start(), type=match[1]))
        for match in re.finditer(r"\bfrom_raw_parts(?:_mut)?\s*\(", code):
            slices.append(site(path, match.start(), function=match[0].rstrip(" (")))
        for name in BOUNDARY_CALLS:
            for match in re.finditer(rf"\b{name}\s*\(", code):
                calls[name].append(site(path, match.start()))
    prod_owners = [owner for owner in owners if owner["group"] == "src_production"]
    roles = Counter(owner["role"] for owner in prod_owners)
    fingerprint = hashlib.sha256()
    for path, source in sorted(files.items()):
        fingerprint.update(path.encode() + b"\0" + source.replace("\r\n", "\n").encode() + b"\0")
    return {
        "source_sha256_lf_normalized": fingerprint.hexdigest(),
        "rust_file_count": len(files), "totals": totals,
        "test_external_modules": sorted(external_tests),
        "production_owner_drop_count": len(prod_owners),
        "production_owner_drop_roles": dict(roles),
        "production_duplicate_owner_drops_beyond_first_per_role": sum(max(0, count - 1) for count in roles.values()),
        "owner_drop_sites": owners,
        "native_message_decode_sites": messages,
        "hook_payload_decode_sites": hooks,
        "raw_slice_sites": slices,
        "native_boundary_call_sites": calls,
        "unsafe_lexical_sites": unsafe_lexical,
        "per_file": by_file,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", required=True, help="Existing local commit/ref; never fetches")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    baseline = git(root, "rev-parse", args.baseline).strip()
    report = {
        "tool": f"scripts/unsafe-audit.py {VERSION}", "python": sys.version,
        "created_utc": datetime.now(timezone.utc).isoformat(),
        "baseline_sha": baseline, "working_tree_head": git(root, "rev-parse", "HEAD").strip(),
        "method": __doc__,
        "owner_metric": "Explicit type-name roles: " + repr(OWNER_ROLES) + "; excludes selection/composition guards, paint sessions, brushes/pens and non-GDI owners.",
        "message_metric": "LPARAM variable lparam/payload .0 cast to named UI or hook payload types; excludes scalar HWND/HDC casts, userdata retrieval, and second-level WindowInit casts.",
        "slice_metric": "Code calls to from_raw_parts/from_raw_parts_mut; inventory must be inspected to distinguish DIB from other storage.",
        "baseline": inventory(source_files(root, baseline)),
        "current": inventory(source_files(root, None)),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n")
    for label in ("baseline", "current"):
        values = report[label]
        print(label, json.dumps(values["totals"]))
        print("resource owner Drops", values["production_owner_drop_count"], values["production_owner_drop_roles"])
        for key in ("native_message_decode_sites", "hook_payload_decode_sites", "raw_slice_sites"):
            print(key, dict(Counter(item["group"] for item in values[key])))


if __name__ == "__main__":
    main()

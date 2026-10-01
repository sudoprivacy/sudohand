#!/usr/bin/env python3
"""Rebuild/check the pinned adb migration inventory. No browser is launched.

Run with Python 3.14 and the reference package's dependencies installed:
  python scripts/audit_adb_reference.py --write
  python scripts/audit_adb_reference.py --check

This validates inventory completeness, not Rust behavior or test execution.
Git blobs make the snapshot independent of checkout line endings.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import importlib
import inspect
import json
import re
import subprocess
import sys
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs/migrations/adb-history-inventory.json"
MATRIX = ROOT / "docs/migrations/adb-behavior-matrix.json"


def git(repo, *args):
    return subprocess.check_output(
        ["git", "-C", str(repo), *args], encoding="utf-8"
    ).strip()


def build(reference):
    pin = json.loads((ROOT / "references/ai-dev-browser-baseline.json").read_text())
    sha = pin["commit"]
    assert git(reference, "rev-parse", "HEAD") == sha, (
        "Reference checkout is not pinned"
    )
    assert git(reference, "rev-parse", "--is-shallow-repository") == "false", (
        "Fetch full reference history"
    )
    assert not git(reference, "status", "--porcelain", "--untracked-files=no"), (
        "Reference has tracked edits"
    )
    paths = git(reference, "ls-tree", "-r", "--name-only", sha).splitlines()
    matrix = json.loads(MATRIX.read_text(encoding="utf-8"))
    assert matrix["reference"] == sha, (
        "Matrix baseline differs from the submodule manifest"
    )
    consumers = {
        r["id"]
        for r in json.loads(
            (ROOT / "docs/migrations/dependency-inventory.json").read_text(
                encoding="utf-8"
            )
        )["repositories"]
    }
    groups = matrix["capabilities"]
    ids = [g["id"] for g in groups]
    assert len(ids) == len(set(ids)), "Duplicate capability IDs"
    by_tool = {}
    by_source = {}
    for g in groups:
        for name in g["tools"]:
            assert name not in by_tool, f"Tool mapped twice: {name}"
            by_tool[name] = g["id"]
        for path in g["sources"]:
            assert path in paths, f"Missing source: {path}"
            by_source.setdefault(path, []).append(g["id"])
        for path in g["tests"]:
            assert path in paths, f"Missing test anchor: {path}"
        for commit in g["history"]:
            git(reference, "merge-base", "--is-ancestor", commit, sha)
        assert g["acceptance"] and g["main"] and g["pr27"] and g["consumers"]
        assert set(g["consumers"]) <= consumers, f"Unknown consumer ID: {g['id']}"

    sys.path.insert(0, str(reference.resolve()))
    from ai_dev_browser import core
    from ai_dev_browser._cli import _generate_parser, _parse_docstring_failure
    from ai_dev_browser.tools._generate import _discover_tools

    contracts = []
    for meta in _discover_tools():
        name = meta["name"]
        module = importlib.import_module(f"ai_dev_browser.tools.{name}")
        parser = _generate_parser(
            getattr(module, name), requires_tab=meta["requires_tab"]
        )
        fn = getattr(core, name)
        source_path = fn.__module__.replace(".", "/") + ".py"
        assert by_tool.get(name) in by_source.get(source_path, []), (
            f"Tool implementation missing from capability sources: {name} -> {source_path}"
        )
        options = []
        for a in parser._actions:
            options.append(
                {
                    "flags": a.option_strings,
                    "dest": a.dest,
                    "required": a.required,
                    "default": a.default,
                    "nargs": a.nargs,
                    "choices": a.choices,
                    "action": type(a).__name__,
                    "type": getattr(a.type, "__qualname__", None),
                    "help": a.help,
                }
            )
        doc = inspect.getdoc(fn) or ""
        contracts.append(
            {
                **meta,
                "capability": by_tool.get(name),
                "signature": str(inspect.signature(fn)),
                "source": source_path,
                "summary": parser.description,
                "failure_hint": _parse_docstring_failure(doc),
                "options": options,
            }
        )
    names = sorted(c["name"] for c in contracts)
    assert names == pin["public_cli_tools"] == sorted(by_tool), (
        "CLI baseline/matrix drift"
    )

    sources, tests, assets = [], [], []
    ci = git(reference, "show", f"{sha}:.github/workflows/ci.yml")
    ci_integration = set(re.findall(r"tests/integration/test_[\w]+\.py", ci))
    for path in paths:
        if path.startswith("ai_dev_browser/") and not path.endswith(".py"):
            assets.append(
                {
                    "path": path,
                    "capabilities": ["C04", "C27"]
                    if "/extension/" in path
                    else ["C23", "C27"],
                }
            )
        if not path.endswith(".py") or not path.startswith(
            ("ai_dev_browser/", "tests/")
        ):
            continue
        code = git(reference, "show", f"{sha}:{path}")
        tree = ast.parse(code)
        if path.startswith("tests/"):
            defs = []
            for node in ast.walk(tree):
                if isinstance(
                    node, (ast.FunctionDef, ast.AsyncFunctionDef)
                ) and node.name.startswith("test_"):
                    defs.append(
                        {
                            "name": node.name,
                            "line": node.lineno,
                            "fixtures": [a.arg for a in node.args.args],
                            "decorators": [ast.unparse(d) for d in node.decorator_list],
                        }
                    )
            tests.append(
                {
                    "path": path,
                    "tests": sorted(defs, key=lambda d: d["line"]),
                    "ci_selection": "unit_directory"
                    if path.startswith("tests/unit/test_")
                    else "explicit_file"
                    if path in ci_integration
                    else "not_explicitly_selected",
                    "mock_tokens": sorted(
                        set(
                            re.findall(
                                r"\b(?:monkeypatch|Mock|AsyncMock|MagicMock|patch)\b",
                                code,
                            )
                        )
                    ),
                    "skip_lines": [
                        i
                        for i, line in enumerate(code.splitlines(), 1)
                        if "pytest.skip" in line or "skipif" in line
                    ],
                    "capabilities": [g["id"] for g in groups if path in g["tests"]],
                }
            )
            continue
        generated = path.startswith("ai_dev_browser/cdp/")
        tool = path.startswith("ai_dev_browser/tools/") and Path(path).stem in by_tool
        mapped = (
            [by_tool[Path(path).stem]]
            if tool
            else ["C23"]
            if generated
            else by_source.get(path, [])
        )
        assert mapped, f"Unmapped production module: {path}"
        declarations = []

        def walk(nodes, declarations, prefix=""):
            for node in nodes:
                if isinstance(node, ast.ClassDef):
                    declarations.append(
                        {
                            "name": prefix + node.name,
                            "kind": "class",
                            "line": node.lineno,
                            "bases": [ast.unparse(b) for b in node.bases],
                            "fields": [
                                ast.unparse(n)
                                for n in node.body
                                if isinstance(n, ast.AnnAssign)
                            ],
                        }
                    )
                    walk(node.body, declarations, prefix + node.name + ".")
                elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    declarations.append(
                        {
                            "name": prefix + node.name,
                            "kind": "async"
                            if isinstance(node, ast.AsyncFunctionDef)
                            else "sync",
                            "line": node.lineno,
                            "args": ast.unparse(node.args),
                            "returns": ast.unparse(node.returns)
                            if node.returns
                            else None,
                            "decorators": [ast.unparse(d) for d in node.decorator_list],
                        }
                    )

        # Generated CDP gets a domain/module inventory; hand-written SDK gets member signatures.
        if not generated and not tool:
            walk(tree.body, declarations)
        sources.append(
            {
                "path": path,
                "capabilities": mapped,
                "generated_cdp": generated,
                "sha256_normalized": hashlib.sha256(code.encode()).hexdigest(),
                "declarations": declarations,
            }
        )

    history = []
    for line in git(
        reference, "log", "--reverse", "--format=%H%x09%as%x09%s", sha
    ).splitlines():
        commit, date, subject = line.split("\t", 2)
        changed = git(
            reference,
            "diff-tree",
            "--root",
            "--no-commit-id",
            "--name-only",
            "-r",
            commit,
        ).splitlines()
        history.append(
            {
                "commit": commit,
                "date": date,
                "subject": subject,
                "paths": changed,
                "capabilities_by_current_path": sorted(
                    {
                        x
                        for p in changed
                        for x in (
                            by_source.get(p, [])
                            + (
                                [by_tool[Path(p).stem]]
                                if p.startswith("ai_dev_browser/tools/")
                                and Path(p).stem in by_tool
                                else []
                            )
                        )
                    }
                ),
                "curated_anchors": [
                    g["id"]
                    for g in groups
                    if any(commit.startswith(h) for h in g["history"])
                ],
            }
        )
    tags = []
    for tag in git(
        reference, "tag", "--merged", sha, "--sort=version:refname"
    ).splitlines():
        tags.append({"tag": tag, "commit": git(reference, "rev-list", "-n", "1", tag)})
    package = tomllib.loads(git(reference, "show", f"{sha}:pyproject.toml"))["project"]
    return {
        "schema": 1,
        "reference": sha,
        "limits": [
            "Commit metadata and changed paths are complete for the pinned ancestry; patch review is separately documented.",
            "CI selection and mock/skip tokens are static evidence, not proof of collection or live execution.",
            "Signatures include private methods used by consumers; generated CDP is inventoried by module.",
            "Parameter/help snapshots do not establish JSON schemas or behavioral equivalence.",
        ],
        "counts": {
            "commits": len(history),
            "tags": len(tags),
            "tools": len(contracts),
            "production_python_modules": len(sources),
            "generated_cdp_modules": sum(s["generated_cdp"] for s in sources),
            "sdk_declarations": sum(len(s["declarations"]) for s in sources),
            "test_files": sum(bool(t["tests"]) for t in tests),
            "test_definitions": sum(len(t["tests"]) for t in tests),
            "integration_files_explicit_in_ci": len(ci_integration),
        },
        "package": {
            key: package[key]
            for key in (
                "requires-python",
                "license",
                "dependencies",
                "optional-dependencies",
            )
        },
        "cli": contracts,
        "sources": sources,
        "assets": assets,
        "tests": tests,
        "tags": tags,
        "history": history,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--reference", type=Path, default=ROOT / "references/ai-dev-browser"
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check", action="store_true")
    args = parser.parse_args()
    result = build(args.reference)
    if args.write:
        OUT.write_text(
            json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
    else:
        assert result == json.loads(OUT.read_text(encoding="utf-8")), (
            "Inventory drift; inspect and rebuild with --write"
        )
    print(json.dumps(result["counts"]))


if __name__ == "__main__":
    main()

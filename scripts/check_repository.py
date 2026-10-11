#!/usr/bin/env python3
"""Check the public checkout's package boundary and documentation links."""
from pathlib import Path
import re
import sys
import tomllib
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]


def check_repository(root: Path) -> list[str]:
    errors = []
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    for member in workspace["members"]:
        manifest_path = root / member / "Cargo.toml"
        manifest = tomllib.loads(manifest_path.read_text())
        package = manifest["package"]
        for field in ("description", "license", "repository"):
            if not package.get(field):
                errors.append(f"{member}: missing package {field}")
        for table in ("dependencies", "dev-dependencies", "build-dependencies"):
            dependencies = [manifest.get(table, {})]
            dependencies.extend(target.get(table, {}) for target in manifest.get("target", {}).values())
            for group in dependencies:
                for name, dependency in group.items():
                    if name.startswith("talos3d-architecture"):
                        errors.append(f"{member}: private domain dependency {name}")
                    if isinstance(dependency, dict) and "path" in dependency:
                        dependency_path = (manifest_path.parent / dependency["path"]).resolve()
                        if not dependency_path.is_relative_to(root.resolve()):
                            errors.append(f"{member}: dependency {name} escapes the public checkout")
                        elif not (dependency_path / "Cargo.toml").is_file():
                            errors.append(f"{member}: dependency {name} has no manifest")
    if "app-core" not in workspace["members"]:
        errors.append("app-core must be checked by the public workspace")

    docs = list(root.glob("*.md")) + list((root / "docs").rglob("*.md"))
    for doc in sorted(docs):
        if doc.name == "WORKTREE_PURPOSE.md":
            continue
        content = re.sub(r"```.*?```", "", doc.read_text(), flags=re.S)
        for match in re.finditer(r"!?\[[^\]\n]*\]\((<[^>]+>|[^\s)]+)(?:\s+\"[^\"]*\")?\)", content):
            target = match.group(1).strip("<>")
            parsed = urlsplit(target)
            if parsed.scheme or not parsed.path:
                continue
            destination = (doc.parent / unquote(parsed.path)).resolve()
            if not destination.is_relative_to(root.resolve()) or not destination.exists():
                line = content[:match.start()].count("\n") + 1
                errors.append(f"{doc.relative_to(root)}:{line}: broken local link {target}")
    return errors


if __name__ == "__main__":
    failures = check_repository(ROOT)
    for failure in failures:
        print(failure, file=sys.stderr)
    print(f"Public repository checks: {'FAIL' if failures else 'PASS'}")
    sys.exit(bool(failures))

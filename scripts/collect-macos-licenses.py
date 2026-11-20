"""Collect license and notice files from the resolved macOS Cargo dependency graph."""

import json
import shutil
import sys
from pathlib import Path


def main():
    destination = Path(sys.argv[1])
    supplemental = Path(__file__).with_name("macos-licenses")
    sources = json.loads((supplemental / "sources.json").read_text(encoding="utf-8"))
    metadata = json.load(sys.stdin)
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    roots = {
        packages[package_id]["name"]: package_id
        for package_id in metadata["workspace_members"]
        if packages[package_id]["name"] in {"wave-bridge", "wave-companion"}
    }
    pending = list(roots.values())
    visited = set()
    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        pending.extend(
            dep["pkg"]
            for dep in nodes[package_id]["deps"]
            if any(kind["kind"] != "dev" for kind in dep["dep_kinds"])
        )

    report = [
        "Stadia Wave — Third-party Rust dependencies",
        "",
        "Includes dependencies used at build time. Where a package offers a choice",
        "of licenses, its original license expression is listed.",
        "Bundled license and notice files are under Rust/.",
        "The Rust standard library's copyright and licenses are under Rust-toolchain/.",
        "",
    ]
    for package in sorted(
        (packages[package_id] for package_id in visited if packages[package_id]["source"]),
        key=lambda package: (package["name"], package["version"], package["source"]),
    ):
        source_dir = Path(package["manifest_path"]).parent
        files = {
            path.relative_to(source_dir): path
            for path in source_dir.rglob("*")
            if not path.is_symlink()
            and path.is_file()
            and path.suffix.lower() not in {".rs", ".c", ".h", ".cpp", ".swift"}
            and path.name.upper().startswith(
                ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE", "UNLICENSE", "THIRD-PARTY")
            )
        }
        if package["license_file"]:
            path = source_dir / package["license_file"]
            files[Path(path.name)] = path

        label = f'{package["name"]}-{package["version"]}'
        directory = destination / "Rust" / label
        vcs = source_dir / ".cargo_vcs_info.json"
        if vcs.is_file():
            revision = json.loads(vcs.read_text(encoding="utf-8"))["git"]["sha1"]
            upstream_notices = sources.get(revision, {})
        else:
            upstream_notices = {}
        for name in upstream_notices:
            files[Path(name)] = supplemental / name
        if label == "mac-0.1.1":
            # The published crate grants the Apache-2.0 option in its README.
            files[Path("README.md")] = source_dir / "README.md"
            files[Path("LICENSE-APACHE.txt")] = destination / "Stadia-Wave-LICENSE.txt"
        if not files:
            sys.exit(f"{label}: no license files found. Add the upstream notices before packaging.")
        for relative, path in sorted(files.items()):
            target = directory / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
        source = package["source"]
        if source.startswith("registry+"):
            source = f'https://crates.io/crates/{package["name"]}/{package["version"]}'
        report.extend(
            [
                f'{label} — {package["license"] or "see upstream license files"}',
                f'  Source: {source}',
                "  Files: " + ", ".join(f"Rust/{label}/{path}" for path in sorted(files)),
                *(f"  Upstream license: {url}" for url in upstream_notices.values()),
                "",
            ]
        )

    (destination / "Rust-dependencies.txt").write_text("\n".join(report), encoding="utf-8")
    print(packages[roots["wave-companion"]]["version"])


if __name__ == "__main__":
    main()

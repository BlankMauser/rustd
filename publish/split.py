#!/usr/bin/env python3
"""Turn a fork-layout rustd checkout into the published layout.

    split.py SRC OUT

SRC is a checkout of the integration branch (rust-analyzer's tree with rustd's changes in
place). OUT is a git repository that receives rustd's own files, a patch series for the
rust-analyzer files rustd changes, and rust-analyzer itself as a submodule pinned to BASE.
Files in publish/overlay (README, docs, workflows, vendor/apply.py) are copied last and win.
"""
import os, re, shutil, subprocess, sys

BASE = "dd5ed69d4b5b26fe7f91013b1d02608cd37ae199"
UPSTREAM = "https://github.com/rust-lang/rust-analyzer.git"
OURS = ["crates/rustd", "crates/rustd-wasm", "harness", "demo"]
CODEGEN = ["crates/hir-ty/src/mir/codegen.rs", "crates/hir-ty/src/mir/codegen"]
REPORTS_KEEP_AT_ROOT = {"LICENSE-APACHE", "LICENSE-MIT", "rustfmt.toml", "clippy.toml"}


def git(cwd, *args, **kw):
    return subprocess.run(["git", "-C", cwd, *args], check=True, text=True, capture_output=True, **kw).stdout


def main():
    src, out = map(os.path.abspath, sys.argv[1:3])
    overlay = os.path.join(os.path.dirname(os.path.abspath(__file__)), "overlay")
    head = git(src, "rev-parse", "HEAD").strip()

    for name in os.listdir(out):
        if name in (".git", ".gitmodules", "vendor"):
            continue
        path = os.path.join(out, name)
        shutil.rmtree(path) if os.path.isdir(path) and not os.path.islink(path) else os.remove(path)
    os.makedirs(f"{out}/vendor", exist_ok=True)
    for name in ("patches", "rustc_abi"):
        shutil.rmtree(f"{out}/vendor/{name}", ignore_errors=True)

    tracked = git(src, "ls-files").splitlines()
    changed = {}
    for line in git(src, "diff", "--name-status", "--no-renames", BASE, head).splitlines():
        status, path = line.split("\t", 1)
        changed[path] = status

    def copy(path, to):
        os.makedirs(os.path.dirname(f"{out}/{to}") or out, exist_ok=True)
        shutil.copy2(f"{src}/{path}", f"{out}/{to}")

    patched = {}
    for path in tracked:
        top = next((o for o in OURS if path == o or path.startswith(o + "/")), None)
        if top:
            copy(path, path)
        elif path == CODEGEN[0] or path.startswith(CODEGEN[1] + "/"):
            copy(path, "crates/hir-ty-codegen/" + path[len("crates/hir-ty/src/mir/"):])
        elif path.startswith("lib/rustc_abi/"):
            copy(path, "vendor/rustc_abi/" + path[len("lib/rustc_abi/"):])
        elif path in REPORTS_KEEP_AT_ROOT:
            copy(path, path)
        elif path == ".github/workflows/rustd.yaml":
            # The same jobs, on main and on pull requests, with the submodule patched first.
            text = open(f"{src}/{path}").read()
            text = text.replace('    branches: ["rustd-*"]\n', '    branches: ["main"]\n  pull_request:\n')
            text = re.sub(
                r"( +)- uses: actions/checkout@v6\n",
                r"\1- uses: actions/checkout@v6\n\1  with:\n\1    submodules: true\n\1- run: python3 vendor/apply.py\n",
                text,
            )
            os.makedirs(f"{out}/.github/workflows", exist_ok=True)
            open(f"{out}/{path}", "w").write(text)
        elif "/" not in path and path.endswith(".md") and changed.get(path) == "A":
            copy(path, "docs/reports/" + path)
        elif path in ("Cargo.lock", ".gitignore") or path.startswith(("publish/", ".github/")):
            pass
        elif path == ".cargo/config.toml":
            copy(path, path)
        elif path in changed:
            parts = path.split("/")
            group = parts[1] if parts[0] in ("crates", "lib") and len(parts) > 2 else "workspace"
            patched.setdefault(group, []).append(path)

    os.makedirs(f"{out}/vendor/patches")
    for group, paths in sorted(patched.items()):
        diff = git(src, "diff", "--no-renames", "--full-index", "--binary", BASE, head, "--", *paths)
        with open(f"{out}/vendor/patches/{group}.patch", "w") as f:
            f.write(diff)
    with open(f"{out}/vendor/rust-analyzer.rev", "w") as f:
        f.write(BASE + "\n")

    manifest = open(f"{src}/Cargo.toml").read()
    manifest = re.sub(r'path = "\./crates/', 'path = "vendor/rust-analyzer/crates/', manifest)
    manifest = re.sub(r'path = "lib/rustc_abi"', 'path = "vendor/rustc_abi"', manifest)
    manifest = re.sub(r"members = \[[^\]]*\]", 'members = ["crates/rustd"]', manifest, count=1)
    manifest = re.sub(r"\nexclude = \[[^\]]*\]", '\nexclude = ["crates/rustd-wasm", "demo", "harness"]', manifest, count=1)
    manifest = manifest.replace('authors = ["rust-analyzer team"]', 'authors = ["rustd contributors"]')
    manifest = manifest.replace("https://github.com/rust-lang/rust-analyzer", "https://github.com/DioxusLabs/rustd")
    open(f"{out}/Cargo.toml", "w").write(manifest)
    shutil.copy2(f"{src}/Cargo.lock", f"{out}/Cargo.lock")
    open(f"{out}/.gitignore", "w").write("target*/\n__pycache__/\n/vendor/.applied\n")

    wasm = f"{out}/crates/rustd-wasm/Cargo.toml"
    text = open(wasm).read()
    text = re.sub(r'path = "\.\./(?!\.\.)', 'path = "../../vendor/rust-analyzer/crates/', text)
    text = text.replace('path = "../../lib/rustc_abi"', 'path = "../../vendor/rustc_abi"')
    open(wasm, "w").write(text)

    if os.path.isdir(overlay):
        shutil.copytree(overlay, out, dirs_exist_ok=True)
    open(f"{out}/vendor/source.rev", "w").write(head + "\n")
    if os.path.exists(f"{out}/vendor/rust-analyzer/.git"):
        # Apply the series and let cargo drop the lockfile entries only rust-analyzer's own
        # binaries needed, so the committed lockfile passes --locked.
        subprocess.run([sys.executable, "vendor/apply.py"], cwd=out, check=True)
        subprocess.run(["cargo", "metadata", "--format-version", "1", "--offline"], cwd=out, check=True, stdout=subprocess.DEVNULL)
    print(f"{len(patched)} patches over {sum(map(len, patched.values()))} files, from {head[:10]}")


main()

#!/usr/bin/env python3
"""Turn a fork-layout rustd checkout into the published layout.

    split.py SRC OUT

SRC is a checkout of the integration branch (rust-analyzer's tree with rustd's changes in
place). OUT is a git repository that receives rustd's own files. Its submodule
vendor/rust-analyzer is our branch of rust-analyzer: BASE with the rust-analyzer files rustd
changes committed on top, which this brings up to date with new commits. The code generator
is one of rustd's own files, and `hir-ty` in that branch names it by path.
Files in publish/overlay (README, docs, workflows) are copied last and win.
"""
import os, re, shutil, subprocess, sys

BASE = "dd5ed69d4b5b26fe7f91013b1d02608cd37ae199"
BRANCH = "rust-analyzer"
REPO = "https://github.com/DioxusLabs/rustd"
CODEGEN_AT = "crates/hir-ty-codegen"
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
    for name in ("apply.py", "rust-analyzer.rev", ".applied"):
        if os.path.exists(f"{out}/vendor/{name}"):
            os.remove(f"{out}/vendor/{name}")

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
        elif path == CODEGEN[0]:
            # A module named by path is the `mod.rs` of its directory.
            copy(path, f"{CODEGEN_AT}/mod.rs")
        elif path.startswith(CODEGEN[1] + "/"):
            copy(path, f"{CODEGEN_AT}/" + path[len(CODEGEN[1]) + 1:])
        elif path.startswith("lib/rustc_abi/"):
            copy(path, "vendor/rustc_abi/" + path[len("lib/rustc_abi/"):])
        elif path in REPORTS_KEEP_AT_ROOT:
            copy(path, path)
        elif path == ".github/workflows/rustd.yaml":
            # The same jobs, on main and on pull requests, with the submodule checked out.
            text = open(f"{src}/{path}").read()
            text = text.replace('    branches: ["rustd-*"]\n', '    branches: ["main"]\n  pull_request:\n')
            text = re.sub(
                r"( +)- uses: actions/checkout@v6\n",
                r"\1- uses: actions/checkout@v6\n\1  with:\n\1    submodules: true\n",
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

    commits = fork(src, f"{out}/vendor/rust-analyzer", head, changed, patched)

    manifest = open(f"{src}/Cargo.toml").read()
    manifest = re.sub(r'path = "\./crates/', 'path = "vendor/rust-analyzer/crates/', manifest)
    manifest = re.sub(r'path = "lib/rustc_abi"', 'path = "vendor/rustc_abi"', manifest)
    manifest = re.sub(r"members = \[[^\]]*\]", 'members = ["crates/rustd"]', manifest, count=1)
    manifest = re.sub(r"\nexclude = \[[^\]]*\]", '\nexclude = ["crates/rustd-wasm", "demo", "harness"]', manifest, count=1)
    manifest = manifest.replace('authors = ["rust-analyzer team"]', 'authors = ["rustd contributors"]')
    manifest = manifest.replace("https://github.com/rust-lang/rust-analyzer", "https://github.com/DioxusLabs/rustd")
    open(f"{out}/Cargo.toml", "w").write(manifest)
    shutil.copy2(f"{src}/Cargo.lock", f"{out}/Cargo.lock")
    open(f"{out}/.gitignore", "w").write("target*/\n__pycache__/\n")
    # The submodule is a branch of this same repository.
    open(f"{out}/.gitmodules", "w").write(
        f'[submodule "vendor/rust-analyzer"]\n\tpath = vendor/rust-analyzer\n\turl = {REPO}.git\n\tbranch = {BRANCH}\n'
    )

    wasm = f"{out}/crates/rustd-wasm/Cargo.toml"
    text = open(wasm).read()
    text = re.sub(r'path = "\.\./(?!\.\.)', 'path = "../../vendor/rust-analyzer/crates/', text)
    text = text.replace('path = "../../lib/rustc_abi"', 'path = "../../vendor/rustc_abi"')
    open(wasm, "w").write(text)

    if os.path.isdir(overlay):
        shutil.copytree(overlay, out, dirs_exist_ok=True)
    open(f"{out}/vendor/source.rev", "w").write(head + "\n")
    # Let cargo drop the lockfile entries only rust-analyzer's own binaries needed, so the
    # committed lockfile passes --locked.
    subprocess.run(["cargo", "metadata", "--format-version", "1", "--offline"], cwd=out, check=True, stdout=subprocess.DEVNULL)
    tip = git(f"{out}/vendor/rust-analyzer", "rev-parse", "HEAD").strip()
    print(f"{commits} new commits over {sum(map(len, patched.values()))} files on {BRANCH}, now {tip[:10]}, from {head[:10]}")


def fork(src, tree, head, changed, patched):
    """Brings the branch of rust-analyzer checked out at `tree` to what `src` has of its
    files, one commit for each crate that differs. Answers how many commits that took."""
    if not os.path.exists(f"{tree}/.git"):
        sys.exit(f"{tree} is not checked out: git submodule update --init")
    known = subprocess.run(["git", "-C", tree, "rev-parse", "--verify", "-q", BRANCH], capture_output=True).returncode == 0
    git(tree, "checkout", "-q", "-f", *([BRANCH] if known else ["-B", BRANCH, BASE]))
    git(tree, "clean", "-q", "-fd")
    ours = {path for paths in patched.values() for path in paths}
    stale = [p for p in git(tree, "diff", "--name-only", "--no-renames", BASE, "HEAD").splitlines() if p not in ours]
    if stale:
        # What rustd no longer changes is rust-analyzer's again.
        for path in stale:
            there = subprocess.run(["git", "-C", tree, "cat-file", "-e", f"{BASE}:{path}"], capture_output=True).returncode == 0
            git(tree, *(("checkout", BASE, "--", path) if there else ("rm", "-q", "-f", path)))
        git(tree, "commit", "-q", "-m", f"rustd: back to rust-analyzer's own, from {head[:10]}")
    commits = int(bool(stale))
    for group, paths in sorted(patched.items()):
        for path in paths:
            if changed[path] == "D":
                if os.path.lexists(f"{tree}/{path}"):
                    os.remove(f"{tree}/{path}")
                continue
            os.makedirs(os.path.dirname(f"{tree}/{path}"), exist_ok=True)
            shutil.copy2(f"{src}/{path}", f"{tree}/{path}")
            if path == "crates/hir-ty/src/mir.rs":
                text = open(f"{tree}/{path}").read()
                assert text.count("\npub mod codegen;\n") == 1
                up = "../" * (len(path.split("/")) + 1)
                text = text.replace(
                    "\npub mod codegen;\n",
                    f'\n// rustd\'s code generator, in the repository this one is a submodule of.\n#[path = "{up}{CODEGEN_AT}/mod.rs"]\npub mod codegen;\n',
                )
                open(f"{tree}/{path}", "w").write(text)
        git(tree, "add", "-A", "-f", "--", *paths)
        if git(tree, "diff", "--cached", "--name-only"):
            git(tree, "commit", "-q", "-m", f"rustd: {group}\n\nFrom rustd-collate {head}.")
            commits += 1
    return commits

main()

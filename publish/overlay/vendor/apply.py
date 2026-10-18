#!/usr/bin/env python3
"""Check out rust-analyzer at the pinned commit and apply rustd's patches to it.

    python3 vendor/apply.py            apply (the tree is reset to the pin first)
    python3 vendor/apply.py --check    exit 1 if the tree is not at pin + patches

The code generator lives in this repository at crates/hir-ty-codegen and is linked into
rust-analyzer's `hir-ty`, whose private items it uses.
"""
import glob, os, shutil, subprocess, sys

here = os.path.dirname(os.path.abspath(__file__))
tree = os.path.join(here, "rust-analyzer")
pin = open(os.path.join(here, "rust-analyzer.rev")).read().strip()
patches = sorted(glob.glob(os.path.join(here, "patches", "*.patch")))
codegen = os.path.join(here, "..", "crates", "hir-ty-codegen")
mir = os.path.join(tree, "crates", "hir-ty", "src", "mir")
stamp = os.path.join(tree, ".git-rustd-applied") if os.path.isdir(os.path.join(tree, ".git")) else os.path.join(here, ".applied")


def git(*args):
    subprocess.run(["git", "-C", tree, *args], check=True)


def wanted():
    import hashlib
    digest = hashlib.sha256(pin.encode())
    for patch in patches:
        digest.update(open(patch, "rb").read())
    return digest.hexdigest()


def link(name):
    source, target = os.path.join(codegen, name), os.path.join(mir, name)
    if os.path.lexists(target):
        shutil.rmtree(target) if os.path.isdir(target) and not os.path.islink(target) else os.remove(target)
    try:
        os.symlink(os.path.relpath(source, mir), target, target_is_directory=os.path.isdir(source))
    except OSError:
        shutil.copytree(source, target) if os.path.isdir(source) else shutil.copy2(source, target)


def main():
    current = open(stamp).read().strip() if os.path.exists(stamp) else ""
    if "--check" in sys.argv:
        sys.exit(0 if current == wanted() else 1)
    if not os.path.exists(os.path.join(tree, "Cargo.toml")):
        subprocess.run(["git", "-C", os.path.dirname(here), "submodule", "update", "--init", "vendor/rust-analyzer"], check=True)
    git("checkout", "-q", "-f", pin)
    git("clean", "-q", "-fd", "crates", "lib", "xtask")
    for patch in patches:
        git("apply", "--whitespace=nowarn", patch)
    link("codegen.rs")
    link("codegen")
    open(stamp, "w").write(wanted() + "\n")
    print(f"rust-analyzer at {pin[:10]} with {len(patches)} patches")


main()

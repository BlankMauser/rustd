"""linkline.py: a link line with rustd's objects ahead of what rustc built of the same crates.

Both define the same names, and rustd's have to be the ones that stay. An ELF linker keeps the
first definition when told to. Apple's linker has no flag for it, so there rustc's objects and
archives are replaced by copies whose definitions are weak (`rustd weaken`): a weak definition
gives way to one that is not, and to a weak one before it on the line.
"""
import os, subprocess, sys

MACHO = sys.platform == "darwin"
THEIRS = (".rlib", ".rcgu.o")


def first_of_rustc(link):
    """Where rustc's own objects start in a link line."""
    return next(i for i, a in enumerate(link) if a.endswith(THEIRS))


def ahead(link, objects, rustd):
    """`link` with `objects` before rustc's, and the linker told that the first definition stays."""
    link = list(link)
    first = first_of_rustc(link)
    if not MACHO:
        link[first:first] = [*objects, "-Wl,--allow-multiple-definition"]
        return link
    # The library of the toolchain defines nothing a workspace crate does.
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    theirs = [i for i, a in enumerate(link) if a.endswith(THEIRS) and not a.startswith(sysroot)]
    copies = subprocess.check_output([rustd, "weaken", *(link[i] for i in theirs)], text=True).splitlines()
    for i, copy in zip(theirs, copies):
        link[i] = copy
    link[first:first] = objects
    return link

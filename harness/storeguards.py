#!/usr/bin/env python3
"""storeguards.py: whose the store is, and what of a checkout is in it.

A daemon fills a store and ends. Then:

  - the directory has to be the user's alone (0700) and every file in it too (0600);
  - no file of the store, and no file of a remote the store was pushed to, may have the
    path of the checkout in it: what one checkout kept is found by another, and read by
    whoever reads the remote;
  - a pack that other users can write to has to be removed, said so, and the program
    built from the rest has to print what cargo's does;
  - a store directory that other users can write to has to be refused by `rustd cache`
    with the reason, and a daemon on it has to say it is not used, build the right
    program, and neither read nor write anything in it;
  - `rustd cache pull` from a remote whose lists nobody signed has to be refused, with
    nothing fetched, unless RUSTD_REMOTE_UNSIGNED=1; `rustd cache push` asks for nothing.

    python3 storeguards.py
"""
import os, shutil, stat, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from storelib import Checkout, Daemon, cache, files, scratch, verify

SEALS = {"RUSTD_CACHE_SEAL_AT": "1", "RUSTD_CACHE_MAX": "off"}
OTHERS_WRITE = "written to by other users"


def fail(what):
    raise SystemExit(f"FAIL: {what}")


def mode(path):
    return stat.S_IMODE(os.stat(path).st_mode)


def filled(checkout, store, name):
    """Has a daemon build the program with `store`; returns what the daemon logged."""
    daemon = Daemon(checkout, store, name=name, env=SEALS)
    checkout.check(daemon, name)
    daemon.kill()
    return daemon.log()


def the_users_alone(store):
    if mode(store) != 0o700:
        fail(f"the store is {mode(store):o}")
    open_to = [(name, mode(os.path.join(store, name))) for name in os.listdir(store)]
    if not open_to or any(it != 0o600 for _, it in open_to):
        fail(f"files of the store are not 0600: {open_to}")
    print(f"  modes: the store 0700, its {len(open_to)} files 0600")


def names_no_checkout(checkout, directory, what):
    """Nothing below `directory` has the checkout's place, or the home of the user, in it."""
    places = [checkout.work.encode(), os.path.expanduser("~").encode()]
    read = 0
    for at, _, names in os.walk(directory):
        for name in names:
            data = open(os.path.join(at, name), "rb").read()
            read += len(data)
            for place in places:
                if place in data:
                    fail(f"{what}: {name} has {place.decode()} in it")
    if read < 10_000:
        fail(f"{what}: only {read} bytes to look through")
    print(f"  paths: none of the checkout or the home in the {read} bytes of {what}")


def writable_pack(checkout, store):
    pack = files(store, ".pack")[0]
    os.chmod(pack, 0o666)
    whole, _, out = verify(store)
    if whole or OTHERS_WRITE not in out:
        fail(f"verify did not tell of the pack others can write to: {out}")
    log = filled(checkout, store, "writable-pack")
    if OTHERS_WRITE not in log or os.path.basename(pack) not in log:
        fail(f"the daemon did not say it removed the pack: {log[-400:]}")
    whole, _, out = verify(store)
    if not whole or any(mode(it) != 0o600 for it in files(store, ".pack")):
        fail(f"the store after the pack was removed: {out}")
    print("  a pack others can write to: told of by verify, removed by the daemon, the program right")


def writable_store(checkout, store):
    before = sorted(os.listdir(store))
    for open_to in (0o770, 0o707):
        os.chmod(store, open_to)
        for command in (["stats"], ["verify"], ["gc"]):
            status, out = cache(store, *command)
            if status == 0 or OTHERS_WRITE not in out:
                fail(f"cache {command[0]} on a {open_to:o} store: {out}")
        log = filled(checkout, store, f"writable-{open_to:o}")
        if "store: not used" not in log or OTHERS_WRITE not in log:
            fail(f"the daemon did not say the {open_to:o} store is not used: {log[-400:]}")
        if sorted(os.listdir(store)) != before:
            fail(f"the {open_to:o} store was changed: {sorted(os.listdir(store))}")
    os.chmod(store, 0o700)
    print("  a store others can write to: refused by cache stats, verify and gc; not used and "
          "not changed by the daemon, the program right")


def unsigned(store, work):
    remote, second = os.path.join(work, "remote"), os.path.join(work, "second")
    nothing_asked = {"RUSTD_REMOTE_UNSIGNED": ""}
    status, out = cache(store, "push", remote, env=nothing_asked)
    if status != 0:
        fail(f"push: {out}")
    names_no_checkout(CHECKOUT, remote, "the remote")
    for value in ("", "0", "true"):
        status, out = cache(second, "pull", remote, env={"RUSTD_REMOTE_UNSIGNED": value})
        if status == 0 or "RUSTD_REMOTE_UNSIGNED=1" not in out or files(second, ".pack"):
            fail(f"pull with RUSTD_REMOTE_UNSIGNED={value!r} was not refused: {out}")
    status, out = cache(second, "pull", remote, env={"RUSTD_REMOTE_UNSIGNED": "1"})
    whole, _, verified = verify(second)
    if status != 0 or not whole or len(files(second, ".pack")) != len(files(store, ".pack")):
        fail(f"pull with RUSTD_REMOTE_UNSIGNED=1: {out}\n{verified}")
    the_users_alone(second)
    print(f"  unsigned lists: push asks for nothing; pull refused unless RUSTD_REMOTE_UNSIGNED=1, "
          f"then {len(files(second, '.pack'))} packs fetched")


if os.name != "posix":
    raise SystemExit("storeguards.py: the modes of files are checked on Unix only")
WORK = scratch("rustd-storeguards-")
CHECKOUT = Checkout(WORK)
STORE = os.path.join(WORK, "store")
filled(CHECKOUT, STORE, "fill")
CHECKOUT.set(1)
filled(CHECKOUT, STORE, "fill")
the_users_alone(STORE)
names_no_checkout(CHECKOUT, STORE, "the store")
unsigned(STORE, WORK)
writable_pack(CHECKOUT, STORE)
writable_store(CHECKOUT, STORE)
shutil.rmtree(WORK, ignore_errors=True)
print("storeguards: all right")

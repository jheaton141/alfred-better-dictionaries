#!/usr/bin/env python3
"""Upgrade an already-installed Better Dictionaries workflow IN PLACE.

Importing a freshly built .alfredworkflow over an installed one drops the
per-dictionary keywords/hotkeys that BetterDict created, because they are
not part of the exported file. This script instead copies the new scripts
into the installed workflow folder and patches only the `lookup` script
filter in its info.plist, so every imported dictionary, keyword, hotkey and
search index stays exactly as it is. No re-import needed.

Usage (quit Alfred Preferences first; Alfred itself can stay running):
    python3 upgrade_in_place.py [--migrate [--keep-backup]] [workflow folder]

--migrate additionally converts already-imported dictionaries to the new,
much smaller storage layout (no re-import from the macOS dictionary files
needed). Search is unavailable while it runs; the old data is deleted only
after every dictionary has been rebuilt (--keep-backup keeps it).
"""

import glob
import os
import plistlib
import shutil
import stat
import subprocess
import sys

BUNDLE_ID = "mr.pennyworth.betterDicts"
HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPTS = ["lib.sh", "lookup.sh", "search.sh"]
CMD = 1048576


def candidate_dirs():
    roots = [
        os.path.expanduser(
            "~/Library/Application Support/Alfred/Alfred.alfredpreferences"
        )
    ]
    try:  # user may sync preferences elsewhere
        sync = subprocess.run(
            ["defaults", "read", "com.runningwithcrayons.Alfred", "syncfolder"],
            capture_output=True, text=True,
        ).stdout.strip()
        if sync:
            roots.insert(0, os.path.expanduser(sync) + "/Alfred.alfredpreferences")
    except OSError:
        pass
    for root in roots:
        yield from glob.glob(f"{root}/workflows/*/info.plist")


def find_installed():
    for p in candidate_dirs():
        try:
            with open(p, "rb") as f:
                if plistlib.load(f).get("bundleid") == BUNDLE_ID:
                    return os.path.dirname(p)
        except Exception:
            continue
    return None


BD_ARG = None
OLD_PY = '"$alfred_workflow_data/.venv/bin/python" pyapp/BetterDict.py'


def patch_plist(path, new_version):
    with open(path, "rb") as f:
        wf = plistlib.load(f)
    by_note = {u: d.get("note") for u, d in wf["uidata"].items()}
    uid_of = lambda note: next(u for u, n in by_note.items() if n == note)
    ipa, opener = uid_of("Play pronunciation audio"), uid_of(
        "Open definition in browser"
    )
    lookup = next(
        o for o in wf["objects"]
        if o["type"] == "alfred.workflow.input.scriptfilter"
        and o["config"].get("keyword") == "lookup"
    )
    c = lookup["config"]
    c["script"] = './lookup.sh "$1"'
    c["alfredfiltersresults"] = False
    c["alfredfiltersresultsmatchmode"] = 0
    c["subtext"] = "keyword: 'lookup' — pick a dictionary, then type a word"
    conn = lambda dst, mod: {
        "destinationuid": dst, "modifiers": mod,
        "modifiersubtext": "", "vitoclose": False,
    }
    wf["connections"][lookup["uid"]] = [conn(ipa, CMD), conn(opener, 0)]
    for o in wf["objects"]:
        c = o.get("config")
        if isinstance(c, dict) and OLD_PY in c.get("script", ""):
            c["script"] = c["script"].replace(OLD_PY, "./bd")
        sc = c.get("script", "") if isinstance(c, dict) else ""
        if "cocoaDialog" in sc and "confirm()" in sc:
            new = open(f"{HERE}/info.plist", "rb")
            ref = next(x for x in plistlib.load(new)["objects"] if x["uid"] == o["uid"])
            o["config"]["script"] = ref["config"]["script"]
    wf["version"] = new_version
    with open(path, "wb") as f:
        plistlib.dump(wf, f)


def main():
    global BD_ARG
    argv = sys.argv[1:]
    if "--bd" in argv:
        i = argv.index("--bd")
        BD_ARG = argv[i + 1]
        del argv[i:i + 2]
    args = [a for a in argv if not a.startswith("--")]
    migrate = "--migrate" in sys.argv
    keep_backup = "--keep-backup" in sys.argv
    dest = args[0] if args else find_installed()
    if not dest or not os.path.exists(f"{dest}/info.plist"):
        sys.exit("Couldn't find the installed workflow; pass its folder "
                 "(Alfred > Workflows > right-click > Open in Finder).")
    with open(f"{HERE}/info.plist", "rb") as f:
        new_version = plistlib.load(f)["version"]

    print(f"Upgrading {dest} -> {new_version}")
    for plist_name in ("info.plist", "info.plist.orig"):
        p = f"{dest}/{plist_name}"
        if os.path.exists(p):
            shutil.copy(p, f"{p}.pre-{new_version}")
            patch_plist(p, new_version)
            print(f"  patched {plist_name} (backup: {plist_name}.pre-{new_version})")
    for name in SCRIPTS:
        shutil.copy(f"{HERE}/{name}", f"{dest}/{name}")
        os.chmod(f"{dest}/{name}", os.stat(f"{dest}/{name}").st_mode | stat.S_IXUSR)
    bd_src = BD_ARG or next(
        (p for p in (f"{HERE}/bd/target/release/bd", f"{HERE}/bd-arm64", f"{HERE}/bd")
         if os.path.isfile(p)), None)
    if not bd_src:
        sys.exit("The Rust importer binary 'bd' is missing. Download it (see README: "
                 "'Getting bd') or pass --bd /path/to/bd.")
    shutil.copy(bd_src, f"{dest}/bd")
    os.chmod(f"{dest}/bd", 0o755)
    try:
        subprocess.call(["xattr", "-d", "com.apple.quarantine", f"{dest}/bd"],
                        stderr=subprocess.DEVNULL)
    except OSError:
        pass
    shutil.copy(f"{HERE}/setup.sh", f"{dest}/setup.sh")
    print(f"  installed bd from {bd_src}")
    subprocess.call(["pkill", "-f", "alfred-dict-server"])  # restart w/ new flags
    print("Done. Existing dictionaries, keywords and hotkeys are untouched.")

    if migrate:
        data = os.path.expanduser(
            f"~/Library/Application Support/Alfred/Workflow Data/{BUNDLE_ID}"
        )
        cmd = [f"{dest}/bd", "migrate", data]
        if keep_backup:
            cmd.insert(2, "--keep-backup")
        print("Migrating imported dictionaries (this can take a while)...")
        sys.exit(subprocess.call(cmd, cwd=dest))


if __name__ == "__main__":
    main()

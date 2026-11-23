# -*- coding: utf-8 -*-

import glob
import gzip
import json
import os
import re
import shutil
import sys
import time
import typing
from base64 import b16encode
from collections import defaultdict
from struct import unpack
from subprocess import *

import click
import meilisearch
from bs4 import BeautifulSoup

import alfred
import appledict
import plist
from ProgressBar import IndefiniteProgressBar
from ProgressBar import ProgressBar
from ProgressBar import run_parallely_with_progress_bar
from WorkflowGraph import WorkflowGraph

# Index-size knobs. The search index is the biggest consumer of disk and the
# main driver of cold-start latency, so keep what it stores to what search
# actually needs: Alfred shows a single subtitle line, and matches deep in a
# very long entry add little relevance but a lot of index.
SUBTITLE_MAX_CHARS = 300
FULLTEXT_MAX_CHARS = 4000

HOME = os.path.expanduser("~")
SEARCH_IP = os.environ.get("SEARCH_IP", "127.0.0.1")
SEARCH_PORT = os.environ.get("SEARCH_PORT", "6789")
WORKFLOW_DIR = alfred.get_workflow_dir()
WORKFLOW_ID = plist.read(f"{WORKFLOW_DIR}/info.plist")["bundleid"]
DEFAULT_WORKFLOW_DATA_DIR = alfred.default_workflow_data_dir(WORKFLOW_ID)


def noop():
    pass


def read_int(f: typing.BinaryIO) -> int:
    return unpack("i", f.read(4))[0]


def get_word_defs_map(dict_data_path):
    """returns a dict where key is the word
    and value is a list of its definitions."""
    word_to_defs_map = defaultdict(list)

    dict_body = appledict.DictBody(dict_data_path)
    for defn in dict_body.definitions():
        # Example XML defn opening tag:
        # <d:entry
        #   xmlns:d=".apple.com/DTDs/DictionaryService-1.0.rng"
        #   id="m_en_gbus0134270"
        #   d:title="apple"
        #   class="entry">
        # The principled way to find the word being defined
        # would be to parse the XML and then get 'd:title' from it,
        # however, that's too slow, so we resort to regex matching.
        word = re.search('d:title="(.*?)"', defn).group(1)
        word_to_defs_map[word].append(defn)
    return word_to_defs_map


def base16(input_str):
    return b16encode(input_str.encode("utf-8")).decode("utf-8")


def filename_defn_pairs(word, defns):
    """for <word> and its <n>th definition,
    we are going to create an HTML file whose name is
    base16(<word><n>).html and content is the definition

    we do this as we need some sort of 'safe' encoding as words might
    contain characters that can't be part of a filename. also, we don't
    want collisions on case-insensitive filesystems.

    returns list of (filename, definition)
    """
    return [
        (f'{base16(f"{word}{i}")}.html', defn) for i, defn in enumerate(defns)
    ]


def write_pack(word_defs_map_items, pack_path):
    """Write every definition into ONE file, each as its own gzip member,
    and return {filename: (offset, length)}.

    Individual HTML files cost a full 4KB filesystem block each plus
    directory overhead; one pack of compressed entries is a fraction of the
    size. Because every entry is an independent gzip member, lib.sh can pull
    out any single one with `tail | head | gunzip` -- no Python at search time.
    """
    placement = {}
    items = list(word_defs_map_items)
    pb = ProgressBar("Packing definitions...")
    with open(pack_path, "wb") as pack:
        for n, (word, defs) in enumerate(items):
            for filename, defn in filename_defn_pairs(word, defs):
                blob = gzip.compress(defn.encode("utf-8"), 9, mtime=0)
                placement[filename] = (pack.tell(), len(blob))
                pack.write(blob)
            if n % 500 == 0:
                pb.update(percent=max(((n + 1) * 100) / len(items), 0.01),
                          message=word)
    pb.finish()
    return placement


class AccumulatedIndexer:
    def __init__(self, index):
        self.items = []
        self.index = index

    def add(self, items):
        self.items.extend(items)

    def finish(self):
        task_info = self.index.add_documents(self.items)

        def status():
            return self.index.get_task(task_info.task_uid).status

        ipb = IndefiniteProgressBar(title="Waiting for index to be ready...")
        while (s := status()) != "succeeded":
            ipb.update(message="")
            time.sleep(2)
        ipb.finish()


def build_index(
    word_defs_map_items, dict_id, db_path, html_dir, placement, wait=False
):
    index = create_index(dict_id, db_path)
    title = "Building index for instant search..."

    def make_alfred_items(word, defs):
        return [
            make_alfred_item(
                word, filename, definition, html_dir, placement[filename]
            )
            for filename, definition in filename_defn_pairs(word, defs)
            if word != ""
        ]

    run_parallely_with_progress_bar(
        items=word_defs_map_items,
        func=lambda word_n_defs: make_alfred_items(*word_n_defs),
        msgfunc=lambda word_n_defs: word_n_defs[0],
        accumulator=AccumulatedIndexer(index),
        title=title,
        wait=wait,
    )


def make_alfred_item(word, filename, definition, html_dir, placement):
    soup = BeautifulSoup(definition, "lxml")
    fulltext = soup.get_text()
    snippet = fulltext
    snippet_div = soup.find(attrs={"d:def": "1"})
    if snippet_div is not None:
        snippet = snippet_div.get_text()
    snippet = snippet[:SUBTITLE_MAX_CHARS]
    fulltext = fulltext[:FULLTEXT_MAX_CHARS]

    # forms of a word typically include:
    # plural forms of a noun: (mouse, mice)
    # intensifiers for odjectives: (good, better, best)
    # past-tense variations of verbs: (fly, flew, flown)
    forms = ""
    forms_div = soup.find(attrs={"class": "infg"})
    if forms_div is not None:
        forms = forms_div.get_text()

    html_path = f"{html_dir}/{filename}"
    item = {
        "arg": html_path,
        "title": word,
        "id": filename.split(".")[0],
        "forms": forms,
        "subtitle": (forms + snippet)[:SUBTITLE_MAX_CHARS],
        "fulltext": fulltext,
        "quicklookurl": html_path,
        # location of the definition inside the dict's pack file; the HTML
        # file at html_path is materialized lazily by lib.sh at search time
        "off": placement[0],
        "len": placement[1],
    }

    ipa_div = soup.find(attrs={"d:prn": "IPA solitary"})
    if ipa_div is None:
        ipa_div = soup.find(attrs={"d:prn": "IPA"})
    if ipa_div is None:
        return item

    pronunciation = ipa_div.get_text().split(",")[0]

    item["subtitle"] = "[⌘: 🗣] " + item["subtitle"]
    item["mods"] = {
        "cmd": {"arg": pronunciation, "subtitle": "🗣 " + pronunciation}
    }

    return item


def dict_info(dict_path):
    return plist.read(f"{dict_path}/Contents/Info.plist")


def all_dict_paths():
    dict_paths = []
    dict_globs = [
        "/System/Library/AssetsV2/**/*.dictionary",
        "/Library/Dictionaries/**/*.dictionary",
        f"{HOME}/Library/Dictionaries/**/*.dictionary",
    ]
    for dict_glob in dict_globs:
        dict_paths.extend(glob.glob(dict_glob, recursive=True))
    return [
        dict_path
        for dict_path in dict_paths
        if any(
            map(
                os.path.exists,
                [
                    f"{dict_path}/Contents/Resources/Body.data",
                    # While all dictionaries that come bundled with macOS store Body.data
                    # in the Contents/Resources folder,
                    # https://agiletortoise.com/terminology/mac/ stores the Body.data
                    # directly in the Contents folder.
                    f"{dict_path}/Contents/Body.data",
                ],
            )
        )
    ]


def get_dict_id(info):
    # meilisearch index only allows alphanumeric characters, hyphens, and
    # underscores in the index name, so replace all other characters with
    # hyphens.
    return re.sub(r"[^a-zA-Z0-9_-]", "-", info["CFBundleIdentifier"])


def get_dict_name(info):
    return info.get(
        "CFBundleDisplayName",
        info.get(
            "CFBundleName",
            info["CFBundleIdentifier"],
        ),
    )


def create_workflow_objects(dict_name, dict_id):
    wf = WorkflowGraph(f"{WORKFLOW_DIR}/info.plist")

    # Functional:
    junction = wf.newJunction()
    hotkey = wf.newHotkey(note=dict_name)
    fallback_search = wf.newFallbackSearch(
        note=dict_name,
        title=f"search {dict_name}",
    )
    script_filter = wf.newBashScriptFilter(
        note=dict_name,
        title=f"search {dict_name}",
        script=(
            "open -g ./AlfredExtraPane.app\n\n"
            'query="$1"\n\n'
            f'./search.sh "$query" "{dict_id}"'
        ),
    )
    router = wf.getObjWithLabel("router")
    router_output = wf.addOutputToRouter(router, output=dict_id)
    ipa_player = wf.getObjWithLabel("Play pronunciation audio")
    html_opener = wf.getObjWithLabel("Open definition in browser")
    wf.connect(src=hotkey, dst=script_filter)
    wf.connect(src=fallback_search, dst=script_filter)
    wf.connect(src=junction, dst=script_filter)
    wf.connect(src=script_filter, dst=ipa_player, mod=wf.CMD)
    wf.connect(src=script_filter, dst=html_opener)
    wf.connect(src=router, dst=junction, src_out=router_output)

    # Presentational:
    dummy_junction = wf.getObjWithLabel("dummy junction")
    dummy_hotkey = wf.getObjWithLabel("dummy hotkey")
    dummy_fallback_search = wf.getObjWithLabel("dummy fallback search")
    dummy_script_filter = wf.getObjWithLabel("dummy script filter")

    row_count = wf.getOutputCount(router)
    VERT_OFFSET = 180 * (row_count - 1)
    wf.setX(junction, wf.x(dummy_junction))
    wf.setY(junction, wf.y(dummy_junction) + VERT_OFFSET)

    wf.setX(hotkey, wf.x(dummy_hotkey))
    wf.setY(hotkey, wf.y(dummy_hotkey) + VERT_OFFSET)

    wf.setX(fallback_search, wf.x(dummy_fallback_search))
    wf.setY(fallback_search, wf.y(dummy_fallback_search) + VERT_OFFSET)

    wf.setX(script_filter, wf.x(dummy_script_filter))
    wf.setY(script_filter, wf.y(dummy_script_filter) + VERT_OFFSET)

    wf.setY(html_opener, wf.y(script_filter))

    wf.save()


def import_dict(dict_path, import_base_dir):
    info = dict_info(dict_path)
    dict_id = get_dict_id(info)
    dict_name = get_dict_name(info)

    data_path = f"{dict_path}/Contents/Resources/Body.data"
    if not os.path.exists(data_path):
        # https://agiletortoise.com/terminology/mac/ stores the Body.data directly
        # in the Contents folder (and not in Contents/Resources folder).
        data_path = f"{dict_path}/Contents/Body.data"

    dest_dir = f"{import_base_dir}/{dict_id}"
    # HTML files are generated on demand into this cache (see lib.sh)
    dest_html_dir = f"{import_base_dir}/cache/{dict_id}"
    pack_path = f"{dest_dir}/defs.pack"
    db_path = f"{import_base_dir}/db"

    if not is_search_server_up():
        start_search_server(db_path)

    os.makedirs(dest_html_dir, exist_ok=True)
    os.makedirs(dest_dir, exist_ok=True)

    word_defs_map = get_word_defs_map(data_path)
    word_defs_map_items = word_defs_map.items()
    placement = write_pack(word_defs_map_items, pack_path)
    build_index(word_defs_map_items, dict_id, db_path, dest_html_dir, placement)

    imported = {"items": []}
    imported_json_path = f"{import_base_dir}/imported.json"

    if os.path.exists(imported_json_path):
        with open(imported_json_path) as f:
            imported = json.load(f)

    imported["items"].append({"title": dict_name, "arg": dict_id})

    create_workflow_objects(dict_name, dict_id)
    with open(imported_json_path, "w") as f:
        json.dump(imported, f, indent=2)


def is_search_server_up():
    return call(["pgrep", "alfred-dict-server"], stdout=DEVNULL) == 0


def start_search_server(db_path):
    with open(f"{db_path}.log", "wb") as logfile:
        cmd = [
            f"{WORKFLOW_DIR}/alfred-dict-server",
            "--db-path",
            db_path,
            "--http-addr",
            f"{SEARCH_IP}:{SEARCH_PORT}",
            "--http-payload-size-limit",
            "1000000000",
            "--no-analytics",
        ]
        Popen(cmd, stdout=logfile, stderr=logfile)


def search_client(db_path):
    if not is_search_server_up():
        start_search_server(db_path)
    while not is_search_server_up():
        time.sleep(0.1)
    client = meilisearch.Client(f"http://{SEARCH_IP}:{SEARCH_PORT}")
    while not client.is_healthy():
        time.sleep(0.1)
    return client


def create_index(dict_id, db_path):
    db = search_client(db_path)
    task_info = db.create_index(dict_id)
    db.wait_for_task(task_info.task_uid, timeout_in_ms=10000)
    index = db.get_index(dict_id)
    # "subtitle" is just a prefix/excerpt of "fulltext"; indexing both
    # doubles the index for no relevance gain.
    index.update_searchable_attributes(
        [
            "title",
            "forms",
            "fulltext",
        ]
    )
    index.update_ranking_rules(
        [
            "exactness",
            "attribute",
            "typo",
            "words",
            "proximity",
        ]
    )
    # Meilisearch >= 1.6: only record word positions per attribute rather than
    # per word pair. Considerably smaller index and faster indexing; the
    # trade-off is slightly coarser proximity ranking.
    index.update_settings({"proximityPrecision": "byAttribute"})
    index.update_displayed_attributes(
        [
            # everything except id, forms, and fulltext
            "off",
            "len",
            "arg",
            "mods",
            "title",
            "subtitle",
            "quicklookurl",
        ]
    )
    return index


def list_unimported_dicts(import_base_dir) -> list[alfred.Item]:
    alfreditems = []

    imported = []
    imported_json_path = f"{import_base_dir}/imported.json"

    if os.path.exists(imported_json_path):
        with open(imported_json_path) as f:
            imported = [i["title"] for i in json.load(f)["items"]]

    for dict_path in all_dict_paths():
        try:
            info = dict_info(dict_path)
            dict_name = get_dict_name(info)
            if dict_name in imported:
                continue
            alfreditems.append(alfred.Item(title=dict_name, arg=dict_path))
        except Exception as e:
            print(f'Unable to process "{dict_path}": {e}', file=sys.stderr)
    return alfreditems


def read_legacy_html_dir(html_dir):
    """word -> [definitions] from the old one-HTML-file-per-definition layout.

    Files are visited in name order so that, within a word, definitions keep
    the order they were imported in (filenames are base16(word + n)).
    """
    word_to_defs_map = defaultdict(list)
    for name in sorted(os.listdir(html_dir)):
        if not name.endswith(".html"):
            continue
        with open(f"{html_dir}/{name}", "rb") as f:
            text = f.read().decode("utf-8")
        m = re.search(r"<body>(.*)</body>", text, re.S)
        if m is None:
            continue
        defn = m.group(1).strip()
        title = re.search('d:title="(.*?)"', defn)
        if title is not None:
            word_to_defs_map[title.group(1)].append(defn)
    return word_to_defs_map


def kill_search_server(import_base_dir):
    call(["pkill", "-f", "alfred-dict-server"], stdout=DEVNULL)
    while is_search_server_up():
        time.sleep(0.2)
    pidfile = f"{import_base_dir}/server.pid"
    if os.path.exists(pidfile):
        os.remove(pidfile)


def migrate_all(import_base_dir, keep_backup):
    """Convert every imported dictionary from the old layout (one HTML file
    per definition, big index) to the pack + slim index layout, WITHOUT
    re-reading the macOS dictionary files.

    The new index goes into a fresh database ('db.new'): LMDB never gives
    freed space back to the filesystem, so deleting and recreating indexes
    inside the old 'db' would not make anything smaller. Nothing old is
    deleted until every dictionary has been rebuilt; if anything fails, the
    old data is untouched and migrate can simply be run again.
    """
    imported_json_path = f"{import_base_dir}/imported.json"
    if not os.path.exists(imported_json_path):
        print("Nothing imported; nothing to migrate.")
        return
    with open(imported_json_path) as f:
        dict_ids = [i["arg"] for i in json.load(f)["items"]]

    todo = [
        d
        for d in dict_ids
        if os.path.isdir(f"{import_base_dir}/{d}/html")
    ]
    if not todo:
        print("All dictionaries already use the new layout.")
        return
    print(f"Migrating: {', '.join(todo)}")

    db_path = f"{import_base_dir}/db"
    new_db_path = f"{import_base_dir}/db.new"

    kill_search_server(import_base_dir)
    shutil.rmtree(new_db_path, ignore_errors=True)
    start_search_server(new_db_path)

    for dict_id in todo:
        print(f"[{dict_id}] reading old HTML files...")
        word_defs_map = read_legacy_html_dir(
            f"{import_base_dir}/{dict_id}/html"
        )
        items = word_defs_map.items()
        cache_dir = f"{import_base_dir}/cache/{dict_id}"
        os.makedirs(cache_dir, exist_ok=True)
        pack_path = f"{import_base_dir}/{dict_id}/defs.pack"
        print(f"[{dict_id}] packing {len(word_defs_map)} words...")
        placement = write_pack(items, pack_path)
        print(f"[{dict_id}] building index...")
        build_index(items, dict_id, new_db_path, cache_dir, placement, wait=True)

    # Everything rebuilt: swap databases, then reclaim the old data.
    kill_search_server(import_base_dir)
    old_db_path = f"{import_base_dir}/db.premigrate"
    shutil.rmtree(old_db_path, ignore_errors=True)
    os.rename(db_path, old_db_path)
    os.rename(new_db_path, db_path)
    if os.path.exists(f"{new_db_path}.log"):
        os.replace(f"{new_db_path}.log", f"{db_path}.log")
    if not keep_backup:
        shutil.rmtree(old_db_path, ignore_errors=True)
        for dict_id in todo:
            shutil.rmtree(f"{import_base_dir}/{dict_id}/html")
    print("Migration complete.")


@click.group()
def main():
    pass


@main.command()
@click.argument(
    "workflow_data_dir",
    type=click.Path(exists=True, file_okay=False, resolve_path=True),
    envvar="alfred_workflow_data",
    default=DEFAULT_WORKFLOW_DATA_DIR,
)
def list_unimported(workflow_data_dir: str):
    unimported = list_unimported_dicts(workflow_data_dir)
    print(json.dumps({"items": [d.as_dict() for d in unimported]}, indent=2))


@main.command(name="import")
@click.argument(
    "dict_path",
    type=click.Path(exists=True, file_okay=False, resolve_path=True),
)
@click.argument(
    "workflow_data_dir",
    type=click.Path(exists=True, file_okay=False, resolve_path=True),
    envvar="alfred_workflow_data",
    default=DEFAULT_WORKFLOW_DATA_DIR,
)
def import_(dict_path: str, workflow_data_dir: str):
    import_dict(dict_path, workflow_data_dir)


@main.command()
@click.option(
    "--keep-backup",
    is_flag=True,
    help="Keep the old database and HTML files (needs extra disk space "
    "until you delete them yourself).",
)
@click.argument(
    "workflow_data_dir",
    type=click.Path(exists=True, file_okay=False, resolve_path=True),
    envvar="alfred_workflow_data",
    default=DEFAULT_WORKFLOW_DATA_DIR,
)
def migrate(workflow_data_dir: str, keep_backup: bool):
    migrate_all(workflow_data_dir, keep_backup)


@main.command()
@click.argument(
    "workflow_data_dir",
    type=click.Path(exists=True, file_okay=False, resolve_path=True),
    envvar="alfred_workflow_data",
    default=DEFAULT_WORKFLOW_DATA_DIR,
)
def factory_reset(workflow_data_dir: str):
    shutil.rmtree(workflow_data_dir)
    shutil.copy(f"{WORKFLOW_DIR}/info.plist", f"{WORKFLOW_DIR}/info.plist.bkp")
    shutil.copy(f"{WORKFLOW_DIR}/info.plist.orig", f"{WORKFLOW_DIR}/info.plist")
    os.mkdir(workflow_data_dir)


if __name__ == "__main__":
    main()

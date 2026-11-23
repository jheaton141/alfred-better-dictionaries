<h1 align="center">
  
<a href="https://github.com/mr-pennyworth/alfred-better-dictionaries/releases/latest/">
  <img src="icon.png" width="16%"><br/>
  <img alt="Download"
       src="https://img.shields.io/github/downloads/mr-pennyworth/alfred-better-dictionaries/total?color=purple&label=Download"><br/>
</a>
  Better Dictionaries
</h1>
Better search and live previews for built-in macOS dictionaries.

### Features
 - IPA (phonetic) pronunciations:
   Press `⌘↩` to hear the pronunciation.
 - In-Alfred live previews with colors that
   automatically adapt to Alfred's theme:
   ![](images/auto-theme.png)

 - Reverse search:
   ![](images/reverse-search.png)

 - More relevant search results:  
   left: macOS/Alfred built-in search, right: BetterDict
   ![](images/built-in-vs-workflow.png)

 - If one word has multiple, unrelated meanings with different
   origin, they are showed as diffrent entries
   (in the above example, see "arm" has two entries at the top) 

 - Dictionary and thesaurus set up automatically; add any compatible
   third-party dictionary


### Rebuilt in Rust
This fork replaces the original Python + Meilisearch backend with `bd`, a
single native Rust binary (v0.8.2, source in `bd/`). The workflow's look and
features are unchanged; what changed is everything underneath.

Measured head-to-head against the original
([6829ce8](https://github.com/mr-pennyworth/alfred-better-dictionaries),
Python 3.14 + Meilisearch 1.6.1) on the New Oxford American Dictionary
(111,579 entries), same Apple Silicon Mac:

| | Original | Rust `bd` | |
|---|---|---|---|
| Import time | 47.3 s | 3.5–3.7 s | ~13× faster |
| Disk per dictionary | 929 MB | 103 MB | ~9× smaller |
| Peak memory during import | 1.13 GB | 0.43 GB | ~2.6× less |
| Warm lookup | 40–46 ms | 4–15 ms | ~5–8× faster |
| First lookup after idle | 56–198 ms (first call could return nothing) | 16 ms | |
| Background processes | permanent Meilisearch server | none | |
| Runtime dependencies | Python, venv, `uv`, Meilisearch, first-run download | none | |

**Storage.** The original stored every entry twice: a 449 MB search database
plus 111,606 pre-rendered HTML files (480 MB). `bd` keeps only the compressed
source definitions in one pack file (`defs.pack`, 79 MB) and a compact
memory-mapped index (`index.bin`, 23 MB). The HTML shown in Alfred's preview
is generated on demand for the handful of results on screen and cached. That
is 929 MB down to 103 MB, and the 480 MB of HTML becomes the 79 MB pack, about
6× smaller, with nothing pre-rendered.

**Ranking.** Results are tiered: exact title, inflected form (`ran` finds
*run*), title prefix, title words, typo-tolerant match, then the words of the
definition. For multi-word queries, entries whose title is the query or part
of it come first, and the fallback weights rare words above common ones and
ignores function words (`run away` returns *run*, `kick the bucket` returns
*croak* and *expire*). Output is deterministic.

**Dependencies.** `bd` builds from a handful of well-known crates, and CI runs
`cargo deny` (advisories, licenses, sources) on every push, so an unmaintained
or vulnerable dependency fails the build. Search results and the generated
HTML were verified byte-identical across the last dependency upgrade.

**Caveats.** Timings are from one machine; the original's import time is a
single run (it hung twice on Python 3.14 until a fork-safety workaround was
set), and the "first lookup" figures had a warm OS page cache. Natural-language
questions ("where do babies come from") still return only loose matches.

Credit: the workflow design, UI and idea are
[mr-pennyworth's](https://github.com/mr-pennyworth/alfred-better-dictionaries).

### Getting started
Import the workflow and type `lookup` followed by a word. The first time,
the workflow sets itself up: it imports macOS's built-in dictionary and
thesaurus in the background (a few seconds) and searches them together as
one. Nothing to choose or import. Each result is tagged with the book it
came from, and `⌘↩` plays the pronunciation.

To search only one kind of book, type `lookup` first, then a prefix before
the word. The prefix works only after `lookup`; typing `dict:` or `t:` on its
own in Alfred does not start a search.

| Type | Searches |
|---|---|
| `lookup happy` | dictionary and thesaurus together |
| `lookup d: happy` | dictionaries only (also `dict:` or `dictionary:`) |
| `lookup t: happy` | thesauri only (also `thes:` or `thesaurus:`) |

Third-party dictionaries count as dictionaries.

### Adding a third-party dictionary
Type `.dict-import` and pick a dictionary installed on your Mac (for
example one added in Dictionary.app). It is added to the same `lookup`
search, and also gets its own keyword and hotkey, which you can assign in
the workflow editor to search only that dictionary.

`.dict-factory-reset` removes everything imported; the next `lookup` sets
the built-in dictionary and thesaurus up again.


### Notes and Warnings
 - Everything is done by `bd`, a single native Rust binary (source in
   `bd/`): importing, the search index and the lookups themselves. No
   Python, no virtual environment, no search server, no download.
   The workflow is just `bd`, a few tiny shell scripts and the extra pane.
   Apple Silicon only.

 - Searches take a few milliseconds: the index is a single file per
   dictionary that is memory-mapped on demand. Results are ranked: exact
   title, then inflected forms ("ran" finds *run*), then title prefixes
   and words, then typos, then the words of the definition itself.

 - Works with Alfred 5 and 6.

 - The first-run setup needs the dictionaries to be downloaded in
   Dictionary.app (Settings). If they are missing, `lookup` says so.

 - Definitions are stored in one compressed pack file per dictionary
   (`<dict>/defs.pack`) next to its search index (`<dict>/index.bin`); the
   HTML that Alfred previews is generated on demand for the few results
   shown and cached under `cache/` (pruned after a week of disuse).

### Building and upgrading without re-importing
You need only Rust (`rustup`). `./build.sh` makes
`Better Dictionaries.arm64.alfredworkflow`.

Re-importing a newer `.alfredworkflow` over an installed one loses the
per-dictionary keywords/hotkeys. To upgrade in place instead, quit Alfred
Preferences and run `./upgrade.sh` from a checkout of this repo (pass the
workflow folder if it isn't found automatically). Your imported
dictionaries, keywords and hotkeys are kept, and no re-import from the
macOS dictionary files is needed.

It builds `bd`, patches the installed workflow's scripts, builds the new
search index from your existing dictionaries, then deletes the old
Meilisearch server and database and any leftover Python/`uv` files
(`--keep-backup` keeps the database; `--no-migrate` lets the first `lookup`
do the conversion in the background).

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

 - Import any compatible dictionary


### Rebuilt in Rust
This fork replaces the original Python + Meilisearch backend with `bd`, a
single native Rust binary (v0.6.0, source in `bd/`). The workflow's look and
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

**Caveats.** Timings are from one machine; the original's import time is a
single run (it hung twice on Python 3.14 until a fork-safety workaround was
set), and the "first lookup" figures had a warm OS page cache. Natural-language
questions ("where do babies come from") still return only loose matches.

Credit: the workflow design, UI and idea are
[mr-pennyworth's](https://github.com/mr-pennyworth/alfred-better-dictionaries).

### Importing a Dictionary
After importing the workflow, type `.dict-import` into Alfred.
![](images/import-any.png)
Select the dictionary you want to import.  


### Dictionary-specific Keywords and Hotkeys
After a dictionary is imported, a script filter and a hotkey trigger
is automatically created into the workflow editor.  

 - Freshly-installed workflow without any imported dictionaries:
   ![](images/before-import.png)

 - Two hotkeys and keyword triggers each automatically added after importing
   two dictionaries. They come pre-labeled with dictionary names:
   ![](images/after-import.png)

 - This allows you to assign hotkeys and keywords for specific dictionaries.
   For example, below you can see how I have manually assigned keywords
   `thesaurus` and `defn` to the thesaurus and dictionary respectively.
   In addition, I can trigger the dictionary search using `⌃⌥⌘D`.  
   ![](images/example-assignment.png)


### Word Lookup
You can use the hotkeys/keywords created above for directly searhcing
specific dictionaries. That's the recommended way for dictionaries
that you use frequently.

For the infrequently used dictionaries for which you haven't assigned
any hotkeys/keywords, follow this:
 1. Type `lookup` into Alfred. You'll see a list of all dictionaries
    imported into BetterDict.
    ![](images/lookup-imported.png)
 2. Select the dictionary (or press `⇥` to autocomplete it), and keep
    typing the search query in the same Alfred window, e.g.
    `lookup Oxford arm`.


### Notes and Warnings
 - Everything is done by `bd`, a single native Rust binary (source in
   `bd/`): importing, the search index and the lookups themselves. No
   Python, no virtual environment, no search server, no first-run download.
   The workflow is just `bd`, a few tiny shell scripts and the extra pane.
   Apple Silicon only.

 - Importing a dictionary takes a minute or two on Apple Silicon.

 - Searches take a few milliseconds: the index is a single file per
   dictionary that is memory-mapped on demand. Results are ranked: exact
   title, then inflected forms ("ran" finds *run*), then title prefixes
   and words, then typos, then the words of the definition itself.

 - Works with Alfred 5 and 6.

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

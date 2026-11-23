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

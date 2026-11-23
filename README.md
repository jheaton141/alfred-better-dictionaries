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
 - On first run after importing/upgrading the workflow, BetterDict bootstraps
   a local Python environment in Alfred Workflow Data. This can take a bit
   longer than normal startup.

 - Importing a dictionary could take as much as 30 minutes
   on old machines or if there's significant CPU activity from other apps.

 - After each mac restart, for the first time when you run
   the workflow, expect a comparatively slower search.
   Subsequent searches should be instant. The search server is started
   (and its index pre-warmed) the moment you type `lookup` or use any
   dictionary keyword, so the first real search is usually already warm.

 - Works with Alfred 5 and 6. Dictionaries imported with an older version
   keep working, but re-import them (`.dict-factory-reset`) to get the
   smaller, faster index described below.

 - Definitions are stored in one compressed pack file per dictionary
   (`<dict>/defs.pack`); the HTML that Alfred previews is generated on
   demand for the few results shown and cached under `cache/` (pruned after
   a week of disuse). Together with the slimmer search index this is much
   smaller than the old one-HTML-file-per-definition layout. Re-import
   existing dictionaries (`.dict-factory-reset`) to benefit.

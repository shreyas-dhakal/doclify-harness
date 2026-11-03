# doclify-search

The viewer searches Markdown/MDX in `docs/doclify` and the latest version of each portable record. Each query rebuilds a SQLite cache at `.doclify/local/search.sqlite3` from those authoritative files, then atomically replaces the previous cache. This favors freshness and recovery over incremental indexing; deleting or corrupting the cache is safe, and external edits become visible on the next query. The cache may contain document and record text, is local to the repository, and belongs in the ignored `.doclify/local/` directory. Raw captures and queues are not indexed.

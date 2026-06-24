# TODOs

- **`Mutex<Connection>` serializes all DB access across every task.** `commands::Db` wraps a
  single `rusqlite::Connection` behind one `Mutex`, so two unrelated tasks sending messages at
  the same time still queue behind each other for every read/write (the lock is held only
  briefly per query, not across the agent `.await`, but it's still one global lock for the
  whole app). Fine at Step 1's scale (one user, a handful of tasks). Revisit once **ROADMAP
  Step 5** (scaling) is in view — likely a connection pool (e.g. `r2d2`/`rusqlite_pool`) or
  WAL-mode SQLite with multiple reader connections, rather than a single shared lock.

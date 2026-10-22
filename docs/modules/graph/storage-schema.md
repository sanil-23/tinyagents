# Graph and tasks storage schema v2

`tinyagents_graph::storage_schema()` (owner `graph`) and
`tinyagents_tasks::storage_schema()` (owner `tasks`), both scope class Agent.

## Graph

One checkpointer. The old collection prefix becomes the `ns` body field, part of
every id and index.

| v1 | v2 |
|---|---|
| `graph_*`, `delegation_graph_*`, `flows_graph_*` `{checkpoints,threads,writes}` | `graph.{checkpoints,threads,writes}`, `ns` = `graph`, `delegation`, `flows` |
| checkpoint doc `{thread, namespace, seq, checkpoint_id, record}` | `{ns, thread, subgraph, seq, checkpoint_id, checkpoint}` under `Key::of([ns, thread, seq])` |
| `<prefix>_leases` `{owner, expires_at_ms}` | `tsd.leases`, owner `graph`, key `Key::of([ns, thread])` (epoch fence) |
| `graph_task_cache` `{graph, node, hash, value, expiresAtMs}` | `tsd.kv` owner `graph`, ns `task_cache:<graph>`, `expires_ms` metadata |
| goals, todos and the scratchpad (harness `Store`) | `tsd.kv` through the host's store, unchanged in code |

A collection gets one step per legacy prefix and a step chain cannot fork, so
the steps chain `1 -> 2 -> 3 -> 4` (declared version 4). A migrated row carries
the version its step ends at (`graph` 2, `delegation` 3, `flows` 4); the body
shape is the same.

`tsd.leases` and `tsd.kv` belong to `tsd`, so the host calls
`import_legacy_leases(storage)` and `import_legacy_task_cache(storage)` once per
scope after `Registry::migrate`. Both record completion in `tsd.migrations`.

Breaking: `DriverCheckpointer::with_prefix` is now `with_namespace`, and
`DriverTaskCache::with_collection` is `with_namespace`. A task cache needs a
driver with record metadata when a TTL is used.

## Tasks

| v1 | v2 |
|---|---|
| `orchestration_tasks` | `tasks.records`, id `Key::of([task_id])` |
| `orchestration_task_history` | `tasks.history`, id `Key::of([task_id, rev])`, unique index `task_rev` |
| `orchestration_completions` | `tasks.completions`, id `Key::of([task_id])` |

Bodies are bare snake_case DTOs with `*_ms` times; `OrchestrationTaskRecord` and
`CompletionRecord` (tool and RPC output types) are unchanged. The last-update
time is the document's `updated_ms` metadata; a migrated record's is the
migration time. Transitions use `Repo::update`. The history snapshot is written
after the record commit, not in the same batch.

`DriverTaskStore::with_prefix`, `collection_specs(prefix)` and
`DriverCompletionStore::with_collection` are removed: the names are fixed.

# Memory V2

Yunspire keeps Obsidian Markdown as the canonical knowledge source. SQLite stores rebuildable indexes, durable jobs, delivery receipts, and derived memories; it does not replace the Vault.

## Memory tracks

Memory V2 separates derived records into four tracks:

- `user_episode`: a bounded event or completed interaction worth recalling.
- `user_profile`: an evidenced, revisable user preference or constraint.
- `agent_case`: an execution case with an observed outcome.
- `agent_skill`: an approved routing or workflow improvement.

Every record carries `sourceDocId`, an optional source path and SHA-256 hash, minimal evidence, confidence, a monotonically increasing version, optional `supersedesId`, expiry, and one of `draft`, `active`, `superseded`, or `tombstone`.

## Scope isolation

Recall requires an exact five-dimensional scope match:

```text
userId + agentId + appId + projectId + sessionId
```

Callers must explicitly use `global` for a global project or session. An omitted dimension never broadens a query into another user, agent, application, project, or session.

## Reflection lifecycle

Background reflection is a native durable job:

```text
queued -> running -> awaiting_review -> completed
                     |                 |
                     +-> revise -------+
                     +-> reject
```

The model can only create a `draft` memory proposal. A proposal is excluded from recall until the user approves it. Rejected or replaced proposals become tombstones, and interrupted running jobs return to `queued` at startup.

## Search

Vault search combines a Chinese-friendly CJK lexical index with a deterministic local feature vector derived from characters, terms, titles, paths, tags, and Wiki Links. Standard RRF fuses both ranks and exposes their contributions; missing or corrupt vectors fall back to FTS. This is local feature similarity, not a neural embedding. Results always return the Vault ID and relative Markdown path so the UI can reopen the canonical source.

Memory recall uses the same strict scope boundary and excludes drafts, expired records, superseded records, and tombstones.

## First-party boundary

Memory V2 is a Yunspire first-party implementation. The production backend remains local SQLite, Obsidian Markdown remains the canonical knowledge source, and no external memory service, vector database, compatibility protocol, or automatic network synchronization is required or bundled.

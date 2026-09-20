# Performance plan

Written 2026-09-20 from measurements on the real account, release build, home
broadband, then revised after a review that read the plan against the code.
Ordered by measured impact over effort.

## What it costs today

| Operation | Measured | What that means at scale |
|---|---|---|
| One API round trip | ~380 ms average (10 calls in a forced pass: 3.8 s) | latency, not bandwidth, is the unit of cost |
| Listing one folder | `children` pages, then one `links` call per 150 entries, all sequential | 500 small folders ≈ 1000 calls ≈ 6–7 min; one 5 000-file folder ≈ 35 calls ≈ 13 s |
| Node key unlock (S2K) | 158 µs; ~430 µs per listed node with its name and passphrase decrypts | 10 000 nodes ≈ 4 s CPU — real, *not* the bottleneck |
| Download, 20 MiB (5 blocks) | 5.5 s ≈ 3.8 MB/s, one block at a time | each block: fetch ≈ 1 s then decrypt, nothing overlapped |
| Upload, 20 MiB (5 blocks) | 18.7 s ≈ 1.1 MB/s | one `blocks` prepare call (~350 ms) **per block** plus a sequential PUT each |
| Every CLI invocation | `users` + `addresses` + `my-files` ≈ 1.5 s before any work | the daemon pays it once; `ls`/`get`/`put` pay it every time |
| Daemon idle tick, every 30 s | one events call plus a full recursive `read_dir` of the sync folder | fine at 1k files, wasteful at 100k; a local edit waits up to 30 s |

Two assumptions this overturned: the S2K unlock is cheap with the pure-Rust
backend (the reference port warned of "tens of milliseconds"; it is 0.16 ms),
so caching keys is only worth it where something else needs them; and upload is
3.4× slower than download for structural reasons, not bandwidth.

Two gaps that are not speed but that parallelism will expose: `api.rs` has **no
retry or backoff at all**, and the daemon posts a desktop notification on
**every** failed pass, so an outage means one notification every 30 s.

## Plan, in order

### 1. Parallel block downloads   (small)
`drive::download` fetches block *i*, decrypts, writes, then fetches *i+1*.
Change to `futures::stream::buffered(N)` over the block list: N fetches in
flight, results yielded in index order (that is `buffered`, not
`buffer_unordered`), so the file is still written sequentially and memory is
bounded at N × 4 MiB. `fetch_block` already takes `&self`. Needs `futures-util`
added as a direct dependency; it is already compiled in the tree. N = 6 to
start. An expired block URL (401/403/404 from storage) is handled by refetching
the revision's block list once.
**Expected:** 20 MiB from 5.5 s to bandwidth-bound, ~1.5–2 s here.

### 2. Faster uploads   (medium)
In `drive::upload`, work in batches of 16 blocks: encrypt and sign the batch
sequentially (the manifest is extended in that loop, so its order is safe by
construction), send **one** `blocks` prepare call for the whole `BlockList`,
then PUT the batch concurrently. Match returned upload links to blocks by their
`Index` field, falling back to position only when it is absent. Memory: 16
ciphertexts plus one plaintext buffer, ~68 MiB. The draft-revision lifecycle is
unchanged: a batch that fails midway leaves the draft as today, and the retry
displaces it through `ClientUID`; the failing window just gets shorter.
**Expected:** 20 MiB from 18.7 s to ~3–4 s.

### 3. Concurrent-safe `Api` with retries   (medium; enables 4, 6, 7)
`get`/`post`/`put`/`delete` take `&mut self` only because token refresh
rewrites the session. Put the session behind `Arc<RwLock<Session>>` so every
method takes `&self` and `Api` is `Clone`. The refresh must do more than
serialise: Proton **rotates the refresh token**, so when several concurrent
requests all get 401, only the first may refresh. Under the lock, compare the
access token the failed request used with the current one; if they differ,
someone already refreshed, so just retry with the new token. Without this rule
the second refresh burns a dead token and logs the user out.

Retries, in the same change, but only where safe: GET and PUT calls and storage
fetches retry on 429 (honouring `Retry-After`), 5xx and connection errors, with
1 s, 2 s, 4 s backoff. POSTs that create things (`files`, `revisions`,
`folders`, `trash_multiple`) are not idempotent and are **not** retried; a
block upload that fails re-requests a fresh upload target instead of re-posting
to the old one. The daemon gets exponential backoff across consecutive failed
passes and notifies once per outage, not once per tick.

Honest note: nothing tests `call` or `refresh` today. This item adds a test
against a local fake HTTP server that answers 401 once and 200 after, run with
several concurrent requests, so the refresh rule is proven rather than assumed.
Persisting rotated tokens touches three call sites (`account::persist`, the
daemon's persist closure, and the two `session.uid` reads in `drive.rs`); all
become a read through the lock.

### 4. Parallel listing   (medium; needs 3)
Two halves. Inside `list()`, fetch the 150-entry `links` chunks concurrently,
which is what helps a single large folder. In the walk, list all folders at one
depth concurrently (bounded, 8 in flight) before descending; everything else
per child (rename, trash decision, download, bookkeeping) stays sequential as
it must. This speeds up the first sync, a `--force`, and the `Refresh` fallback
in item 6. It is **not** the steady-state win; item 6 is.
**Expected:** a 500-folder first sync from ~6 min to well under a minute; a
5 000-file folder from ~13 s to ~3 s.

### 5. Local changes by inotify   (medium; independent)
Replace the 30-second sweep with the `notify` crate, so a local edit starts a
pass within a couple of seconds and an idle daemon does no filesystem work.
The details are where this goes wrong, so: ignore `.kpdrive-part` names,
suppress triggers while a pass is writing (our own downloads, renames and
`set_modified` all fire events), and debounce for ~2 s so an editor's
write-temp-then-rename or LibreOffice's lock file produces one pass, not three.
Even so, a fast trigger makes "upload a half-written file, then upload it
again" likelier than the 30 s poll did, which costs one extra revision per
save on some editors; that is acceptable, and noted. The sweep stays as an
hourly safety net for what inotify misses (network mounts, watch limits). The
remote side keeps its 30 s events poll; nothing pushes from Proton.

### 6. Incremental sync from events   (large; needs 3, uses 4 as fallback)
The events endpoint tells us *which* links changed and how, and each event
carries the link's `ParentLinkID`; today that is reduced to a boolean and a
full walk. Process the events instead: fetch details for the listed links,
decrypt each with its parent's key, and apply only those changes. Resolving a
parent needs its key, so the daemon's `Drive`, which lives for the daemon's
lifetime, keeps an in-memory `link id → (parent id, unlocked key)` map filled
by every walk and every event; a parent not in the map is resolved by walking
`ParentLinkID` upward and cached. Rules: a trashed or deleted folder removes
its local subtree by path prefix; the cursor advances only after the whole
batch applied, and applying is idempotent so a retried batch is harmless;
`Refresh: true` means the cursor is stale and falls back to the item-4 walk.
The local-to-remote scan still runs each pass (no API cost). This is also what
Proton's guidelines ask for: sync from events, do not traverse.
**Expected:** steady-state passes cost the events call plus one `links` call
per batch of changes, regardless of tree size.

### 7. Faster CLI startup   (small; needs 3)
`users`, `addresses` and `my-files` do not depend on each other; fetch them
concurrently. 1.5 s → ~0.7 s on every `ls`, `get`, `put`, `share`.

## Not doing

- **A resumable-upload scheme** (drafts surviving a crash): worth doing, but
  it is robustness rather than speed.
- **Polling events faster than 30 s**: Proton's guidelines say not to, and it
  is already the cheapest call we make.
- **Coalescing "Sync now" clicks**: the tray's command channel is unbounded,
  so several clicks queue several forced passes. Two-line fix, folded into 3.

## How each step is judged

A `bench/` script seeds a reproducible tree into `kpdrive-test/` (20 folders
× 10 small files, one folder of 400 files, plus one 20 MiB file), then times a
forced pass, an upload and a download, and tears the tree down. Every item
states the number it should move; a step that does not move its number is
reverted. The test suite and a byte-for-byte compare of the 20 MiB round trip
guard correctness, and item 3's fake-server test guards the refresh rule.

Delivery order: 1, 2, 3, 4, each a commit and each measured; then 5; then 6;
7 as soon as 3 is in.

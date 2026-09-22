# Photos: how they should be handled

Proton Photos is a second library, on a volume of its own, and today kpdrive
only reads it. The configuration names two halves that do not exist yet,
`photos_ingestion_folder` and `photos_ingestion_perm_rm`, which is the gap this
plan is about: putting photos **into** Proton Photos from the desktop, and what
happens to the local file afterwards.

The second half of this document is the list of things the download side gets
wrong today.

Claims here are marked **verified** where they were checked against the code or
a live account, and **unsettled** where they are assumptions. An earlier draft
of this plan stated several of the unsettled ones as fact; a review caught it,
and the difference matters, because two of them decide whether the feature
works at all.

## What happens today

One pass, from `kpdrive photos` or from the daemon every half hour when
`photos_sync` is on:

1. Ask for the photos share; no share means the account has no library.
2. Page the whole timeline, 500 ids per page, until a short page. The anchor is
   the last link id of the previous page, not a time cursor.
3. Sort by capture time, drop duplicate ids.
4. Anything with no recorded entry, or whose recorded file is missing, is
   fetched in chunks of 150, one download at a time.
5. Each lands in `<photos_sync_folder>/YYYY/MM/<name>`, its modified time set
   to the capture time, ` (2)` appended when that name is taken.

Three details that are easy to miss and that the rest of this plan depends on:

- **The months are UTC.** Filing uses `civil_utc`, so a photo taken at half past
  midnight at UTC+1 lands in the previous month, disagreeing with the phone and
  the web app.
- **A missing capture time files under 1970/01**, with an epoch modified time,
  silently: `CaptureTime` is `#[serde(default)]`, and the fallback to the
  node's modify time only applies to ids absent from the timeline map.
- **A failed download leaves its part file behind** for good, and the part name
  replaces the extension rather than extending it, so `IMG_1.jpg` and
  `IMG_1.png` in one month share a single temp path.

## Ingestion: uploading a photo

Why ingestion and not editing the timeline: the README's design notes settle
that. Proton Photos decides where a photo sits in the timeline, and editing a
photo's metadata can move it, so the only honest operation from a desktop
client is adding a photo and letting the library place it.

The shape of it: a folder the user drops photos into, or points at their
phone's import directory. Anything new there goes up into Proton Photos, and
the local file is then trashed or deleted according to
`photos_ingestion_perm_rm`.

### 0. A local ingest record, before anything else
Whatever Proton does about duplicates, the failure that matters is local: the
upload succeeds, the process dies before the file is trashed, and the next pass
uploads it again. A small record of what has been ingested — path, size,
modified time, resulting link id, the shape `sync::Entry` already uses — makes
a pass idempotent on its own, and turns the worst case from "the library
doubled" into "one photo went up twice".

This comes first. Nothing else in ingestion is safe without it.

### A capture time
Proton files a photo by when it was taken. EXIF `DateTimeOriginal` is **local
wall-clock with no offset**, not an instant, and treating it as one is wrong by
up to fourteen hours, which moves photos across day and month boundaries.

**Take:** read `OffsetTimeOriginal`, falling back to `OffsetTime`, and use it.
With no offset recorded, interpret the wall-clock in the machine's local zone,
which is the best guess available and the same one every photo tool makes. With
no EXIF at all, the file's modified time. `kamadak-exif` (pure Rust, 0.6) reads
all three.

While here: give the download side the same care. A `CaptureTime` of zero
should fall back to the node's modify time rather than filing under 1970, and
`YYYY/MM` being UTC-derived should be written down, or changed to local time to
match what the user sees on their phone.

### A thumbnail   (unsettled, and load-bearing)
Whether the API **requires** a thumbnail or merely shows a blank tile without
one is not known, and the answer decides the shape of the feature.

**Settle first,** by uploading one without a thumbnail, or by reading an
official client's photo upload. Then:

- If cosmetic: upload everything, thumbnail what can be decoded, and leave the
  rest without one.
- If required: photos that cannot be decoded cannot be ingested, and that has
  to be said in the README and next to `photos_ingestion_folder` in the config,
  not only in a log line. For a folder pointed at a phone, HEIC and video are
  most of the content, and a user who is told nothing will believe it was
  backed up.

**Take, for what can be decoded:** the `image` crate (0.25, MIT or Apache-2.0,
so compatible with this project's licence) handles JPEG, PNG, WebP, GIF and
TIFF with no C library behind it. HEIC needs libheif and video needs ffmpeg;
both are out of scope until someone asks for them.

### A content hash   (unsettled, an optimisation)
Proton appears to deduplicate photos so a phone re-uploading its camera roll
does not double the library. If so, sending the hash lets an ingested photo the
phone already uploaded be recognised server-side rather than duplicated.

This is worth having and is **not** a blocker: item 0 already makes our own
passes idempotent. Settle the algorithm, the input bytes and the field name by
reading them off an official client, and add it when known.

### The photo's place in the volume
Photos go in the photos volume, under its own share, not in the Drive tree. The
existing upload path writes to the Drive volume and sends
`MIMEType: application/octet-stream`, neither of which is right here.

**Take:** a photos-specific upload that reuses the block, key and revision
machinery, with the photo metadata alongside it.

### After a successful upload
Only after the revision is sealed and the link confirmed:

- `photos_ingestion_perm_rm: false` (the default) moves the file to the
  desktop's trash.
- `photos_ingestion_perm_rm: true` deletes it.

**Take:** hand the file to the desktop rather than reimplementing the trash
specification. A file on another filesystem — an SD card or a phone mount,
which is exactly what the ingestion folder may be — cannot be renamed into the
home trash directory, and the specification's answer is a trash directory on
that filesystem, plus collision handling for names already in there. `gio
trash` or the portal does all of that and is already installed on a KDE
desktop.

Nothing is removed if the upload failed, was skipped, or was a duplicate the
server already had. A photo that cannot be ingested stays put, so the folder
becomes the list of what still needs a human.

### Where ingestion runs, and when
- It needs **its own inotify watch**: the existing one covers the sync root,
  and the ingestion folder is by definition outside it.
- The photo pass runs every half hour, so without a watch a dropped photo waits
  that long. With one, it should settle first.
- **"Settled" needs its own definition**, not the sync folder's debounce: that
  is a folder-wide quiet window with a thirty-second cap, which still fires
  part-way through a large video. Use size and modified time unchanged across
  two looks a few seconds apart.
- One photo at a time to begin with. Concurrency after correctness.

### Guards
- The ingestion folder may not be inside the sync folder or the photos
  destination: two mechanisms would fight over the same file.
- Fix the guard that exists before adding two more folders to it. It compares
  raw paths, so a symlink walks through it, and it only runs when a sync root
  is already recorded, so `--dest` pointed at the future sync folder passes
  before setup has run. Canonicalise both paths, and fall back to the
  configured folder when no sync state exists yet.

## The download side, as it stands

### 1. A trashed photo may be retried forever   (unsettled precondition)
`photo_nodes` drops anything trashed, so no entry is recorded and the id is
selected again next pass. **Verified** as a mechanism; what is **unsettled** is
whether trashed photos appear in the timeline at all. If they do, a library
with a few hundred of them wastes a page of calls every half hour, for ever.

**Do:** confirm the precondition first. Then record a skip explicitly — a flag
on the entry, not an empty path, which would resolve to the destination folder
and read as present. Re-check skipped ids on a slow cadence and on `--refetch`,
so a photo restored from the web trash is not blacklisted for good, and a
server that momentarily omits a link does not cost a real photo.

### 2. Deleting the local copy brings it back
"Needs fetching" means *no entry, or the file is gone*, so removing a photo
locally means it returns within the half hour. There is no way to keep a
subset.

**Do:** treat a tracked file that has gone as the user's decision. Add
`kpdrive photos --refetch` for a loss that was not deliberate.

### 3. An edited photo is never refreshed
The entry stores the revision but only reads it when restoring a missing file.

**Do:** compare it and re-fetch when it differs, as the file sync already does.

### 4. Removals in Proton never reach the copy
A photo deleted in Proton stays on disk. Safe, and probably right for a copy,
but it is not written down and the folder drifts.

**Do:** keep not deleting, and say so in the README.

### 5. The whole timeline is listed every pass
20,000 photos is 40 requests a pass, about 2,000 a day, to learn that nothing
changed.

**Do:** use the event stream. **Verified** against a live account: the photos
volume answers `drive/volumes/<photos volume id>/events/latest` with an event
id, the same route and shape the file sync already uses, while the
photos-prefixed variant is a 404. So this is the existing machinery pointed at
a second volume, and the cursor belongs in the photo state.

An earlier draft suggested stopping the paging early once a page held only
known ids. That is unsafe and is dropped: paging is anchored by link id rather
than capture time, so an imported old camera roll inserts photos behind the
stop point and they would never be fetched.

### 6. Downloads are one at a time
The file sync keeps six blocks in flight; photos never learned to.

**Do:** the same buffered stream, over photos rather than blocks — but two
fixes come first, or it ships corruption:

- a part-file name unique per photo, since the current one replaces the
  extension and collides between `IMG_1.jpg` and `IMG_1.png`;
- a name reserved before the download starts, since names are currently chosen
  by whether the path exists, and two downloads in flight would both be handed
  the same one.

Clean up orphaned part files while there: a failed download leaves one for
ever.

### 7. Nothing bounds the first run
Switching photos on downloads the entire library, with no estimate and no check
that the disk can hold it.

**Do:** check free space before each chunk and stop cleanly when it runs low,
saying what was fetched and what remains. A total up front is not available:
**verified** that the timeline carries only a link id and a capture time, and
that link details carry no size field, so the size lives in the encrypted
attributes of each revision. Totalling it would mean fetching and decrypting
every link before downloading anything, which costs more than the check saves.

### 8. The window says nothing about photos
A pass is invisible until it finishes and a notification appears.

**Do:** carry a photo count in the report the window already polls.

## State, shared by both halves

`photos.json` is loaded, mutated and written by the command and by the daemon,
with a fixed temporary name. Two passes at once already risk losing entries,
and the plan adds a command people will run while the daemon is up.

**Do, before any new photo verb:**

- refuse to run the command when a daemon is running, or take a lock;
- write through a temporary name unique to the process;
- make a corrupt or missing state file degrade to adopting what is on disk
  rather than re-downloading it under `(2)` names. Today a corrupt file fails
  every load, and the only recovery duplicates the library.

## Order

**Answer this first:** does the timeline include videos, and are they whole
files or paired assets? It decides whether the download half is already missing
half of a typical library, and it decides how much of the ingestion half can
ever work. One probe against an account with photos in it.

Then, download side: the state safety above, then 1, 3 and 2, which are small
and two of which are wrong rather than missing. Then 5, which removes most of
the per-pass cost, then 6 with its two prerequisites, then 7 and 8. 4 is a
paragraph of documentation.

Then ingestion: item 0, the capture-time policy, and the thumbnail question,
which is the one that can still change the shape of the feature. The content
hash is wanted but blocks nothing.

## Cut

Deliberately not doing, unless someone asks: a `--since` filter, a `Removed/`
folder mirroring deletions, reflecting album membership as folders, and logging
which time source a capture time came from.

## Open questions

- Does the timeline include videos, and are they whole files or paired assets?
- Is a thumbnail required by the API, or only by the eye?
- The content hash: algorithm, input, and field name.
- Live photos and bursts: if the timeline names only the primary, the rest are
  never fetched, and an ingested live photo may need both parts.
- Are trashed photos listed in the timeline, or do the ids merely linger?
- Do album members appear in the timeline?

# Photos: how they should be handled

Proton Photos is a second library, on a volume of its own, and today kpdrive
only reads it. The configuration now names two halves that do not exist yet,
`photos_ingestion_folder` and `photos_ingestion_perm_rm`, which is the gap this
plan is about: putting photos **into** Proton Photos from the desktop, and what
happens to the local file afterwards.

The second half of this document is the smaller list of things the download
side gets wrong today.

## What happens today

One pass, from `kpdrive photos` or from the daemon every half hour when
`photos_sync` is on:

1. Ask for the photos share; no share means the account has no library.
2. Page the whole timeline, 500 ids per page, until a short page.
3. Sort by capture time, drop duplicate ids.
4. Anything with no recorded entry, or whose recorded file is missing, is
   fetched in chunks of 150, one download at a time.
5. Each lands in `<photos_sync_folder>/YYYY/MM/<name>`, its modified time set
   to the capture time, ` (2)` appended when that name is taken.

The destination is refused if it sits inside the sync folder, which would
upload the whole library into Drive as ordinary files.

## Ingestion: uploading a photo

Why ingestion and not editing the timeline: the README's design notes settle
that. Proton Photos decides where a photo sits in the timeline, and editing a
photo's metadata can move it, so the only honest operation from a desktop
client is adding a photo and letting the library place it.

The shape of it: a folder the user drops photos into, or points at their
phone's import directory. Each pass, anything new in there goes up into Proton
Photos, and the local file is then trashed or deleted according to
`photos_ingestion_perm_rm`.

Uploading a photo is not uploading a file. Four things have to be produced that
the file path never needed, and each is a decision:

### A capture time
Proton files a photo by when it was taken, not when it was uploaded. That comes
from EXIF `DateTimeOriginal`, and from the file's modified time when there is
no EXIF, which is the case for screenshots and anything already stripped.

**Take:** `kamadak-exif` for the read, modified time as the fallback, and say
in the log which was used when it matters.

### A thumbnail
Every photo in the library carries one; the web app will show a blank tile
without it. Making one means decoding the image and scaling it, which is the
first real dependency this project would take on for a feature rather than a
protocol.

**Take:** the `image` crate, which decodes JPEG, PNG, WebP, GIF and TIFF with
no C library behind it. HEIC and video need libheif and ffmpeg respectively, so
those are out of scope until someone asks: skip them with a line in the log
rather than uploading something the library cannot show.

### A content hash
Proton deduplicates photos by a hash so a phone re-uploading its camera roll
does not double it. Ingesting the same file twice, or ingesting a photo that is
already in the library from the phone, has to be a no-op rather than a
duplicate.

**Settle first:** which hash, over what bytes, and where it goes in the upload
payload. This is the one thing in this plan that cannot be guessed and has to
be read off the wire or from an official client.

### The photo's place in the volume
Photos go in the photos volume, under its own share, not in the Drive tree. The
existing upload path writes to the Drive volume and sends
`MIMEType: application/octet-stream`, neither of which is right here.

**Take:** a photos-specific upload that reuses the block, key and revision
machinery, with the photo metadata alongside it.

### After a successful upload
Only after the revision is sealed and the link confirmed:

- `photos_ingestion_perm_rm: false` (the default) moves the file to the desktop
  trash, following the XDG spec so it shows up in Dolphin's Trash: the file
  into `~/.local/share/Trash/files`, a `.trashinfo` beside it recording the
  original path and time.
- `photos_ingestion_perm_rm: true` deletes it.

Nothing is removed if the upload failed, was skipped, or was a duplicate the
server already had. A photo that cannot be thumbnailed is left where it is, so
the folder becomes the list of what still needs a human.

### Guards worth having from the start
- The ingestion folder may not be inside the sync folder or the photos
  destination, for the same reason the destination may not be inside the sync
  folder: two mechanisms would fight over the same file.
- Only settle on a file that has stopped changing, the same debounce the file
  watcher uses, or a half-copied photo goes up.
- Upload one photo at a time to begin with. The concurrency that made file
  uploads fast can come after correctness.

## The download side, as it stands

### 1. A trashed photo is retried forever   (bug)
The timeline lists it, the link fetch drops anything trashed, and nothing is
recorded, so every pass asks again. On a library with a few hundred trashed
photos that is a page of wasted calls every half hour.

**Do:** record an entry for an id that resolves to nothing, and skip it after.

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

**Do:** keep not deleting, and say so in the README. If it is ever wanted, a
`Removed/` folder rather than an outright delete: for most people this copy is
the backup.

### 5. The whole timeline is listed every pass
20,000 photos is 40 requests a pass, about 2,000 a day, to learn that nothing
changed.

**Do:** the photos volume has its own event stream, the same shape the file
sync already uses. Until that is built, stop paging once a whole page is ids
already recorded.

### 6. Downloads are one at a time
The file sync keeps six blocks in flight; photos never learned to.

**Do:** the same buffered stream, over photos rather than blocks.

### 7. Nothing bounds the first run
Switching photos on downloads the entire library, with no estimate and no check
that the disk can hold it.

**Do:** total the sizes first and refuse if the free space does not cover it,
naming both numbers. `--since YYYY-MM` is the natural companion.

### 8. The window says nothing about photos
A pass is invisible until it finishes and a notification appears.

**Do:** carry a photo count in the report the window already polls.

## Order

Ingestion is the feature; the download fixes are maintenance. Within the
download list, 1, 3 and 2 are small and two of them are wrong rather than
missing, so they are worth doing first whatever happens to ingestion. 5 and 6
are what make a large library bearable, 7 and 8 are comfort, and 4 is a
paragraph of documentation.

For ingestion itself, nothing can start before the content hash is settled:
without it, every pass risks duplicating the library.

## Open questions, to answer against a real library

- The content hash: algorithm, input, and field name in the upload payload.
- Does the timeline include videos, and are they whole files or paired assets?
- Live photos and bursts: if the timeline names only the primary, the rest are
  never fetched, and an ingested live photo may need both parts.
- Are trashed photos listed in the timeline, or do the ids merely linger?
- Is the timeline ordered newest first? The cheap fix in 5 depends on it.
- Do album members appear in the timeline, and is album membership worth
  reflecting as folders, or is `YYYY/MM` enough?

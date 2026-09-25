# Changelog

## 0.6.0 - 2026-09-25

- Multi-account support

## 0.5.1 - 2026-09-25

- Reap the programs the daemon starts to avoid zombies
- update screenshots & readme

## 0.5.0 - 2026-09-24

- Updated translations to match new UI design
- Trash the local copy of a photo deleted in Proton
- Pause syncing on a chosen power source, low battery by default
- added about section in ui
- added sync pause added network selector for auto-pause
- Photo ingestion, and photo downloads that actually download

## 0.4.1 - 2026-09-23

- No changes, changes are limited to dev build and package info

## 0.4.0 - 2026-09-23

- Localizations with AI translated text
- Integrate photos in UI and main daemon
- Photos timeline in read-only, Photos ingestion Folder
- Open account window from double clicking on tray icon
- Readme rewritten

## 0.3.0 - 2026-09-21

- Ask before rm trashes something, unless told not to
- Report losing the network once, not every thirty seconds
- Show only this run's log, newest first
- Keep the daemon and the window in step about the session
- Better install instructionss

## 0.2.0 - 2026-09-20

- Ask what to do about a sync folder that already has files
- Let the window choose which log levels are stored
- Show the newest hundred log lines
- Make the log text selectable
- Stop logging a warning when there is no .protonignore
- Push a folder's changed files concurrently
- Sync from the event stream instead of walking the tree
- Speed: parallel transfers, shareable API client, parallel walk, inotify
- Performance plan, with the measurements behind it
- changing icon
- added fedora update ca fix compiling issues

## 0.1.0 - 2026-09-20

- Fix the Debian CI job and make it catch bad package names
- readme & icon change
- Replace the icon with a pure-vector one
- changing readme
- Build RPMs for two Fedoras and DEBs for Debian
- Package as RPMs and release from version tags
- Configurable sync folder and .protonignore
- Account window, activity log, and GPL-3.0-or-later
- Photos: download the timeline
- Public links: share/unshare plus a Dolphin menu entry
- Conflicts: keep both versions instead of stalling
- KDE integration: daemon with tray and status socket, setup, Dolphin overlay plugin
- Uploads and two-way sync
- Proton Drive client skeleton: browser login, session in KWallet, ls and get

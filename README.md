# kpdrive

Proton Drive sync client for KDE Plasma, written in Rust.

## Decisions

- **Shape:** sync folder (default `~/ProtonDrive`), Nextcloud-style. Not a FUSE mount.
- **API client:** our own thin client over the Drive REST API. Crypto comes from
  Proton's official [proton-crypto-rs](https://github.com/ProtonMail/proton-crypto-rs)
  (MIT, pure-Rust `rustpgp` backend, pinned commit). No third-party SDK ports.
- **One binary:** `kpdrive` runs as the daemon or as a CLI talking to it over a Unix socket.
- **KDE integration without C++:** tray via StatusNotifierItem, notifications and
  KWallet via D-Bus, prompts via `kdialog`, browser via `xdg-open`, Places entry via `user-places.xbel`,
  context menu via `kio/servicemenus`.
- **The one C++ piece:** Dolphin overlay icons (`KOverlayIconPlugin`). Lives in its own
  directory, talks to the daemon socket, optional package.
- **Packaging:** `cargo install` now; RPM, deb, AUR later.
- **Terms:** Proton permits personal, non-commercial third-party use of its SDK/API today.

## Milestones

1. Login via browser sign-in (Proton session fork: the browser handles password, 2FA, captcha), session stored in KWallet.
2. List root, download one file.
3. Remote → local one-way sync driven by the event stream.
4. Local → remote uploads.
5. Tray, Places entry, notifications, servicemenus, Dolphin overlay plugin.

Conflicts, trash, sharing and photos wait until two-way sync is stable.

## Build

```
cargo build
```

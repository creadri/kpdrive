# Dolphin overlay icons

The only C++ in kpdrive. Shows a check mark on synced files and a pending
badge on files waiting to be pushed, by asking the running daemon
(`kpdrive sync --watch`) over `$XDG_RUNTIME_DIR/kpdrive.sock`.

Fedora build deps:

```
sudo dnf install cmake extra-cmake-modules qt6-qtbase-devel kf6-kcoreaddons-devel kf6-kio-devel
```

Build and install system-wide (Qt only loads plugins from its install prefix):

```
cmake -S dolphin-overlay -B dolphin-overlay/build -DCMAKE_INSTALL_PREFIX=/usr -DCMAKE_BUILD_TYPE=Release
cmake --build dolphin-overlay/build
sudo cmake --install dolphin-overlay/build
```

Restart Dolphin afterwards.

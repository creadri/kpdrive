# The version comes from the git tag in CI:
#   rpmbuild --define "kpdrive_version 1.2.3"
# The default below only exists so a local build works without it.
%{!?kpdrive_version: %global kpdrive_version 0.1.0}

# Cargo release builds carry no debug symbols, so the debuginfo extraction
# would produce an empty package and fail the build.
%global debug_package %{nil}

Name:           kpdrive
Version:        %{kpdrive_version}
Release:        1%{?dist}
Summary:        Proton Drive sync client for KDE Plasma
License:        GPL-3.0-or-later
URL:            https://github.com/creadri/kpdrive
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz
# Every crate, including the proton-crypto git checkout, from `cargo vendor`.
# The release workflow makes it and attaches it to the release; Koji and COPR
# build without network access, so nothing may be fetched while building.
Source1:        %{url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo
# cargo_vendor_manifest, and the bundled(crate(...)) Provides it feeds
BuildRequires:  cargo-rpm-macros >= 24
BuildRequires:  rust
BuildRequires:  gcc-c++
BuildRequires:  cmake
BuildRequires:  extra-cmake-modules
# rustls builds AWS-LC, which is C and drives its own cmake/perl steps.
BuildRequires:  perl-interpreter
# msgfmt, which compiles the translation catalogs into the binaries
BuildRequires:  gettext
BuildRequires:  qt6-qtbase-devel
BuildRequires:  qt6-qtdeclarative-devel
BuildRequires:  kf6-kcoreaddons-devel
BuildRequires:  kf6-kio-devel
BuildRequires:  kf6-kirigami-devel
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# The session needs something answering org.freedesktop.secrets to hold the
# Proton session. On Plasma that is KWallet; gnome-keyring also qualifies.
Recommends:     kf6-kwallet
# `kpdrive share --copy` puts the link on the clipboard.
Recommends:     wl-clipboard
# Prompts during `kpdrive login` when run from a terminal.
Recommends:     kdialog

%description
kpdrive keeps a local folder in step with Proton Drive, in both directions and
end to end encrypted. It runs as a daemon with a Plasma tray icon, creates
public links, downloads the Photos timeline, and honours a .protonignore file.

%package ui
Summary:        Account and activity log window for kpdrive
Requires:       %{name}%{?_isa} = %{version}-%{release}
Requires:       kf6-kirigami
Requires:       qt6-qtdeclarative

%description ui
A Kirigami window showing the signed-in account, storage in use, the sync
folder, and the activity log with search and a retention setting.

%package dolphin
Summary:        Dolphin integration for kpdrive
Requires:       %{name}%{?_isa} = %{version}-%{release}
Requires:       kf6-kio
Supplements:    (%{name} and dolphin)

%description dolphin
Overlay icons marking which files in the sync folder are up to date, and a
"Copy Proton Drive link" entry in the context menu.

%prep
%autosetup -a1
# Swap crates.io and the proton-crypto git source for vendor/.
cat cargo-vendor-config.toml >> .cargo/config.toml

%build
# Under Fedora's RUSTFLAGS (full debuginfo, one codegen unit) rustc 1.98
# overflows its stack on zerocopy's token trees and on the DWARF for our deep
# async types. It asked for 32 MiB; 64 leaves room as the code grows.
export RUST_MIN_STACK=67108864
cargo build --release --workspace --offline --locked
%cargo_vendor_manifest

cmake -S dolphin-overlay -B build-dolphin \
      -DCMAKE_INSTALL_PREFIX=%{_prefix} \
      -DCMAKE_INSTALL_LIBDIR=%{_libdir} \
      -DCMAKE_BUILD_TYPE=Release
cmake --build build-dolphin --parallel

%install
install -Dpm0755 target/release/kpdrive    %{buildroot}%{_bindir}/kpdrive
install -Dpm0755 target/release/kpdrive-ui %{buildroot}%{_bindir}/kpdrive-ui
install -Dpm0644 packaging/be.otterit.kpdrive.desktop \
                 %{buildroot}%{_datadir}/applications/be.otterit.kpdrive.desktop
install -Dpm0644 packaging/be.otterit.kpdrive.metainfo.xml \
                 %{buildroot}%{_metainfodir}/be.otterit.kpdrive.metainfo.xml
install -Dpm0644 packaging/kpdrive-share.desktop \
                 %{buildroot}%{_datadir}/kio/servicemenus/kpdrive-share.desktop
install -Dpm0644 assets/icon.svg \
                 %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/be.otterit.kpdrive.svg
DESTDIR=%{buildroot} cmake --install build-dolphin

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/be.otterit.kpdrive.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/be.otterit.kpdrive.metainfo.xml
cargo test --release --workspace --offline --locked

%files
%license LICENSE cargo-vendor.txt
%doc README.md
%{_bindir}/kpdrive

%files ui
%{_bindir}/kpdrive-ui
%{_datadir}/applications/be.otterit.kpdrive.desktop
%{_metainfodir}/be.otterit.kpdrive.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/be.otterit.kpdrive.svg

%files dolphin
%{_qt6_plugindir}/kf6/overlayicon/kpdriveoverlay.so
%{_datadir}/kio/servicemenus/kpdrive-share.desktop

%changelog
* Wed Sep 23 2026 Adrien Nelis <github.com.daybed777@passmail.net> - 0.4.0-1
- adding script to increase version release due to lazyness
- Twenty-four languages, and count them correctly
- Tell translators what the ambiguous fragments mean
- Speak French, and whatever else anyone writes a catalog for
- rewrite readme
- Rewrite the photos plan after review
- Name the photo settings as the README specifies, and plan ingestion
- Let the sync daemon bring down Proton Photos too
- Open the account window from the tray icon

* Mon Sep 21 2026 Adrien Nelis <github.com.daybed777@passmail.net> - 0.3.0-1
- updating version
- Ask before rm trashes something, unless told not to
- Report losing the network once, not every thirty seconds
- Show only this run's log, newest first
- Keep the daemon and the window in step about the session
- install instruction & screenshot

* Sun Sep 20 2026 Adrien Nelis <github.com.daybed777@passmail.net> - 0.2.0-1
- changing to verison 0.2.0
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

* Sun Sep 20 2026 Adrien Nelis <github.com.daybed777@passmail.net> - 0.1.0-1
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

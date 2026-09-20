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
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc-c++
BuildRequires:  cmake
BuildRequires:  extra-cmake-modules
# rustls builds AWS-LC, which is C and drives its own cmake/perl steps.
BuildRequires:  perl-interpreter
BuildRequires:  qt6-qtbase-devel
BuildRequires:  qt6-qtdeclarative-devel
BuildRequires:  kf6-kcoreaddons-devel
BuildRequires:  kf6-kio-devel
BuildRequires:  kf6-kirigami-devel

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
%autosetup

%build
# Cargo fetches the dependency tree here, so the build needs network access.
cargo build --release --workspace

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
install -Dpm0644 packaging/kpdrive-share.desktop \
                 %{buildroot}%{_datadir}/kio/servicemenus/kpdrive-share.desktop
install -Dpm0644 assets/icon.svg \
                 %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/be.otterit.kpdrive.svg
DESTDIR=%{buildroot} cmake --install build-dolphin

%check
cargo test --release --workspace

%files
%license LICENSE
%doc README.md
%{_bindir}/kpdrive

%files ui
%{_bindir}/kpdrive-ui
%{_datadir}/applications/be.otterit.kpdrive.desktop
%{_datadir}/icons/hicolor/scalable/apps/be.otterit.kpdrive.svg

%files dolphin
%{_qt6_plugindir}/kf6/overlayicon/kpdriveoverlay.so
%{_datadir}/kio/servicemenus/kpdrive-share.desktop

%changelog
* Sun Sep 20 2026 Adrien Nelis <adrien.nelis@otterit.be> - 0.1.0-1
- First packaged release.

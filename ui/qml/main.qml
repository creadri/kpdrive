// The account and log window, laid out like System Settings: a sidebar of
// pages on the left, the page on the right. Plain strings rather than i18n():
// that call comes from KDE's localization context, which a cxx-qt engine does
// not install.
import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import QtQuick.Dialogs
import org.kde.kirigami as Kirigami
import org.kde.kirigami.dialogs as KirigamiDialogs
import be.otterit.kpdrive

Kirigami.ApplicationWindow {
    id: root

    title: "Proton Drive"
    width: Kirigami.Units.gridUnit * 52
    height: Kirigami.Units.gridUnit * 36
    minimumWidth: Kirigami.Units.gridUnit * 36
    minimumHeight: Kirigami.Units.gridUnit * 26

    // The page title band below is drawn here; Kirigami's own header would
    // only add an empty band above it.
    pageStack.globalToolBar.style: Kirigami.ApplicationHeaderStyle.None

    FolderDialog {
        id: folderDialog
        title: backend.i18n("Choose the Proton Drive folder")
        currentFolder: root.folderUrl(backend.syncFolder)
        onAccepted: root.chooseFolder(root.urlPath(selectedFolder))
    }

    FolderDialog {
        id: photosDialog
        title: backend.i18n("Choose where Proton Photos are downloaded")
        currentFolder: root.folderUrl(backend.photosFolder)
        onAccepted: backend.changePhotosFolder(root.urlPath(selectedFolder))
    }

    FolderDialog {
        id: ingestDialog
        title: backend.i18n("Choose the folder to upload photos from")
        currentFolder: root.folderUrl(backend.ingestFolder)
        onAccepted: backend.changeIngestFolder(root.urlPath(selectedFolder))
    }

    // The same question the CLI asks, with the same two answers.
    KirigamiDialogs.PromptDialog {
        id: occupiedPrompt

        property string folder: ""

        title: backend.i18n("The folder is not empty")
        standardButtons: Controls.Dialog.NoButton
        customFooterActions: [
            Kirigami.Action {
                text: backend.folderChoices()[0]
                icon.name: "merge"
                onTriggered: {
                    backend.changeSyncFolder(occupiedPrompt.folder, "merge");
                    occupiedPrompt.close();
                }
            },
            Kirigami.Action {
                text: backend.folderChoices()[1]
                icon.name: "edit-move"
                onTriggered: {
                    backend.changeSyncFolder(occupiedPrompt.folder, "rename");
                    occupiedPrompt.close();
                }
            }
        ]
    }

    Backend {
        id: backend

        // Sign-in needs the browser in front more than anything else does.
        onOpenUrlRequested: url => Qt.openUrlExternally(url)
    }

    // The daemon has no way to push, so ask it now and then.
    Timer {
        interval: 3000
        running: root.visible
        repeat: true
        triggeredOnStart: true
        onTriggered: backend.refreshSyncStatus()
    }

    // Clears itself rather than carrying a close button: the button would
    // assign visible and break the binding on the message.
    Timer {
        running: backend.status.length > 0
        interval: 8000
        onTriggered: backend.status = ""
    }

    // Qt asks the compositor for an activation token before handing the URL
    // over, which is what lets the browser raise itself on Wayland. Launching
    // xdg-open directly cannot, so the tab opens behind this window.
    function open(url, what) {
        if (Qt.openUrlExternally(url))
            backend.status = backend.i18n("Opened {what}").replace("{what}", what);
        else
            backend.status = backend.i18n("Could not open {what} ({url})").replace("{what}", what).replace("{url}", url);
    }

    // Folder dialogs speak URLs; the backend wants plain paths.
    function folderUrl(path) {
        return path.length > 0 ? "file://" + encodeURI(path) : "";
    }
    function urlPath(url) {
        return decodeURIComponent(url.toString().replace(/^file:\/\//, ""));
    }

    // The log is shown as one text block so a selection can run across lines,
    // which means the severity colours have to be markup rather than a
    // property per row. Text read from disk is escaped before it becomes markup.
    function renderLog(lines) {
        const out = [];
        for (let i = 0; i < lines.length; i++) {
            const line = lines[i];
            const safe = line.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
            // Only the lines that need a colour get markup: a span per line
            // costs real memory once a log runs to thousands of them, and the
            // ordinary ones already read as the item's own colour.
            const colour = line.indexOf(" ERROR ") >= 0 ? Kirigami.Theme.negativeTextColor
                         : line.indexOf(" WARN ") >= 0 ? Kirigami.Theme.neutralTextColor
                         : null;
            out.push(colour ? '<span style="color:' + colour + '">' + safe + '</span>' : safe);
        }
        return out.join("<br>");
    }

    // Switching to a folder that already holds files is the user's call, so it
    // is put to them in the words the CLI uses. An empty one needs no asking.
    function chooseFolder(path) {
        const question = backend.folderQuestion(path);
        if (question.length === 0) {
            backend.changeSyncFolder(path, "merge");
            return;
        }
        occupiedPrompt.folder = path;
        occupiedPrompt.subtitle = question;
        occupiedPrompt.open();
    }

    function formatSize(bytes) {
        if (!bytes || bytes < 1)
            return "0 GiB";
        const gib = bytes / (1024 * 1024 * 1024);
        if (gib >= 1)
            return gib.toLocaleString(Qt.locale(), "f", 1) + " GiB";
        return (bytes / (1024 * 1024)).toLocaleString(Qt.locale(), "f", 1) + " MiB";
    }

    pageStack.initialPage: Kirigami.Page {
        padding: 0

        RowLayout {
            anchors.fill: parent
            spacing: 0

            // ---- Sidebar ----------------------------------------------------
            Rectangle {
                Layout.fillHeight: true
                Layout.preferredWidth: Kirigami.Units.gridUnit * 13
                Kirigami.Theme.colorSet: Kirigami.Theme.View
                Kirigami.Theme.inherit: false
                color: Kirigami.Theme.backgroundColor

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: Kirigami.Units.smallSpacing
                    spacing: Kirigami.Units.smallSpacing

                    ListView {
                        id: sidebar
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        clip: true
                        spacing: Kirigami.Units.smallSpacing / 2
                        model: [
                            { name: backend.i18n("Account & activity"), icon: "folder-cloud" },
                            { name: backend.i18n("Settings"), icon: "configure" },
                            { name: backend.i18n("Logs"), icon: "view-history" },
                        ]
                        delegate: Controls.ItemDelegate {
                            required property var modelData
                            required property int index
                            width: ListView.view.width
                            text: modelData.name
                            icon.name: modelData.icon
                            highlighted: ListView.isCurrentItem
                            onClicked: sidebar.currentIndex = index
                        }
                    }

                    Controls.Label {
                        Layout.fillWidth: true
                        Layout.leftMargin: Kirigami.Units.smallSpacing
                        text: "kpdrive " + backend.version + " · " + backend.license
                        font: Kirigami.Theme.smallFont
                        opacity: 0.7
                        wrapMode: Text.WordWrap
                    }

                    Controls.Button {
                        text: backend.i18n("Read the licence")
                        flat: true
                        font: Kirigami.Theme.smallFont
                        onClicked: root.open("https://www.gnu.org/licenses/gpl-3.0.html", "the licence")
                    }
                }
            }

            Kirigami.Separator { Layout.fillHeight: true }

            // ---- Page -------------------------------------------------------
            ColumnLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                spacing: 0

                Kirigami.Heading {
                    Layout.fillWidth: true
                    Layout.margins: Kirigami.Units.largeSpacing
                    Layout.leftMargin: Kirigami.Units.gridUnit
                    level: 2
                    text: sidebar.model[sidebar.currentIndex].name
                }

                Kirigami.Separator { Layout.fillWidth: true }

                // Outcomes of what was just clicked, whichever page it was on.
                Kirigami.InlineMessage {
                    Layout.fillWidth: true
                    Layout.margins: Kirigami.Units.smallSpacing
                    visible: backend.status.length > 0
                    text: backend.status
                    type: Kirigami.MessageType.Information
                }

                StackLayout {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    currentIndex: sidebar.currentIndex
                    // Networks come and go; the list is read when shown.
                    onCurrentIndexChanged: if (currentIndex === 1) backend.reloadNetworks()

                    // ---- Account & activity ---------------------------------
                    Controls.ScrollView {
                        id: accountScroll
                        contentWidth: availableWidth

                        ColumnLayout {
                            x: Kirigami.Units.gridUnit
                            width: accountScroll.availableWidth - Kirigami.Units.gridUnit * 2
                            spacing: Kirigami.Units.largeSpacing

                            Item { implicitHeight: Kirigami.Units.smallSpacing }

                            RowLayout {
                                Layout.fillWidth: true
                                spacing: Kirigami.Units.largeSpacing

                                Kirigami.Icon {
                                    source: "folder-cloud"
                                    implicitWidth: Kirigami.Units.iconSizes.huge
                                    implicitHeight: Kirigami.Units.iconSizes.huge
                                }

                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: Kirigami.Units.smallSpacing

                                    Kirigami.Heading {
                                        level: 2
                                        text: backend.loggedIn ? backend.username : backend.i18n("Not signed in")
                                        elide: Text.ElideRight
                                        Layout.fillWidth: true
                                    }

                                    Controls.Label {
                                        text: backend.loggedIn
                                              ? backend.i18n("Signed in to Proton Drive")
                                              : backend.i18n("Sign in to sync this computer with Proton Drive.")
                                        opacity: 0.7
                                        wrapMode: Text.WordWrap
                                        Layout.fillWidth: true
                                    }
                                }

                                Controls.BusyIndicator {
                                    visible: backend.busy
                                    running: visible
                                }
                            }

                            Kirigami.FormLayout {
                                Layout.fillWidth: true

                                ColumnLayout {
                                    Kirigami.FormData.label: backend.i18n("Storage:")
                                    visible: backend.loggedIn
                                    spacing: Kirigami.Units.smallSpacing

                                    // Drawn rather than Controls.ProgressBar:
                                    // Breeze's version drives an internal
                                    // Repeater off its own width and throws
                                    // "cannot read property of null" in a loop
                                    // when that width is constrained.
                                    Rectangle {
                                        implicitWidth: Kirigami.Units.gridUnit * 16
                                        implicitHeight: Kirigami.Units.gridUnit / 2
                                        radius: height / 2
                                        color: Kirigami.Theme.alternateBackgroundColor

                                        Rectangle {
                                            anchors.left: parent.left
                                            anchors.top: parent.top
                                            anchors.bottom: parent.bottom
                                            width: backend.totalBytes > 0
                                                   ? parent.width * Math.min(1, backend.usedBytes / backend.totalBytes)
                                                   : 0
                                            radius: parent.radius
                                            color: Kirigami.Theme.highlightColor
                                            visible: width > 0
                                        }
                                    }

                                    Controls.Label {
                                        text: backend.i18n("{used} of {total} used")
                                              .replace("{used}", root.formatSize(backend.usedBytes))
                                              .replace("{total}", root.formatSize(backend.totalBytes))
                                        opacity: 0.7
                                    }
                                }

                                RowLayout {
                                    Kirigami.FormData.label: backend.i18n("Account:")

                                    Controls.Button {
                                        text: backend.loggedIn ? backend.i18n("Sign out") : backend.i18n("Sign in")
                                        icon.name: backend.loggedIn ? "system-log-out" : "key-enter"
                                        enabled: !backend.busy
                                        onClicked: backend.loggedIn ? backend.logout() : backend.login()
                                    }
                                    Controls.Button {
                                        // TRANSLATORS: button that reloads the account details from Proton
                                        text: backend.i18n("Refresh")
                                        icon.name: "view-refresh"
                                        visible: backend.loggedIn
                                        enabled: !backend.busy
                                        onClicked: backend.refresh()
                                    }
                                }

                                RowLayout {
                                    Kirigami.FormData.label: backend.i18n("On the web:")

                                    Controls.Button {
                                        text: backend.i18n("Drive on the web")
                                        icon.name: "internet-services"
                                        onClicked: root.open("https://drive.proton.me/", "Proton Drive in your browser")
                                    }
                                    Controls.Button {
                                        text: backend.i18n("Proton account")
                                        icon.name: "system-users"
                                        onClicked: root.open("https://account.proton.me/", "your Proton account in your browser")
                                    }
                                }

                                Kirigami.Separator {
                                    Kirigami.FormData.isSection: true
                                    Kirigami.FormData.label: backend.i18n("Activity")
                                }

                                // What the sync daemon is doing. It is a
                                // separate process, so without this the window
                                // can only guess.
                                RowLayout {
                                    Kirigami.FormData.label: backend.i18n("Sync:")
                                    spacing: Kirigami.Units.smallSpacing

                                    Kirigami.Icon {
                                        Layout.alignment: Qt.AlignTop
                                        implicitWidth: Kirigami.Units.iconSizes.small
                                        implicitHeight: Kirigami.Units.iconSizes.small
                                        source: backend.syncPaused ? "media-playback-pause"
                                              : backend.syncFailed ? "data-error"
                                              : backend.syncOffline || !backend.daemonRunning ? "data-warning"
                                              : backend.syncBusy ? "view-refresh"
                                              : "data-success"
                                    }

                                    Controls.Label {
                                        Layout.maximumWidth: Kirigami.Units.gridUnit * 22
                                        text: backend.syncStatus
                                        wrapMode: Text.WordWrap
                                    }
                                }

                                RowLayout {
                                    Controls.Button {
                                        text: backend.i18n("Sync now")
                                        icon.name: "view-refresh"
                                        enabled: backend.daemonRunning && !backend.syncBusy && !backend.syncPaused
                                        onClicked: backend.syncNow()
                                    }
                                    // The manual pause only; a network pause
                                    // lifts itself when the network goes.
                                    Controls.Button {
                                        text: backend.pausedByHand ? backend.i18n("Resume") : backend.i18n("Pause")
                                        icon.name: backend.pausedByHand ? "media-playback-start" : "media-playback-pause"
                                        onClicked: backend.changePaused(!backend.pausedByHand)
                                    }
                                    Controls.Button {
                                        text: backend.i18n("Open folder")
                                        icon.name: "folder-open"
                                        enabled: backend.syncFolder.length > 0
                                        onClicked: root.open(root.folderUrl(backend.syncFolder), "the sync folder")
                                    }
                                }
                            }
                        }
                    }

                    // ---- Settings -------------------------------------------
                    Controls.ScrollView {
                        id: settingsScroll
                        contentWidth: availableWidth

                        Kirigami.FormLayout {
                            width: settingsScroll.availableWidth

                            Kirigami.Separator {
                                Kirigami.FormData.isSection: true
                                Kirigami.FormData.label: backend.i18n("Files")
                            }

                            RowLayout {
                                Kirigami.FormData.label: backend.i18n("Sync folder:")

                                Controls.Label {
                                    Layout.maximumWidth: Kirigami.Units.gridUnit * 16
                                    text: backend.syncFolder.length > 0 ? backend.syncFolder : backend.i18n("nowhere yet")
                                    elide: Text.ElideMiddle
                                    font.family: "monospace"
                                }
                                Controls.Button {
                                    // TRANSLATORS: button that opens a folder picker to sync somewhere else
                                    text: backend.i18n("Change…")
                                    icon.name: "folder-sync"
                                    onClicked: folderDialog.open()
                                }
                            }

                            Controls.Button {
                                Kirigami.FormData.label: backend.i18n("Ignored paths:")
                                // TRANSLATORS: button that opens .protonignore, the list of paths sync leaves alone
                                text: backend.i18n("Ignore file…")
                                icon.name: "document-edit"
                                onClicked: backend.openIgnoreFile()
                            }

                            Kirigami.Separator {
                                Kirigami.FormData.isSection: true
                                Kirigami.FormData.label: backend.i18n("Photos")
                            }

                            ColumnLayout {
                                Kirigami.FormData.label: backend.i18n("Download:")
                                spacing: 0

                                Controls.CheckBox {
                                    text: backend.i18n("Also download Proton Photos")
                                    checked: backend.syncPhotos
                                    onToggled: backend.changeSyncPhotos(checked)
                                }
                                Controls.Label {
                                    Layout.maximumWidth: Kirigami.Units.gridUnit * 22
                                    text: backend.i18n("Your timeline is copied here every half hour. Nothing in Proton Photos is changed or deleted by it.")
                                    wrapMode: Text.WordWrap
                                    opacity: 0.7
                                    font: Kirigami.Theme.smallFont
                                }
                            }

                            RowLayout {
                                Kirigami.FormData.label: backend.i18n("Download folder:")

                                Controls.Label {
                                    Layout.maximumWidth: Kirigami.Units.gridUnit * 16
                                    text: backend.photosFolder.length > 0 ? backend.photosFolder : backend.i18n("your Pictures folder")
                                    elide: Text.ElideMiddle
                                    font.family: "monospace"
                                }
                                Controls.Button {
                                    text: backend.i18n("Change…")
                                    icon.name: "folder-open"
                                    onClicked: photosDialog.open()
                                }
                            }

                            ColumnLayout {
                                Kirigami.FormData.label: backend.i18n("Upload:")
                                spacing: 0

                                Controls.CheckBox {
                                    text: backend.i18n("Upload photos from a folder")
                                    checked: backend.ingestFolder.length > 0
                                    // Turning it on means picking the folder;
                                    // the box follows the setting, not the click.
                                    onToggled: {
                                        checked = Qt.binding(function() { return backend.ingestFolder.length > 0 })
                                        if (backend.ingestFolder.length > 0)
                                            backend.changeIngestFolder("")
                                        else
                                            ingestDialog.open()
                                    }
                                }
                                Controls.Label {
                                    Layout.maximumWidth: Kirigami.Units.gridUnit * 22
                                    text: backend.i18n("Photos and videos put in this folder go up to Proton Photos within a minute, then leave the folder. Anything that cannot be uploaded stays.")
                                    wrapMode: Text.WordWrap
                                    opacity: 0.7
                                    font: Kirigami.Theme.smallFont
                                }
                            }

                            RowLayout {
                                Kirigami.FormData.label: backend.i18n("Upload folder:")
                                visible: backend.ingestFolder.length > 0

                                Controls.Label {
                                    Layout.maximumWidth: Kirigami.Units.gridUnit * 16
                                    text: backend.ingestFolder
                                    elide: Text.ElideMiddle
                                    font.family: "monospace"
                                }
                                Controls.Button {
                                    text: backend.i18n("Change…")
                                    icon.name: "folder-open"
                                    onClicked: ingestDialog.open()
                                }
                            }

                            Controls.CheckBox {
                                Kirigami.FormData.label: backend.i18n("After upload:")
                                visible: backend.ingestFolder.length > 0
                                text: backend.i18n("Delete instead of moving to the trash")
                                checked: backend.ingestPermRm
                                onToggled: backend.changeIngestPermRm(checked)
                            }

                            Kirigami.Separator {
                                Kirigami.FormData.isSection: true
                                Kirigami.FormData.label: backend.i18n("Network")
                            }

                            ColumnLayout {
                                Kirigami.FormData.label: backend.i18n("Pause while on:")
                                Kirigami.FormData.labelAlignment: Qt.AlignTop
                                spacing: Kirigami.Units.smallSpacing

                                Controls.Frame {
                                    visible: backend.networks.length > 0
                                    padding: 1
                                    Layout.preferredWidth: Kirigami.Units.gridUnit * 18
                                    Layout.preferredHeight: Math.min(networkList.contentHeight, Kirigami.Units.gridUnit * 10) + padding * 2

                                    ListView {
                                        id: networkList
                                        anchors.fill: parent
                                        clip: true
                                        model: backend.networks
                                        Controls.ScrollBar.vertical: Controls.ScrollBar {}
                                        delegate: Controls.CheckDelegate {
                                            required property string modelData
                                            width: ListView.view.width
                                            text: backend.activeNetworks.indexOf(modelData) >= 0
                                                  ? backend.i18n("{network} (connected)").replace("{network}", modelData)
                                                  : modelData
                                            checked: backend.pauseNetworks.indexOf(modelData) >= 0
                                            onToggled: backend.changeNetworkPause(modelData, checked)
                                        }
                                    }
                                }

                                Controls.Label {
                                    Layout.maximumWidth: Kirigami.Units.gridUnit * 18
                                    text: backend.networks.length > 0
                                          ? backend.i18n("Syncing pauses while any ticked network is connected, such as a phone's hotspot. A sync already under way finishes first.")
                                          : backend.i18n("NetworkManager lists no networks to choose from.")
                                    wrapMode: Text.WordWrap
                                    opacity: 0.7
                                    font: Kirigami.Theme.smallFont
                                }
                            }

                            Kirigami.Separator {
                                Kirigami.FormData.isSection: true
                                Kirigami.FormData.label: backend.i18n("Logs")
                            }

                            RowLayout {
                                Kirigami.FormData.label: backend.i18n("Keep logs for:")

                                Controls.SpinBox {
                                    from: 1
                                    to: 3650
                                    value: backend.retentionDays
                                    onValueModified: backend.setRetention(value)
                                }
                                // TRANSLATORS: after the number in "Keep logs for: [30] days"
                                Controls.Label { text: backend.i18n("days") }
                            }

                            Controls.ComboBox {
                                // TRANSLATORS: label before a dropdown choosing which log levels are written to disk
                                Kirigami.FormData.label: backend.i18n("Store:")
                                textRole: "text"
                                valueRole: "value"
                                model: [
                                    // TRANSLATORS: dropdown choice, store warnings and errors only
                                    { text: backend.i18n("Warnings and errors"), value: "WARN" },
                                    // TRANSLATORS: dropdown choice, store every log line
                                    { text: backend.i18n("Everything"), value: "INFO" },
                                    // TRANSLATORS: dropdown choice, store errors only
                                    { text: backend.i18n("Errors only"), value: "ERROR" },
                                ]
                                currentIndex: Math.max(0, indexOfValue(backend.logLevel))
                                onActivated: backend.changeLogLevel(currentValue)
                            }
                        }
                    }

                    // ---- Logs -----------------------------------------------
                    Item {
                        ColumnLayout {
                            anchors.fill: parent
                            anchors.margins: Kirigami.Units.largeSpacing
                            spacing: Kirigami.Units.smallSpacing

                            RowLayout {
                                Layout.fillWidth: true

                                Kirigami.SearchField {
                                    id: search
                                    Layout.fillWidth: true
                                    placeholderText: backend.i18n("Search the log")
                                    onTextChanged: backend.searchLogs(text)
                                }

                                Controls.Label {
                                    text: backend.i18np("{n} line", "{n} lines", backend.logLines.length).replace("{n}", backend.logLines.length)
                                    opacity: 0.7
                                }
                            }

                            Item {
                                Layout.fillWidth: true
                                Layout.fillHeight: true

                                Controls.ScrollView {
                                    id: logScroll
                                    anchors.fill: parent
                                    clip: true
                                    visible: backend.logLines.length > 0

                                    // Newest first, so what just happened is
                                    // in view without scrolling.
                                    function toTop() {
                                        logScroll.contentItem.contentY = 0;
                                    }

                                    // The whole log is one laid-out document
                                    // rather than a virtualised list, which is
                                    // what allows a selection to cross lines.
                                    // Laying it out costs memory per line, so
                                    // the backend hands over only its newest
                                    // hundred.
                                    TextEdit {
                                        id: logView
                                        width: logScroll.availableWidth
                                        readOnly: true
                                        selectByMouse: true
                                        selectByKeyboard: true
                                        textFormat: TextEdit.RichText
                                        wrapMode: TextEdit.WrapAnywhere
                                        font.family: "monospace"
                                        color: Kirigami.Theme.textColor
                                        selectionColor: Kirigami.Theme.highlightColor
                                        selectedTextColor: Kirigami.Theme.highlightedTextColor
                                        text: root.renderLog(backend.logLines)

                                        // A new search replaces the text; the
                                        // view has to go back to the newest
                                        // line with it.
                                        onTextChanged: Qt.callLater(logScroll.toTop)

                                        Controls.Menu {
                                            id: logMenu
                                            Controls.MenuItem {
                                                // TRANSLATORS: right-click menu item, copies the selected log text
                                                text: backend.i18n("Copy")
                                                enabled: logView.selectedText.length > 0
                                                onTriggered: logView.copy()
                                            }
                                            Controls.MenuItem {
                                                // TRANSLATORS: right-click menu item, selects the whole log view
                                                text: backend.i18n("Select all")
                                                onTriggered: logView.selectAll()
                                            }
                                        }

                                        TapHandler {
                                            acceptedButtons: Qt.RightButton
                                            onTapped: logMenu.popup()
                                        }
                                    }
                                }

                                Kirigami.PlaceholderMessage {
                                    anchors.centerIn: parent
                                    width: parent.width - Kirigami.Units.gridUnit * 4
                                    visible: backend.logLines.length === 0
                                    icon.name: search.text.length > 0 ? "edit-none" : "view-history"
                                    text: search.text.length > 0 ? backend.i18n("No lines match") : backend.i18n("Nothing logged yet")
                                    explanation: search.text.length > 0
                                                 ? backend.i18n("No line since the sync daemon started contains “{term}”.").replace("{term}", search.text)
                                                 : backend.i18n("This shows what has happened since the sync daemon started.")
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

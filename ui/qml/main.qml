// The account and log window. Plain strings rather than i18n(): that call comes
// from KDE's localization context, which a cxx-qt engine does not install.
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
    width: Kirigami.Units.gridUnit * 44
    height: Kirigami.Units.gridUnit * 34
    minimumWidth: Kirigami.Units.gridUnit * 30
    minimumHeight: Kirigami.Units.gridUnit * 24

    // The tab bar is this window's header; Kirigami's own page header would
    // only add an empty band above it.
    pageStack.globalToolBar.style: Kirigami.ApplicationHeaderStyle.None

    FolderDialog {
        id: folderDialog
        title: backend.i18n("Choose the Proton Drive folder")
        currentFolder: backend.syncFolder.length > 0 ? "file://" + encodeURI(backend.syncFolder) : ""
        // The dialog hands back a URL; sync wants a plain path.
        onAccepted: root.chooseFolder(decodeURIComponent(selectedFolder.toString().replace(/^file:\/\//, "")))
    }

    FolderDialog {
        id: ingestDialog
        title: backend.i18n("Choose the folder to upload photos from")
        currentFolder: backend.ingestFolder.length > 0 ? "file://" + encodeURI(backend.ingestFolder) : ""
        onAccepted: backend.changeIngestFolder(decodeURIComponent(selectedFolder.toString().replace(/^file:\/\//, "")))
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

    // Qt asks the compositor for an activation token before handing the URL
    // over, which is what lets the browser raise itself on Wayland. Launching
    // xdg-open directly cannot, so the tab opens behind this window.
    function open(url, what) {
        if (Qt.openUrlExternally(url))
            backend.status = backend.i18n("Opened {what}").replace("{what}", what);
        else
            backend.status = backend.i18n("Could not open {what} ({url})").replace("{what}", what).replace("{url}", url);
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

        ColumnLayout {
            anchors.fill: parent
            spacing: 0

            Controls.TabBar {
                id: tabs
                Layout.fillWidth: true

                // TRANSLATORS: tab label, the page showing the signed-in account
                Controls.TabButton { text: backend.i18n("Account") }
                // TRANSLATORS: tab label, the page showing the activity log
                Controls.TabButton { text: backend.i18n("Logs") }
            }

            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: tabs.currentIndex

                // ---- Account ------------------------------------------------
                Item {
                ColumnLayout {
                    id: accountPage
                    Timer {
                        running: true; interval: 2500
                        onTriggered: accountPage.grabToImage(function(r) { console.warn("PROBE saved=" + r.saveToFile("/tmp/claude-1000/-home-anelis-Projects-kpdrive/0909956e-d5a7-4976-96c5-469d585f9558/scratchpad/ar-account.png")); },
                                                             Qt.size(accountPage.width * 2, accountPage.height * 2))
                    }
                    anchors.fill: parent
                    anchors.margins: Kirigami.Units.gridUnit
                    spacing: Kirigami.Units.largeSpacing

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

                    ColumnLayout {
                        Layout.fillWidth: true
                        Layout.topMargin: Kirigami.Units.largeSpacing
                        visible: backend.loggedIn
                        spacing: Kirigami.Units.smallSpacing

                        Controls.Label {
                            // TRANSLATORS: heading above the bar showing how much of the account is used
                            text: backend.i18n("Storage")
                            font.bold: true
                        }

                        // Drawn rather than Controls.ProgressBar: Breeze's version
                        // drives an internal Repeater off its own width and
                        // throws "cannot read property of null" in a loop when
                        // that width is constrained. A bar is two rectangles.
                        Rectangle {
                            Layout.fillWidth: true
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

                        RowLayout {
                            Layout.fillWidth: true
                            Layout.topMargin: Kirigami.Units.largeSpacing
                            spacing: Kirigami.Units.smallSpacing

                            // TRANSLATORS: label before the path of the sync folder, as in "Syncing to /home/you/ProtonDrive"
                            Controls.Label { text: backend.i18n("Syncing to"); opacity: 0.7 }

                            Controls.Label {
                                Layout.fillWidth: true
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

                            Controls.Button {
                                // TRANSLATORS: button that opens .protonignore, the list of paths sync leaves alone
                                text: backend.i18n("Ignore file…")
                                icon.name: "document-edit"
                                onClicked: backend.openIgnoreFile()
                            }
                        }
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 0

                        Controls.CheckBox {
                            text: backend.i18n("Also download Proton Photos")
                            checked: backend.syncPhotos
                            onToggled: backend.changeSyncPhotos(checked)
                        }

                        Controls.Label {
                            Layout.fillWidth: true
                            Layout.leftMargin: Kirigami.Units.gridUnit * 2
                            text: backend.i18n("Your timeline is copied into {folder}, checked every half hour. Nothing in Proton Photos is changed or deleted by it.")
                                  .replace("{folder}", backend.photosFolder.length > 0 ? backend.photosFolder : backend.i18n("your Pictures folder"))
                            wrapMode: Text.WordWrap
                            opacity: 0.7
                            font: Kirigami.Theme.smallFont
                        }
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 0

                        Controls.CheckBox {
                            text: backend.i18n("Upload photos from a folder")
                            checked: backend.ingestFolder.length > 0
                            // Turning it on means picking the folder; the box
                            // follows the setting, not the click.
                            onToggled: {
                                checked = Qt.binding(function() { return backend.ingestFolder.length > 0 })
                                if (backend.ingestFolder.length > 0) {
                                    backend.changeIngestFolder("")
                                } else {
                                    ingestDialog.open()
                                }
                            }
                        }

                        RowLayout {
                            Layout.fillWidth: true
                            Layout.leftMargin: Kirigami.Units.gridUnit * 2
                            visible: backend.ingestFolder.length > 0

                            Controls.Label {
                                Layout.fillWidth: true
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
                            Layout.leftMargin: Kirigami.Units.gridUnit * 2
                            visible: backend.ingestFolder.length > 0
                            text: backend.i18n("Delete uploaded photos instead of moving them to the trash")
                            checked: backend.ingestPermRm
                            onToggled: backend.changeIngestPermRm(checked)
                        }

                        Controls.Label {
                            Layout.fillWidth: true
                            Layout.leftMargin: Kirigami.Units.gridUnit * 2
                            text: backend.i18n("Photos and videos put in this folder go up to Proton Photos within a minute, then leave the folder. Anything that cannot be uploaded stays.")
                            wrapMode: Text.WordWrap
                            opacity: 0.7
                            font: Kirigami.Theme.smallFont
                        }
                    }

                    // What the sync daemon is doing. It is a separate process,
                    // so without this the window can only guess.
                    Kirigami.InlineMessage {
                        Layout.fillWidth: true
                        visible: backend.syncStatus.length > 0
                        position: Kirigami.InlineMessage.Position.Inline
                        type: backend.syncFailed ? Kirigami.MessageType.Error
                            : backend.syncOffline || !backend.daemonRunning ? Kirigami.MessageType.Warning
                            : Kirigami.MessageType.Information
                        text: backend.syncStatus
                        actions: [
                            Kirigami.Action {
                                text: backend.i18n("Sync now")
                                icon.name: "view-refresh"
                                visible: backend.daemonRunning && !backend.syncBusy
                                onTriggered: backend.syncNow()
                            }
                        ]
                    }

                    // The daemon has no way to push, so ask it now and then.
                    Timer {
                        interval: 3000
                        running: root.visible
                        repeat: true
                        triggeredOnStart: true
                        onTriggered: backend.refreshSyncStatus()
                    }

                    Flow {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing

                        Controls.Button {
                            text: backend.i18n("Open folder")
                            icon.name: "folder-open"
                            enabled: backend.syncFolder.length > 0
                            onClicked: root.open("file://" + encodeURI(backend.syncFolder), "the sync folder")
                        }
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
                        Controls.Button {
                            // TRANSLATORS: button that reloads the account details from Proton
                            text: backend.i18n("Refresh")
                            icon.name: "view-refresh"
                            enabled: backend.loggedIn && !backend.busy
                            onClicked: backend.refresh()
                        }
                        Controls.Button {
                            text: backend.loggedIn ? backend.i18n("Sign out") : backend.i18n("Sign in")
                            icon.name: backend.loggedIn ? "system-log-out" : "key-enter"
                            enabled: !backend.busy
                            onClicked: backend.loggedIn ? backend.logout() : backend.login()
                        }
                    }

                    Kirigami.InlineMessage {
                        Layout.fillWidth: true
                        visible: backend.status.length > 0
                        text: backend.status
                        type: Kirigami.MessageType.Information
                    }

                    // Clears itself rather than carrying a close button: the
                    // button would assign visible and break the binding above.
                    Timer {
                        running: backend.status.length > 0
                        interval: 8000
                        onTriggered: backend.status = ""
                    }

                    Item { Layout.fillHeight: true }

                    Kirigami.Separator { Layout.fillWidth: true }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing

                        Controls.Label {
                            text: "kpdrive " + backend.version
                            opacity: 0.7
                        }

                        Item { Layout.fillWidth: true }

                        Controls.Label {
                            text: backend.license
                            opacity: 0.7
                        }

                        Controls.Button {
                            text: backend.i18n("Read the licence")
                            flat: true
                            onClicked: root.open("https://www.gnu.org/licenses/gpl-3.0.html", "the licence")
                        }
                    }
                }
                }

                // ---- Logs ---------------------------------------------------
                Item {
                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: Kirigami.Units.largeSpacing
                    spacing: Kirigami.Units.smallSpacing

                    Kirigami.SearchField {
                        id: search
                        Layout.fillWidth: true
                        placeholderText: backend.i18n("Search the log")
                        onTextChanged: backend.searchLogs(text)
                    }

                    Item {
                        Layout.fillWidth: true
                        Layout.fillHeight: true

                        Controls.ScrollView {
                            id: logScroll
                            anchors.fill: parent
                            clip: true
                            visible: backend.logLines.length > 0

                            // Newest first, so what just happened is in view
                            // without scrolling.
                            function toTop() {
                                logScroll.contentItem.contentY = 0;
                            }

                            // The whole log is one laid-out document rather than a
                            // virtualised list, which is what allows a selection to
                            // cross lines. Laying it out costs memory per line, so
                            // the backend hands over only its newest hundred.
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

                                // A new search replaces the text; the view has
                                // to go back to the newest line with it.
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

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing

                        // TRANSLATORS: start of "Keep logs for [30] days"; the number and "days" follow
                        Controls.Label { text: backend.i18n("Keep logs for") }

                        Controls.SpinBox {
                            from: 1
                            to: 3650
                            value: backend.retentionDays
                            onValueModified: backend.setRetention(value)
                        }

                        // TRANSLATORS: end of "Keep logs for [30] days"
                        Controls.Label { text: backend.i18n("days") }

                        Item { width: Kirigami.Units.largeSpacing }

                        // TRANSLATORS: verb, label before a dropdown choosing which log levels are written to disk
                        Controls.Label { text: backend.i18n("Store") }

                        Controls.ComboBox {
                            id: levelBox
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

                        Item { Layout.fillWidth: true }

                        Controls.Label {
                            text: backend.i18np("{n} line", "{n} lines", backend.logLines.length).replace("{n}", backend.logLines.length)
                            opacity: 0.7
                        }
                    }
                }
                }
            }
        }
    }
}

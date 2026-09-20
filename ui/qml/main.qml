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
        title: "Choose the Proton Drive folder"
        currentFolder: backend.syncFolder.length > 0 ? "file://" + encodeURI(backend.syncFolder) : ""
        // The dialog hands back a URL; sync wants a plain path.
        onAccepted: root.chooseFolder(decodeURIComponent(selectedFolder.toString().replace(/^file:\/\//, "")))
    }

    // The same question the CLI asks, with the same two answers.
    KirigamiDialogs.PromptDialog {
        id: occupiedPrompt

        property string folder: ""

        title: "The folder is not empty"
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
            backend.status = "Opened " + what;
        else
            backend.status = "Could not open " + what + " (" + url + ")";
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
            return gib.toFixed(1) + " GiB";
        return (bytes / (1024 * 1024)).toFixed(1) + " MiB";
    }

    pageStack.initialPage: Kirigami.Page {
        padding: 0

        ColumnLayout {
            anchors.fill: parent
            spacing: 0

            Controls.TabBar {
                id: tabs
                Layout.fillWidth: true

                Controls.TabButton { text: "Account" }
                Controls.TabButton { text: "Logs" }
            }

            StackLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                currentIndex: tabs.currentIndex

                // ---- Account ------------------------------------------------
                Item {
                ColumnLayout {
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
                                text: backend.loggedIn ? backend.username : "Not signed in"
                                elide: Text.ElideRight
                                Layout.fillWidth: true
                            }

                            Controls.Label {
                                text: backend.loggedIn
                                      ? "Signed in to Proton Drive"
                                      : "Sign in to sync this computer with Proton Drive."
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
                            text: "Storage"
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
                            text: root.formatSize(backend.usedBytes) + " of "
                                  + root.formatSize(backend.totalBytes) + " used"
                            opacity: 0.7
                        }

                        RowLayout {
                            Layout.fillWidth: true
                            Layout.topMargin: Kirigami.Units.largeSpacing
                            spacing: Kirigami.Units.smallSpacing

                            Controls.Label { text: "Syncing to"; opacity: 0.7 }

                            Controls.Label {
                                Layout.fillWidth: true
                                text: backend.syncFolder.length > 0 ? backend.syncFolder : "nowhere yet"
                                elide: Text.ElideMiddle
                                font.family: "monospace"
                            }

                            Controls.Button {
                                text: "Change…"
                                icon.name: "folder-sync"
                                onClicked: folderDialog.open()
                            }

                            Controls.Button {
                                text: "Ignore file…"
                                icon.name: "document-edit"
                                onClicked: backend.openIgnoreFile()
                            }
                        }
                    }

                    Flow {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing

                        Controls.Button {
                            text: "Open folder"
                            icon.name: "folder-open"
                            enabled: backend.syncFolder.length > 0
                            onClicked: root.open("file://" + encodeURI(backend.syncFolder), "the sync folder")
                        }
                        Controls.Button {
                            text: "Drive on the web"
                            icon.name: "internet-services"
                            onClicked: root.open("https://drive.proton.me/", "Proton Drive in your browser")
                        }
                        Controls.Button {
                            text: "Proton account"
                            icon.name: "system-users"
                            onClicked: root.open("https://account.proton.me/", "your Proton account in your browser")
                        }
                        Controls.Button {
                            text: "Refresh"
                            icon.name: "view-refresh"
                            enabled: backend.loggedIn && !backend.busy
                            onClicked: backend.refresh()
                        }
                        Controls.Button {
                            text: backend.loggedIn ? "Sign out" : "Sign in"
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
                            text: "Read the licence"
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
                        placeholderText: "Search the log"
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

                            // A log reads like a terminal: newest at the bottom.
                            function toEnd() {
                                const flick = logScroll.contentItem;
                                flick.contentY = Math.max(0, flick.contentHeight - flick.height);
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

                                // The text is laid out after it is set, so the
                                // height to scroll to is only known next tick.
                                onTextChanged: Qt.callLater(logScroll.toEnd)
                                Component.onCompleted: Qt.callLater(logScroll.toEnd)

                                Controls.Menu {
                                    id: logMenu
                                    Controls.MenuItem {
                                        text: "Copy"
                                        enabled: logView.selectedText.length > 0
                                        onTriggered: logView.copy()
                                    }
                                    Controls.MenuItem {
                                        text: "Select all"
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
                            text: search.text.length > 0 ? "No lines match" : "Nothing logged yet"
                            explanation: search.text.length > 0
                                         ? "No log line contains “" + search.text + "”."
                                         : "Sync activity, sign-ins and errors show up here."
                        }
                    }

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Kirigami.Units.smallSpacing

                        Controls.Label { text: "Keep logs for" }

                        Controls.SpinBox {
                            from: 1
                            to: 3650
                            value: backend.retentionDays
                            onValueModified: backend.setRetention(value)
                        }

                        Controls.Label { text: "days" }

                        Item { width: Kirigami.Units.largeSpacing }

                        Controls.Label { text: "Store" }

                        Controls.ComboBox {
                            id: levelBox
                            textRole: "text"
                            valueRole: "value"
                            model: [
                                { text: "Warnings and errors", value: "WARN" },
                                { text: "Everything", value: "INFO" },
                                { text: "Errors only", value: "ERROR" },
                            ]
                            currentIndex: Math.max(0, indexOfValue(backend.logLevel))
                            onActivated: backend.changeLogLevel(currentValue)
                        }

                        Item { Layout.fillWidth: true }

                        Controls.Label {
                            text: backend.logLines.length + " line" + (backend.logLines.length === 1 ? "" : "s")
                            opacity: 0.7
                        }
                    }
                }
                }
            }
        }
    }
}

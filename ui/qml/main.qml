// The account and log window. Plain strings rather than i18n(): that call comes
// from KDE's localization context, which a cxx-qt engine does not install.
import QtQuick
import QtQuick.Controls as Controls
import QtQuick.Layouts
import org.kde.kirigami as Kirigami
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
                            visible: backend.syncFolder.length > 0
                            spacing: Kirigami.Units.smallSpacing

                            Controls.Label { text: "Syncing to"; opacity: 0.7 }
                            Controls.Label {
                                Layout.fillWidth: true
                                text: backend.syncFolder
                                elide: Text.ElideMiddle
                                font.family: "monospace"
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

                    Controls.ScrollView {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        clip: true

                        ListView {
                            id: logView
                            model: backend.logLines
                            reuseItems: true

                            // A log reads like a terminal: newest at the bottom.
                            onCountChanged: positionViewAtEnd()
                            Component.onCompleted: positionViewAtEnd()

                            delegate: Controls.Label {
                                required property string modelData
                                width: ListView.view.width
                                text: modelData
                                font.family: "monospace"
                                wrapMode: Text.WrapAnywhere
                                color: modelData.indexOf(" ERROR ") >= 0 ? Kirigami.Theme.negativeTextColor
                                     : modelData.indexOf(" WARN ") >= 0 ? Kirigami.Theme.neutralTextColor
                                     : Kirigami.Theme.textColor
                            }

                            Kirigami.PlaceholderMessage {
                                anchors.centerIn: parent
                                width: parent.width - Kirigami.Units.gridUnit * 4
                                visible: logView.count === 0
                                icon.name: search.text.length > 0 ? "edit-none" : "view-history"
                                text: search.text.length > 0 ? "No lines match" : "Nothing logged yet"
                                explanation: search.text.length > 0
                                             ? "No log line contains “" + search.text + "”."
                                             : "Sync activity, sign-ins and errors show up here."
                            }
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

                        Item { Layout.fillWidth: true }

                        Controls.Label {
                            text: logView.count + " line" + (logView.count === 1 ? "" : "s")
                            opacity: 0.7
                        }
                    }
                }
                }
            }
        }
    }
}

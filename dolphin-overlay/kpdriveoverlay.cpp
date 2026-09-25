// SPDX-License-Identifier: GPL-3.0-or-later
// Dolphin overlay icons for every kpdrive sync and photos folder. Asks the running daemon
// over its Unix socket; see src/daemon.rs for the protocol.
#include <KOverlayIconPlugin>
#include <QElapsedTimer>
#include <algorithm>
#include <QJsonArray>
#include <QJsonDocument>
#include <QLocalSocket>
#include <QStandardPaths>
#include <QUrl>

class KpdriveOverlay : public KOverlayIconPlugin
{
    Q_OBJECT
    Q_PLUGIN_METADATA(IID "org.kde.overlayicon.kpdrive" FILE "kpdriveoverlay.json")
public:
    explicit KpdriveOverlay(QObject *parent = nullptr)
        : KOverlayIconPlugin(parent)
    {
    }

    QStringList getOverlays(const QUrl &url) override
    {
        if (!url.isLocalFile()) {
            return {};
        }
        const QString path = url.toLocalFile();
        // Accounts come and go while Dolphin stays open, so the list is
        // asked for again now and then.
        if (!m_asked.isValid() || m_asked.elapsed() > 30000) {
            // Restarted first: a daemon that cannot be reached clears it again.
            m_asked.restart();
            m_folders = folders();
        }
        if (!std::any_of(m_folders.cbegin(), m_folders.cend(), [&](const QString &dir) {
                return under(path, dir);
            })) {
            return {};
        }
        const QString reply = ask(QStringLiteral("STATUS ") + path);
        if (reply == QLatin1String("OK")) {
            return {QStringLiteral("vcs-normal")};
        }
        if (reply == QLatin1String("SYNC")) {
            return {QStringLiteral("vcs-update-required")};
        }
        return {};
    }

private:
    // Every folder of every account. A daemon from before accounts answers
    // ERR, and is asked for its one sync folder and photos folder instead.
    QStringList folders()
    {
        QStringList out;
        const QString reply = ask(QStringLiteral("FOLDERS"));
        if (reply.startsWith(QLatin1Char('['))) {
            const QJsonArray list = QJsonDocument::fromJson(reply.toUtf8()).array();
            for (const auto &dir : list) {
                out << dir.toString();
            }
            return out;
        }
        if (reply != QLatin1String("ERR")) {
            return out;
        }
        out << ask(QStringLiteral("ROOT"));
        const QString photos = ask(QStringLiteral("PHOTOS"));
        if (photos != QLatin1String("ERR")) {
            out << photos;
        }
        out.removeAll(QString());
        return out;
    }

    static bool under(const QString &path, const QString &dir)
    {
        return !dir.isEmpty() && (path == dir || path.startsWith(dir + QLatin1Char('/')));
    }

    // One line in, one line out. When the daemon is down, back off so a big
    // directory listing does not pay a connect timeout per file.
    QString ask(const QString &line)
    {
        if (m_sock.state() != QLocalSocket::ConnectedState) {
            if (m_backoff.isValid() && m_backoff.elapsed() < 10000) {
                return {};
            }
            m_sock.connectToServer(QStandardPaths::writableLocation(QStandardPaths::RuntimeLocation) + QStringLiteral("/kpdrive.sock"));
            if (!m_sock.waitForConnected(20)) {
                m_backoff.restart();
                m_asked.invalidate();
                return {};
            }
        }
        m_sock.write((line + QLatin1Char('\n')).toUtf8());
        m_sock.flush();
        while (!m_sock.canReadLine()) {
            if (!m_sock.waitForReadyRead(50)) {
                return {};
            }
        }
        return QString::fromUtf8(m_sock.readLine()).trimmed();
    }

    QLocalSocket m_sock;
    QElapsedTimer m_backoff;
    QElapsedTimer m_asked;
    QStringList m_folders;
};

#include "kpdriveoverlay.moc"

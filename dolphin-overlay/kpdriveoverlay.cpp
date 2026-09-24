// SPDX-License-Identifier: GPL-3.0-or-later
// Dolphin overlay icons for the kpdrive sync and photos folders. Asks the running daemon
// over its Unix socket; see src/daemon.rs for the protocol.
#include <KOverlayIconPlugin>
#include <QElapsedTimer>
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
        if (m_root.isEmpty()) {
            m_root = ask(QStringLiteral("ROOT"));
            // An older daemon answers ERR: no photos folder then.
            m_photos = ask(QStringLiteral("PHOTOS"));
            if (m_photos == QLatin1String("ERR")) {
                m_photos.clear();
            }
        }
        if (!under(path, m_root) && !under(path, m_photos)) {
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
                m_root.clear();
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
    QString m_root;
    QString m_photos;
};

#include "kpdriveoverlay.moc"

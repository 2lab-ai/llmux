#include "qt_runtime.h"

#include <LayerShellQt/Window>
#include <QApplication>
#include <QCoreApplication>
#include <QDirIterator>
#include <QFile>
#include <QGuiApplication>
#include <QObject>
#include <QQmlApplicationEngine>
#include <QQmlEngine>
#include <QQmlError>
#include <QScreen>
#include <QUrl>
#include <QWindow>
#include <QtGlobal>
#include <cstdio>
#include <memory>

#if QT_VERSION < QT_VERSION_CHECK(6, 5, 0)
#include <LayerShellQt/Shell>
#endif

namespace llmux_islands {
namespace {

std::unique_ptr<QApplication> application;
int application_argc = 1;
char application_name[] = "llmux-islands-linux";
char *application_argv[] = {application_name, nullptr};

void position_x11_window(QWindow *window)
{
    window->setFlags(window->flags() | Qt::Tool | Qt::FramelessWindowHint | Qt::WindowStaysOnTopHint);
    QScreen *screen = window->screen();
    if (screen == nullptr) {
        screen = QGuiApplication::primaryScreen();
    }
    if (screen != nullptr) {
        const QRect available = screen->availableGeometry();
        const int x = available.x() + (available.width() - window->width()) / 2;
        window->setPosition(x, available.y() + 8);
    }
}

} // namespace

void initialize_application()
{
    if (QCoreApplication::instance() == nullptr) {
        application = std::make_unique<QApplication>(application_argc, application_argv);
    }
    QApplication::setApplicationName(QStringLiteral("llmux Islands"));
    QApplication::setOrganizationName(QStringLiteral("2lab.ai"));
    QApplication::setOrganizationDomain(QStringLiteral("2lab.ai"));
    QGuiApplication::setDesktopFileName(QStringLiteral("io.twolab.LlmuxIslands"));
    QApplication::setQuitOnLastWindowClosed(false);
}

void prepare_surface(const QString &mode)
{
#if QT_VERSION < QT_VERSION_CHECK(6, 5, 0)
    if (mode == QStringLiteral("wayland-layer-shell")) {
        LayerShellQt::Shell::useLayerShell();
    }
#else
    Q_UNUSED(mode)
#endif
}

void load_application(QQmlApplicationEngine &engine)
{
    QObject::connect(&engine,
                     &QQmlEngine::warnings,
                     &engine,
                     [](const QList<QQmlError> &warnings) {
                         for (const auto &warning : warnings) {
                             const auto message = warning.toString().toUtf8();
                             std::fprintf(stderr, "QML warning: %s\n", message.constData());
                         }
                     });
    engine.load(QUrl(QStringLiteral(
        "qrc:/qt/qml/io/twolab/LlmuxIslands/qml/Main.qml")));
}

bool configure_surface(QQmlApplicationEngine &engine, const QString &mode)
{
    const auto roots = engine.rootObjects();
    if (roots.isEmpty()) {
        const auto expected = QStringLiteral(":/qt/qml/io/twolab/LlmuxIslands/qml/Main.qml");
        std::fprintf(stderr,
                     "QML engine created no root object; expected resource present: %s\n",
                     QFile::exists(expected) ? "yes" : "no");
        QDirIterator resources(QStringLiteral(":/qt/qml/io/twolab/LlmuxIslands"),
                               QDirIterator::Subdirectories);
        while (resources.hasNext()) {
            const auto path = resources.next().toUtf8();
            std::fprintf(stderr, "QML resource: %s\n", path.constData());
        }
        return false;
    }

    auto *window = qobject_cast<QWindow *>(roots.constFirst());
    if (window == nullptr) {
        std::fprintf(stderr,
                     "QML root object is not a QWindow: %s\n",
                     roots.constFirst()->metaObject()->className());
        return false;
    }

    if (mode == QStringLiteral("wayland-layer-shell")) {
        auto *surface = LayerShellQt::Window::get(window);
        surface->setAnchors(LayerShellQt::Window::AnchorTop);
        surface->setLayer(LayerShellQt::Window::LayerOverlay);
        surface->setExclusiveZone(-1);
        surface->setKeyboardInteractivity(LayerShellQt::Window::KeyboardInteractivityOnDemand);
        surface->setScope(QStringLiteral("llmux-islands"));
    } else if (mode == QStringLiteral("x11-positioned")) {
        position_x11_window(window);
    } else {
        window->setProperty("llmuxSurfaceMode", QStringLiteral("regular-window"));
    }

    // QML may expose the compact layer-shell surface only after this point.
    // Showing it during engine.load() would commit a normal Wayland surface
    // before LayerShellQt has attached its role.
    window->setProperty("surfaceConfigured", true);

    // QML owns visibility after the native surface role is fully configured.
    return true;
}

int exec_application()
{
    return application == nullptr ? 1 : application->exec();
}

} // namespace llmux_islands

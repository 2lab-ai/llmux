# llmux Islands for Linux

Native Qt 6/QML/Kirigami shell for Arch Linux and KDE Plasma. It uses the
same Rust semantic state, reducer, daemon protocol, privacy rules, and receipt
projection as the cross-platform `llmux-islands-core`; QML only renders state
and dispatches semantic actions.

The live application starts as a compact closed island on Plasma Wayland and
tray-only on other supported desktops, fetches the real daemon dashboard, and
never hydrates production state from its checked-in test fixture.

## Build and run on Arch Linux

From the repository root:

```sh
sudo pacman -S --needed base-devel cargo clang gtk3 kirigami layer-shell-qt \
  libcanberra libnotify qqc2-desktop-style qt6-base qt6-declarative qt6-svg \
  qt6-tools qt6-wayland rust
cd llmux-islands-linux
QMAKE=/usr/bin/qmake6 cargo run --locked
```

The application uses `$XDG_CONFIG_HOME/llmux/islands.json` (or
`$HOME/.config/llmux/islands.json`) with directory mode `0700` and file mode
`0600`. Loopback HTTP is allowed. Remote endpoints require HTTPS and an API
key for requests. The connection editor exposes explicit keep, replace, and
clear semantics; clearing a remote key persists an honestly unauthenticated
configuration until a replacement is set. A stored credential is never
projected back into QML or semantic state; newly entered secret fields are
cleared immediately after dispatch.

The clean Arch build used by CI must use the repository root as its Docker
context because the Linux crate depends on the sibling core and daemon crates:

```sh
docker build --platform linux/amd64 \
  -f llmux-islands-linux/packaging/arch/Dockerfile \
  -t llmux-islands-linux-port .
```

The bounded offscreen startup receipt is:

```sh
cd llmux-islands-linux
QMAKE=/usr/bin/qmake6 QT_QPA_PLATFORM=offscreen \
  QT_QUICK_BACKEND=software QSG_RHI_BACKEND=software \
  cargo run --locked -- --smoke-test
```

Generate deterministic fixture-backed PNG receipts without contacting a live
daemon:

```sh
cd llmux-islands-linux
QMAKE=/usr/bin/qmake6 cargo run --locked -- \
  --snapshot-dir /tmp/llmux-islands-snapshots
```

This explicit mode forces Qt's offscreen software renderer and writes
`usage.png`, `statistics.png`, and `menu.png`. Directory creation, fixture
hydration, capture, and file-save failures return a nonzero process status.
Normal startup never loads the fixture: it starts empty, briefly opens the
boot island, and closes it after one second only when a tray is available.

`packaging/arch/PKGBUILD` builds the release binary and installs its desktop
entry, AppStream metadata, SVG icon, and license. Package-owned installs never
self-overwrite or invoke `sudo`, `pacman`, or an AUR helper from the app; the
maintenance receipt instead gives the exact package-manager instruction.
The clean-container CI runs this PKGBUILD as an unprivileged `makepkg` user,
installs the resulting package, and smoke-launches the installed binary.

## KDE platform mapping

- The tray uses `Qt.labs.platform.SystemTrayIcon`, backed by Qt's
  StatusNotifierItem integration on Plasma. Its menu provides Show/Hide,
  Refresh, and Quit.
- Plasma/KDE Wayland sessions identified conservatively from XDG session and
  desktop capabilities use LayerShellQt as a top overlay with on-demand
  keyboard focus. Other Wayland sessions use the regular-window fallback;
  Wayland deliberately avoids unsupported absolute coordinates.
- X11 uses a frameless, always-on-top tool window positioned at the top center
  of the selected `QScreen`.
- Offscreen and unknown platforms use an honest regular-window fallback.
- Screens come from `QGuiApplication::screens()` and the stored selection is
  applied to the native window; sound choices map to freedesktop sound-theme
  events.
- Notifications use Qt's native tray message API so activation can reopen the
  semantic island; optional sounds use `canberra-gtk-play`.
- The portable Qt tray API has no StatusNotifierItem attention-status setter,
  so provider grouping, total in-flight work, and connection/account health
  are exposed in its tooltip and menu, with the desktop theme's warning icon
  used when attention is required.
- XDG autostart is atomic and idempotent, rejects symlink paths, and is read
  back before the UI reports success.
- A sibling `llmux server --port <selected-port> --no-tui` may start only for
  an HTTP loopback endpoint after `/llmux/status` probing. Remote endpoints
  never spawn it.

Relevant upstream contracts:

- [Kirigami](https://develop.kde.org/docs/getting-started/kirigami/)
- [CXX-Qt](https://kdab.github.io/cxx-qt/book/)
- [LayerShellQt](https://api.kde.org/legacy/plasma/layer-shell-qt/html/dir_f3eec1e9e98e02e34c8efeb863b66c5f.html)
- [Qt SystemTrayIcon](https://doc.qt.io/qt-6/qml-qt-labs-platform-systemtrayicon.html)

## Verification

Fast platform-independent gates:

```sh
cargo fmt --all -- --check
cargo clippy --no-default-features --all-targets --locked -- -D warnings
cargo test --no-default-features --locked
```

The Arch container additionally compiles the CXX-Qt GUI, validates the desktop
and AppStream files, lints all four QML surfaces, runs the Linux tests, executes
the bounded offscreen smoke receipt, builds and installs the real Arch package,
and verifies that the snapshot CLI writes three distinct 960x760 PNGs from the
statically embedded QML module. CI uploads those images and their `SHA256SUMS`
manifest as `llmux-islands-kde-snapshots`.

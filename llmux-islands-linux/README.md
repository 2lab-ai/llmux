# llmux Islands for Linux

Native Qt 6/QML/Kirigami companion for Arch Linux and KDE Plasma. It shares the
Rust semantic state, reducer, daemon protocol, privacy projection, and receipts
with the macOS app; this crate owns only the Linux/KDE presentation and native
platform effects.

User-facing behavior and screenshots: [llmux Islands](../docs/llmux-islands.md).

## Install on Arch Linux

The Linux companion is not currently published to pacman, the AUR, xbrew, or
GitHub Releases. Install the repository `PKGBUILD`:

```bash
sudo pacman -S --needed base-devel git
git clone --depth=1 https://github.com/2lab-ai/llmux.git
cd llmux/llmux-islands-linux/packaging/arch
CARGO_BUILD_JOBS=2 MAKEFLAGS=-j2 makepkg -si
```

The resulting package is named `llmux-islands-git`, provides/conflicts with
`llmux-islands`, and installs:

- `/usr/bin/llmux-islands-linux`
- a desktop entry and AppStream metadata
- the scalable application icon
- the MIT license

Launch from the desktop menu or:

```bash
llmux-islands-linux
```

`CARGO_BUILD_JOBS=2` and `MAKEFLAGS=-j2` bound local build parallelism. The app
does not self-overwrite or invoke `sudo`, pacman, or an AUR helper.

## Runtime configuration and security

The application stores connection/presentation settings at
`$XDG_CONFIG_HOME/llmux/islands.json` or `~/.config/llmux/islands.json`, with
directory mode `0700` and file mode `0600`.

- Loopback HTTP is allowed.
- Remote endpoints require HTTPS and an API key.
- Redirects are denied.
- Clearing a remote key persists an honestly unauthenticated state until a new
  key is supplied.
- Stored credentials are never projected back into QML or semantic state;
  newly entered secret fields are cleared after dispatch.

Normal startup fetches the live daemon dashboard. It never hydrates production
state from the checked-in test fixture. A missing local daemon may be started
only when the selected endpoint is HTTP loopback; remote endpoints never spawn
one.

## KDE platform mapping

- Plasma Wayland sessions use LayerShellQt for the compact top overlay and
  on-demand keyboard focus.
- Other Wayland sessions use a regular-window fallback rather than unsupported
  absolute positioning.
- X11 uses a frameless, always-on-top tool window centered on the selected
  `QScreen`.
- Offscreen and unknown platforms use an honest regular-window fallback.
- `Qt.labs.platform.SystemTrayIcon` uses Plasma's StatusNotifierItem backend;
  its menu provides Show/Hide, Refresh, and Quit.
- The portable tray API has no attention-status setter, so health and activity
  appear in the tooltip/menu and warning icon.
- Native tray messages provide notifications; optional sounds use
  `canberra-gtk-play` and freedesktop sound-theme events.
- XDG autostart writes atomically, rejects symlink paths, and is read back
  before success is reported.

Relevant upstream contracts:

- [Kirigami](https://develop.kde.org/docs/getting-started/kirigami/)
- [CXX-Qt](https://kdab.github.io/cxx-qt/book/)
- [LayerShellQt](https://api.kde.org/legacy/plasma/layer-shell-qt/html/dir_f3eec1e9e98e02e34c8efeb863b66c5f.html)
- [Qt SystemTrayIcon](https://doc.qt.io/qt-6/qml-qt-labs-platform-systemtrayicon.html)

## Run from source

Install the same dependencies listed by
[`packaging/arch/PKGBUILD`](packaging/arch/PKGBUILD), then from this directory:

```bash
CARGO_BUILD_JOBS=2 QMAKE=/usr/bin/qmake6 cargo run --locked
```

## Fast verification

Platform-independent gates avoid compiling the Qt shell:

```bash
CARGO_BUILD_JOBS=2 cargo fmt --all -- --check
CARGO_BUILD_JOBS=2 cargo clippy --no-default-features --all-targets --locked -- -D warnings
CARGO_BUILD_JOBS=2 cargo test --no-default-features --locked
```

Bounded offscreen startup:

```bash
CARGO_BUILD_JOBS=2 QMAKE=/usr/bin/qmake6 \
QT_QPA_PLATFORM=offscreen QT_QUICK_BACKEND=software QSG_RHI_BACKEND=software \
cargo run --locked -- --smoke-test
```

Deterministic fixture-backed screenshots, without contacting a daemon:

```bash
CARGO_BUILD_JOBS=2 QMAKE=/usr/bin/qmake6 cargo run --locked -- \
  --snapshot-dir /tmp/llmux-islands-snapshots
```

Snapshot mode writes distinct Usage, Statistics, and Menu images through the
production renderer. Fixture load, capture, and save errors return nonzero
status. Receipt screenshots in the durable gallery come from the broader
parity-evidence path.

## Clean-Arch CI (maintainers)

GitHub Actions builds the repository-root Docker context with
`packaging/arch/Dockerfile`. That job compiles the CXX-Qt shell, validates
desktop/AppStream/QML files, runs tests and the offscreen smoke receipt, builds
and installs the real package as an unprivileged `makepkg` user, and uploads
deterministic PNG evidence plus `SHA256SUMS`.

Docker is a CI reproduction path, not the normal local install path. It can be
CPU-intensive; use the `PKGBUILD` flow above for normal Arch installation and
do not run the container build merely to launch the app.

When a maintainer specifically needs the CI-equivalent build, the repository
root must be the context because this crate depends on sibling crates:

```bash
docker build --platform linux/amd64 \
  -f llmux-islands-linux/packaging/arch/Dockerfile \
  -t llmux-islands-linux-port .
```

The durable cross-platform inventory, design rules, specification, traces, and
visual provenance live in the
[Linux port dossier](../.prd/docs/llmux-islands-linux-port/README.md).

FROM archlinux:base-devel AS verify

RUN sed -i 's/^#DisableSandbox/DisableSandbox/' /etc/pacman.conf

RUN pacman -Syu --noconfirm \
    appstream \
    cargo \
    clang \
    desktop-file-utils \
    git \
    gtk3 \
    kirigami \
    layer-shell-qt \
    libcanberra \
    libnotify \
    mesa \
    qqc2-desktop-style \
    qt6-base \
    qt6-declarative \
    qt6-svg \
    qt6-tools \
    qt6-wayland \
    rust \
    && pacman -Scc --noconfirm

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY LICENSE ./LICENSE
COPY src ./src
COPY llmux-islands-core ./llmux-islands-core
COPY llmux-islands-linux ./llmux-islands-linux

WORKDIR /src/llmux-islands-linux

ENV QMAKE=/usr/bin/qmake6
ENV QT_QPA_PLATFORM=offscreen
ENV QT_QUICK_BACKEND=software
ENV QSG_RHI_BACKEND=software

RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/src/llmux-islands-linux/target \
    cargo fmt --all -- --check \
    && cargo clippy --no-default-features --all-targets --locked -- -D warnings \
    && cargo test --no-default-features --locked
RUN bash -n packaging/arch/PKGBUILD \
    && desktop-file-validate resources/io.twolab.LlmuxIslands.desktop \
    && appstreamcli validate --no-net resources/io.twolab.LlmuxIslands.metainfo.xml
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/src/llmux-islands-linux/target \
    cargo build --locked
RUN /usr/lib/qt6/bin/qmllint \
    qml/Main.qml qml/Usage.qml qml/Statistics.qml qml/Menu.qml
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/src/llmux-islands-linux/target \
    timeout 20s ./target/debug/llmux-islands-linux --smoke-test
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/src/llmux-islands-linux/target \
    snapshot_status=0 \
    && snapshot_log="$(timeout 40s ./target/debug/llmux-islands-linux --snapshot-dir /tmp/llmux-islands-snapshots 2>&1)" \
       || snapshot_status="$?" \
    && printf 'snapshot process exit: %s\n%s\n' "$snapshot_status" "$snapshot_log" \
    && test "$snapshot_status" -eq 0 \
    && if printf '%s\n' "$snapshot_log" | grep -Fq 'QML warning:'; then exit 1; fi \
    && hashes='' \
    && for surface in usage statistics menu; do \
         file="/tmp/llmux-islands-snapshots/$surface.png"; \
         test -s "$file"; \
         size="$(wc -c < "$file")"; \
         test "$size" -gt 20000; \
         signature="$(od -An -tx1 -N8 "$file" | tr -d ' \n')"; \
         test "$signature" = "89504e470d0a1a0a"; \
         dimensions="$(od -An -tx1 -j16 -N8 "$file" | tr -d ' \n')"; \
         case "$dimensions" in 000003c0*) ;; *) exit 1;; esac; \
         hash="$(sha256sum "$file" | cut -d ' ' -f 1)"; \
         case " $hashes " in *" $hash "*) exit 1;; esac; \
         hashes="$hashes $hash"; \
       done \
    && receipt="/tmp/llmux-islands-snapshots/receipts.png" \
    && test -s "$receipt" \
    && test "$(wc -c < "$receipt")" -gt 8000 \
    && test "$(od -An -tx1 -N8 "$receipt" | tr -d ' \n')" = "89504e470d0a1a0a" \
    && receipt_hash="$(sha256sum "$receipt" | cut -d ' ' -f 1)" \
    && case " $hashes " in *" $receipt_hash "*) exit 1;; esac \
    && cd /tmp/llmux-islands-snapshots \
    && sha256sum usage.png statistics.png receipts.png menu.png > SHA256SUMS

# Exercise the real PKGBUILD against this exact checkout. makepkg refuses to
# run as root, so commit the minimal Docker context and build as an unprivileged
# user before installing the resulting package back into the verification image.
WORKDIR /src
RUN git init \
    && git add Cargo.toml Cargo.lock LICENSE src llmux-islands-core llmux-islands-linux \
    && git -c user.name='llmux CI' -c user.email='ci@invalid.example' \
       commit -m 'package fixture'
RUN useradd --uid 1000 --user-group --create-home builder \
    && chown -R builder:builder /src \
    && install -d -o builder -g builder /home/builder/package
USER builder
WORKDIR /home/builder/package
RUN --mount=type=cache,target=/home/builder/.cargo/registry,uid=1000,gid=1000 \
    --mount=type=cache,target=/home/builder/.cargo/git,uid=1000,gid=1000 \
    cp /src/llmux-islands-linux/packaging/arch/PKGBUILD . \
    && LLMUX_SOURCE_URL=file:///src makepkg --cleanbuild --force --noconfirm
USER root
RUN pacman -U --noconfirm /home/builder/package/llmux-islands-git-*.pkg.tar.zst \
    && timeout 20s /usr/bin/llmux-islands-linux --smoke-test

FROM scratch AS evidence
COPY --from=verify /tmp/llmux-islands-snapshots /snapshots

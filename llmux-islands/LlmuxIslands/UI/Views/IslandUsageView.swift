import SwiftUI

/// The `.usage` content of the floating island: the lifted agent-island tile
/// grid fed from llmux, plus add (Claude / Codex subscription, API key) and
/// remove. Mirrors llmux's `a → n` add-account flow via the daemon OAuth API.
struct IslandUsageView: View {
    @ObservedObject var model: IslandUsageModel
    @ObservedObject var viewModel: NotchViewModel

    var snapshotAdding = false
    var snapshotProject: URL? = nil
    @State private var adding = false
    @State private var now = Date()
    private let clock = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    private var columns: [GridItem] {
        [
            GridItem(.flexible(minimum: 150), spacing: 10),
            GridItem(.flexible(minimum: 150), spacing: 10),
        ]
    }

    private var effectiveAdding: Bool { adding || snapshotAdding }

    private var loginInProgress: Bool {
        guard let phase = model.login?.phase else { return false }
        return phase == "starting" || phase == "pending" || phase == "cancelling"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            content
        }
        .onReceive(clock) { now = $0 }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Text(model.tiles.isEmpty ? "Get started" : "Your AI workspace")
                .font(.system(size: 15, weight: .semibold))
                .foregroundColor(.white)
            connectionBadge
            Spacer()
            Button(effectiveAdding ? "Done" : "Connect account") { adding.toggle() }
                .font(.system(size: 11, weight: .medium))
                .disabled(loginInProgress)
                .help("Connect an existing Claude, ChatGPT, Grok or API account")
            iconButton("arrow.clockwise") { Task { await model.refresh() } }
                .accessibilityLabel("Refresh accounts")
        }
        .padding(.horizontal, 2)
    }

    @ViewBuilder private var connectionBadge: some View {
        switch model.connection {
        case .connecting: badge(.white.opacity(0.4), "connecting…")
        case .online: badge(TerminalColors.green, model.tiles.isEmpty ? "Connected" : "\(model.tiles.count) accounts")
        case .offline: badge(TerminalColors.red, "offline")
        }
    }

    private func badge(_ color: Color, _ text: String) -> some View {
        HStack(spacing: 5) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text(text)
                .font(.system(size: 10, design: .monospaced))
                .foregroundColor(.white.opacity(0.5))
        }
    }

    @ViewBuilder private var content: some View {
        if effectiveAdding {
            AddAccountInline(model: model, onDone: { adding = false })
        } else if let login = model.login {
            LoginProgressView(login: login, model: model)
        } else if case .connecting = model.connection, model.tiles.isEmpty {
            VStack(spacing: 14) {
                ProgressView().controlSize(.small)
                Text("Getting your workspace ready").font(.system(size: 17, weight: .semibold))
                Text("Connecting to llmux. A local daemon starts automatically.")
                    .font(.system(size: 12)).foregroundStyle(.white.opacity(0.6))
            }.frame(maxWidth: .infinity).padding(.vertical, 38)
        } else if case .offline(let reason) = model.connection {
            VStack(spacing: 14) {
                stateMessage(icon: "link.badge.plus", title: "Let’s reconnect llmux",
                             detail: reason, tint: TerminalColors.red.opacity(0.85))
                HStack {
                    Button("Retry") { Task { await model.retryConnection() } }.buttonStyle(.borderedProminent)
                    Button("Connection settings") { viewModel.showConnectionSettings() }
                    Button("Copy update command") {
                        guard !SnapshotMode.isActive else { return }
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(IslandsRecovery.command(
                            appVersion: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "",
                            llmuxInstalled: InstalledTools.find("llmux") != nil), forType: .string)
                    }
                }.font(.system(size: 11))
            }.padding(.bottom, 20)
        } else if model.tiles.isEmpty {
            VStack(alignment: .leading, spacing: 18) {
                Text("One place for your AI accounts.").font(.system(size: 22, weight: .semibold))
                Text("Connect an account you already use. Then choose a project and open Claude Code or Codex.")
                    .font(.system(size: 13)).foregroundStyle(.white.opacity(0.65)).fixedSize(horizontal: false, vertical: true)
                HStack(spacing: 18) {
                    Label("Connect account", systemImage: "1.circle.fill")
                    Label("Choose project", systemImage: "2.circle")
                    Label("Start coding", systemImage: "3.circle")
                }.font(.system(size: 11)).foregroundStyle(.white.opacity(0.65))
                Button("Connect your first account") { adding = true }
                    .buttonStyle(.borderedProminent).tint(TerminalColors.green)
                Text("Use the accounts you already have.")
                    .font(.system(size: 11)).foregroundStyle(.white.opacity(0.45))
            }.frame(maxWidth: .infinity, alignment: .leading).padding(18)
                .background(RoundedRectangle(cornerRadius: 16).fill(Color.white.opacity(0.055)))
        } else {
            ScrollView(.vertical, showsIndicators: false) {
                StartCodingView(isOnline: model.connection == .online, fixtureProject: snapshotProject)
                    .padding(.bottom, 12)
                if !model.attention.isEmpty {
                    NeedsAttentionSection(items: model.attention)
                        .padding(.bottom, 8)
                }
                UsageAccountTileGrid(
                    tiles: model.tiles,
                    columns: columns,
                    now: now,
                    onRemove: { name in Task { await model.remove(name) } },
                    onSetPaused: { name, paused in Task { await model.setPaused(name, paused: paused) } }
                )
                .padding(.bottom, 4)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
    }

    private func stateMessage(icon: String, title: String, detail: String, tint: Color) -> some View {
        VStack(spacing: 8) {
            Image(systemName: icon).font(.system(size: 26)).foregroundColor(tint)
            Text(title).foregroundColor(.white.opacity(0.7))
            Text(detail).font(.system(size: 12)).foregroundColor(.white.opacity(0.6))
                .multilineTextAlignment(.center).fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    }

    private func iconButton(
        _ symbol: String,
        disabled: Bool = false,
        _ action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 11, weight: .semibold))
                .foregroundColor(.white.opacity(0.7))
                .frame(width: 24, height: 24)
                .background(RoundedRectangle(cornerRadius: 7).fill(Color.white.opacity(0.06)))
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .opacity(disabled ? 0.45 : 1)
    }
}

/// exception-beacon: the open panel's first screen when (and only when) the
/// beacon has something to say — the SAME resolver output as the closed chip,
/// so what you glanced is what you get. Healthy state renders nothing (no
/// section, no divider) and the tile grid below keeps full legibility (no
/// dim — comparing healthy accounts' quota is a real workflow).
private struct NeedsAttentionSection: View {
    let items: [GlanceAttention]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("NEEDS ATTENTION")
                .font(.system(size: 10, weight: .bold, design: .monospaced))
                .foregroundColor(Color(red: 1.0, green: 0.72, blue: 0.28))
            ForEach(items) { item in
                HStack(spacing: 8) {
                    EmailPixelized(
                        isActive: AppSettings.emailAnonymousEnabled,
                        cacheKey: item.account
                    ) {
                        Text(item.account)
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundColor(.white)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Text(item.reason)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundColor(.white.opacity(0.75))
                        .lineLimit(1)
                    Spacer(minLength: 0)
                    if let detail = item.detail {
                        Text(detail)
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundColor(.white.opacity(0.45))
                            .lineLimit(1)
                    }
                }
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.white.opacity(0.05)))
    }
}

/// Inline add-account form (rendered in-panel; sheets are unreliable in the
/// borderless island). Mirrors llmux `a → n`: a new OAuth login for Claude or
/// Codex, plus an API-key path.
private struct AddAccountInline: View {
    @ObservedObject var model: IslandUsageModel
    let onDone: () -> Void

    enum Kind: String, CaseIterable, Identifiable {
        case claude = "Claude"
        case codex = "Codex"
        case grok = "Grok"
        case apiKey = "API Key"
        var id: String { rawValue }
    }

    @State private var kind: Kind = .claude
    @State private var apiKey = ""
    @State private var name = ""
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Picker("", selection: $kind) {
                ForEach(Kind.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            switch kind {
            case .claude, .codex, .grok:
                Text(loginBlurb)
                    .font(.system(size: 11))
                    .foregroundColor(.white.opacity(0.5))
                    .fixedSize(horizontal: false, vertical: true)
                action(loginButtonTitle, disabled: false) {
                    let provider = loginProvider
                    onDone()
                    await model.startLogin(provider: provider)
                }
            case .apiKey:
                field("Name (optional)", text: $name, secure: false)
                field("Anthropic API key", text: $apiKey, secure: true)
                action("Add API key", disabled: apiKey.isEmpty) {
                    let ok = await model.addApiKey(name: name, key: apiKey)
                    if ok { onDone() } else { error = model.lastError ?? "failed" }
                }
            }

            if let error {
                Text(error).font(.system(size: 11)).foregroundColor(TerminalColors.red)
            }
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color.white.opacity(0.05)))
    }

    private var loginProvider: String {
        switch kind {
        case .claude: "claude"
        case .codex: "codex"
        case .grok: "grok"
        case .apiKey: ""
        }
    }

    private var loginButtonTitle: String {
        switch kind {
        case .claude: "Sign in to Claude"
        case .codex: "Sign in to ChatGPT"
        case .grok: "Sign in to Grok"
        case .apiKey: ""
        }
    }

    private var loginBlurb: String {
        switch kind {
        case .grok:
            // Device-code flow: the daemon polls while the user approves on
            // the opened x.ai page (docs/grok/spec.md T2).
            "Sign in to Grok in your browser and approve the verification code."
        default:
            "Sign in to your \(kind == .claude ? "Claude" : "ChatGPT") account in your browser."
        }
    }

    private func field(_ placeholder: String, text: Binding<String>, secure: Bool) -> some View {
        Group {
            if secure { SecureField(placeholder, text: text) } else { TextField(placeholder, text: text) }
        }
        .textFieldStyle(.plain)
        .font(.system(size: 12))
        .foregroundColor(.white)
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.white.opacity(0.06)))
    }

    private func action(_ title: String, disabled: Bool, _ run: @escaping () async -> Void) -> some View {
        Button {
            busy = true; error = nil
            Task { await run(); busy = false }
        } label: {
            HStack(spacing: 6) {
                if busy { ProgressView().controlSize(.small) }
                Text(title).font(.system(size: 12, weight: .semibold))
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: 8).fill(TerminalColors.prompt.opacity(0.28)))
            .foregroundColor(.white)
        }
        .buttonStyle(.plain)
        .disabled(disabled || busy)
    }
}

/// Daemon OAuth login progress, shown while a Claude/Codex subscription is being
/// added.
private struct LoginProgressView: View {
    let login: IslandUsageModel.LoginFlow
    @ObservedObject var model: IslandUsageModel

    var body: some View {
        let inProgress = login.phase == "pending" || login.phase == "starting" || login.phase == "cancelling"
        VStack(spacing: 12) {
            switch login.phase {
            case "done":
                Image(systemName: "checkmark.circle.fill").font(.system(size: 30)).foregroundColor(TerminalColors.green)
                Text("Added \(login.message ?? "account")").foregroundColor(.white)
            case "error":
                Image(systemName: "xmark.octagon.fill").font(.system(size: 30)).foregroundColor(TerminalColors.red)
                Text(login.message ?? "login failed").foregroundColor(.white.opacity(0.75)).multilineTextAlignment(.center)
            case "cancelling":
                ProgressView().controlSize(.large)
                Text(login.message ?? "Cancelling login…").foregroundColor(.white.opacity(0.75))
            default:
                ProgressView().controlSize(.large)
                Text(login.message ?? "Waiting for browser…").foregroundColor(.white.opacity(0.75))
                Text("Signing in to \(providerLabel)")
                    .font(.system(size: 10, design: .monospaced)).foregroundColor(.white.opacity(0.4))
                if let uri = login.verificationUri, let url = URL(string: uri) {
                    // Grok device flow: clickable verification link (+ code)
                    // so a remote daemon's login is completable from here.
                    Link(uri, destination: url)
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundColor(TerminalColors.blue)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    if let code = login.userCode {
                        Text("Code: \(code)")
                            .font(.system(size: 11, weight: .semibold, design: .monospaced))
                            .foregroundColor(.white.opacity(0.8))
                            .textSelection(.enabled)
                    }
                }
            }
            Button {
                Task { if inProgress { await model.cancelLogin() } else { model.dismissLogin() } }
            } label: {
                Text(inProgress ? "Cancel" : "Done").font(.system(size: 12, weight: .semibold))
            }
            .buttonStyle(.plain)
            .foregroundColor(.white.opacity(0.6))
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 18)
    }

    private var providerLabel: String {
        switch login.provider {
        case "codex": "ChatGPT"
        case "grok": "Grok"
        default: "Claude"
        }
    }
}

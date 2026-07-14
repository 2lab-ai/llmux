import SwiftUI

/// The `.usage` content of the floating island: the lifted agent-island tile
/// grid fed from llmux, plus add (Claude / Codex subscription, API key) and
/// remove. Mirrors llmux's `a → n` add-account flow via the daemon OAuth API.
struct IslandUsageView: View {
    @ObservedObject var model: IslandUsageModel
    @ObservedObject var viewModel: NotchViewModel

    @State private var adding = false
    @State private var now = Date()
    private let clock = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    private var columns: [GridItem] {
        [
            GridItem(.flexible(minimum: 150), spacing: 10),
            GridItem(.flexible(minimum: 150), spacing: 10),
        ]
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
            Text("Usage")
                .font(.system(size: 15, weight: .semibold))
                .foregroundColor(.white)
            connectionBadge
            Spacer()
            iconButton(adding ? "xmark" : "plus") { adding.toggle() }
            iconButton("arrow.clockwise") { Task { await model.refresh() } }
        }
        .padding(.horizontal, 2)
    }

    @ViewBuilder private var connectionBadge: some View {
        switch model.connection {
        case .connecting: badge(.white.opacity(0.4), "connecting…")
        case .online: badge(TerminalColors.green, "\(model.tiles.count)")
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
        if adding {
            AddAccountInline(model: model, onDone: { adding = false })
        } else if let login = model.login {
            LoginProgressView(login: login, model: model)
        } else if case .offline = model.connection, model.tiles.isEmpty {
            stateMessage(icon: "bolt.horizontal.circle",
                         title: "llmux not reachable",
                         detail: "start the daemon: llmux run  (:3456)",
                         tint: TerminalColors.red.opacity(0.85))
        } else if model.tiles.isEmpty {
            stateMessage(icon: "tray", title: "No accounts yet", detail: "add one with the + button", tint: .white.opacity(0.35))
        } else {
            ScrollView(.vertical, showsIndicators: false) {
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
            Text(detail).font(.system(size: 10, design: .monospaced)).foregroundColor(.white.opacity(0.4))
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    }

    private func iconButton(_ symbol: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 11, weight: .semibold))
                .foregroundColor(.white.opacity(0.7))
                .frame(width: 24, height: 24)
                .background(RoundedRectangle(cornerRadius: 7).fill(Color.white.opacity(0.06)))
        }
        .buttonStyle(.plain)
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
            "llmux opens a grok.com verification page — approve the code there while llmux waits. The token stays in the daemon — it never reaches this app."
        default:
            "llmux opens your browser to sign in to your \(kind == .claude ? "Claude" : "ChatGPT") subscription. The token stays in the daemon — it never reaches this app."
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
        let inProgress = login.phase == "pending" || login.phase == "starting"
        VStack(spacing: 12) {
            switch login.phase {
            case "done":
                Image(systemName: "checkmark.circle.fill").font(.system(size: 30)).foregroundColor(TerminalColors.green)
                Text("Added \(login.message ?? "account")").foregroundColor(.white)
            case "error":
                Image(systemName: "xmark.octagon.fill").font(.system(size: 30)).foregroundColor(TerminalColors.red)
                Text(login.message ?? "login failed").foregroundColor(.white.opacity(0.75)).multilineTextAlignment(.center)
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

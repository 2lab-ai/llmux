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
                UsageAccountTileGrid(
                    tiles: model.tiles,
                    columns: columns,
                    now: now,
                    onRemove: { name in Task { await model.remove(name) } }
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

/// Inline add-account form (rendered in-panel; sheets are unreliable in the
/// borderless island). Mirrors llmux `a → n`: a new OAuth login for Claude or
/// Codex, plus an API-key path.
private struct AddAccountInline: View {
    @ObservedObject var model: IslandUsageModel
    let onDone: () -> Void

    enum Kind: String, CaseIterable, Identifiable {
        case claude = "Claude"
        case codex = "Codex"
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
            case .claude, .codex:
                Text("llmux opens your browser to sign in to your \(kind == .claude ? "Claude" : "ChatGPT") subscription. The token stays in the daemon — it never reaches this app.")
                    .font(.system(size: 11))
                    .foregroundColor(.white.opacity(0.5))
                    .fixedSize(horizontal: false, vertical: true)
                action(kind == .claude ? "Sign in to Claude" : "Sign in to ChatGPT", disabled: false) {
                    let provider = kind == .claude ? "claude" : "codex"
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
                Text("Signing in to \(login.provider == "codex" ? "ChatGPT" : "Claude")")
                    .font(.system(size: 10, design: .monospaced)).foregroundColor(.white.opacity(0.4))
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
}

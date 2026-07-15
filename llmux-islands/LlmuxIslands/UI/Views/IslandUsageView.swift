import SwiftUI

/// The common Usage path: current connection/attention, privacy-safe account
/// identity, and primary quota. Credential metadata and account controls are
/// available through a local-only Advanced disclosure.
struct IslandUsageView: View {
    @ObservedObject var model: IslandUsageModel
    @ObservedObject var viewModel: NotchViewModel

    @State private var adding = false
    @State private var advancedPresented: Bool
    @State private var now = Date()
    @State private var pendingRemovalID: String?
    @State private var pendingRemovalLabel = ""
    @State private var showRemovalConfirmation = false
    private let snapshotNow: Date?
    private let clock = Timer.publish(every: 1, on: .main, in: .common).autoconnect()

    init(
        model: IslandUsageModel,
        viewModel: NotchViewModel,
        advancedInitiallyPresented: Bool = false,
        snapshotNow: Date? = nil
    ) {
        self.model = model
        self.viewModel = viewModel
        self.snapshotNow = snapshotNow
        _advancedPresented = State(initialValue: advancedInitiallyPresented)
        _now = State(initialValue: snapshotNow ?? Date())
    }

    private var loginInProgress: Bool {
        guard let phase = model.login?.phase else { return false }
        return phase == "starting" || phase == "pending" || phase == "cancelling"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            if case .offline(let reason) = model.connection {
                IslandSafetyBanner(title: "llmux is offline", detail: reason, critical: true)
            }
            IslandLatestFailureBanner(receipts: model.verificationReceipts)
            content
        }
        .onReceive(clock) {
            if snapshotNow == nil { now = $0 }
        }
        .confirmationDialog(
            "Remove \(pendingRemovalLabel)?",
            isPresented: $showRemovalConfirmation,
            titleVisibility: .visible
        ) {
            Button("Remove account", role: .destructive) {
                guard let accountID = pendingRemovalID else { return }
                pendingRemovalID = nil
                Task { await model.remove(accountID) }
            }
            Button("Cancel", role: .cancel) {
                pendingRemovalID = nil
            }
        } message: {
            Text("The account will be removed from llmux. This action cannot be undone here.")
        }
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Usage")
                    .font(.headline)
                Text("Quota and account status")
                    .font(.caption)
                    .foregroundColor(.white.opacity(0.6))
            }
            IslandConnectionLabel(connection: model.connection, accountCount: model.tiles.count)
            Spacer()
            actionButton(adding ? "Cancel" : "Add account", symbol: adding ? "xmark" : "plus", disabled: loginInProgress) {
                adding.toggle()
            }
            actionButton("Refresh", symbol: "arrow.clockwise") { Task { await model.refresh() } }
        }
        .padding(.horizontal, 2)
    }

    @ViewBuilder private var content: some View {
        if adding {
            AddAccountInline(model: model, onDone: { adding = false })
        } else if let login = model.login {
            LoginProgressView(login: login, model: model)
        } else if case .offline = model.connection, model.tiles.isEmpty {
            stateMessage(icon: "bolt.horizontal.circle",
                         title: "llmux not reachable",
                         detail: "check the configured llmux endpoint and credentials",
                         tint: TerminalColors.red.opacity(0.85))
        } else if model.tiles.isEmpty {
            stateMessage(icon: "tray", title: "No accounts yet", detail: "Choose Add account to connect one", tint: .white.opacity(0.5))
        } else {
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 10) {
                    if !model.attention.isEmpty {
                        NeedsAttentionSection(items: model.attention)
                    }

                    IslandSurface {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("Accounts")
                                .font(.subheadline.weight(.semibold))
                            UsageAccountCompactList(tiles: model.tiles)
                        }
                    }

                    IslandAdvancedDisclosure(isPresented: $advancedPresented) {
                        UsageAdvancedAccountList(
                            tiles: model.tiles,
                            now: now,
                            onSetPaused: { accountID, paused in
                                Task { await model.setPaused(accountID, paused: paused) }
                            },
                            onRemove: requestRemoval
                        )
                    }
                }
                .padding(.bottom, 4)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
    }

    private func stateMessage(icon: String, title: String, detail: String, tint: Color) -> some View {
        VStack(spacing: 8) {
            Image(systemName: icon).font(.system(size: 26)).foregroundColor(tint)
            Text(title).foregroundColor(.white.opacity(0.7))
            Text(detail).font(.caption).foregroundColor(.white.opacity(0.6))
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    }

    private func actionButton(
        _ title: String,
        symbol: String,
        disabled: Bool = false,
        _ action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Label(title, systemImage: symbol)
                .font(.caption.weight(.medium))
        }
        .buttonStyle(IslandButtonStyle())
        .disabled(disabled)
        .opacity(disabled ? 0.45 : 1)
    }

    private func requestRemoval(_ tile: UsageAccountTile) {
        pendingRemovalID = tile.accountId
        let displayName = tile.email ?? tile.label
        if AppSettings.emailAnonymousEnabled, displayName.contains("@") {
            let ordinal = (model.tiles.firstIndex(where: { $0.id == tile.id }) ?? 0) + 1
            pendingRemovalLabel = IslandPresentationPolicy.privateAccountLabel(
                providerName: tile.provider.displayName,
                ordinal: ordinal
            )
        } else {
            pendingRemovalLabel = displayName
        }
        showRemovalConfirmation = true
    }
}

private struct UsageAdvancedAccountList: View {
    let tiles: [UsageAccountTile]
    let now: Date
    let onSetPaused: (String, Bool) -> Void
    let onRemove: (UsageAccountTile) -> Void
    @AppStorage(AppSettings.emailAnonymousEnabledKey) private var emailAnonymousEnabled = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Account details and controls")
                .font(.subheadline.weight(.semibold))

            ForEach(Array(tiles.enumerated()), id: \.element.id) { index, tile in
                VStack(alignment: .leading, spacing: 7) {
                    HStack(spacing: 8) {
                        UsageProviderIcon(provider: tile.provider, size: 12)
                        Text(tile.provider.displayName)
                            .font(.subheadline.weight(.medium))
                        let displayName = tile.email ?? tile.label
                        EmailPixelized(
                            isActive: emailAnonymousEnabled && displayName.contains("@"),
                            cacheKey: displayName,
                            accessibilityLabel: IslandPresentationPolicy.privateAccountLabel(
                                providerName: tile.provider.displayName,
                                ordinal: index + 1
                            )
                        ) {
                            Text(displayName)
                                .font(.caption)
                                .foregroundColor(.white.opacity(0.6))
                                .lineLimit(1)
                                .truncationMode(.middle)
                        }
                        Spacer()
                        if tile.paused {
                            Text("Paused")
                                .font(.caption.weight(.semibold))
                                .foregroundColor(TerminalColors.amber)
                        }
                    }

                    LazyVGrid(
                        columns: [GridItem(.adaptive(minimum: 88), spacing: 12, alignment: .leading)],
                        alignment: .leading,
                        spacing: 8
                    ) {
                        metadata("Status", tile.status)
                        metadata("Selection", tile.current ? "current" : "standby")
                        if tile.inFlight > 0 {
                            metadata("In flight", "\(tile.inFlight)")
                        }
                        if let tier = tile.tier, !tier.isEmpty {
                            metadata("Plan", tier)
                        }
                        if let token = tile.tokenRefresh {
                            metadata("Token", tokenText(token))
                        }
                        if let reset = tile.info?.fiveHourReset {
                            metadata("5h reset", reset.formatted(date: .omitted, time: .shortened))
                        }
                    }

                    if let technical = tile.issue?.technicalDetails, !technical.isEmpty {
                        Text(technical)
                            .font(.caption2.monospaced())
                            .foregroundColor(.white.opacity(0.6))
                            .textSelection(.enabled)
                    }

                    HStack(spacing: 8) {
                        Button(tile.paused ? "Resume" : "Pause") {
                            onSetPaused(tile.accountId, !tile.paused)
                        }
                        .buttonStyle(IslandButtonStyle())

                        Button("Remove", role: .destructive) {
                            onRemove(tile)
                        }
                        .buttonStyle(IslandButtonStyle(.quiet))
                    }
                }
                .padding(9)
                .background(Color.white.opacity(0.04))
                .overlay(alignment: .top) {
                    Rectangle().fill(Color.white.opacity(0.12)).frame(height: 1)
                }
            }
        }
    }

    private func metadata(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(label)
                .font(.caption2)
                .foregroundColor(.white.opacity(0.44))
            Text(value)
                .font(.caption.monospacedDigit())
                .foregroundColor(.white.opacity(0.6))
        }
    }

    private func tokenText(_ token: TokenRefreshInfo) -> String {
        if token.expiresAt <= now { return "Expired" }
        return token.expiresAt.formatted(date: .omitted, time: .shortened)
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
            Text("Needs attention")
                .font(.subheadline.weight(.semibold))
                .foregroundColor(TerminalColors.amber)
            ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                HStack(spacing: 8) {
                    EmailPixelized(
                        isActive: AppSettings.emailAnonymousEnabled,
                        cacheKey: item.account,
                        accessibilityLabel: "Account \(index + 1)"
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
                            .foregroundColor(.white.opacity(0.6))
                            .lineLimit(1)
                    }
                }
            }
        }
        .padding(10)
        .background(Color.white.opacity(0.04))
        .overlay(alignment: .leading) {
            Rectangle().fill(TerminalColors.amber).frame(width: 2)
        }
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
            HStack(spacing: 0) {
                ForEach(Kind.allCases) { option in
                    let selected = kind == option
                    Button {
                        kind = option
                    } label: {
                        Text(option.rawValue)
                            .font(.caption.weight(selected ? .semibold : .regular))
                            .foregroundColor(selected ? .black : .white.opacity(0.6))
                            .frame(maxWidth: .infinity, minHeight: 44)
                            .background(selected ? Color.white : Color.white.opacity(0.04))
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(selected ? .isSelected : [])
                }
            }
            .overlay(alignment: .bottom) {
                Rectangle().fill(Color.white.opacity(0.12)).frame(height: 1)
            }

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
                field(label: "Name", placeholder: "Optional", text: $name, secure: false)
                field(
                    label: "Anthropic API key",
                    placeholder: "Enter API key",
                    text: $apiKey,
                    secure: true
                )
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
        .background(Color.white.opacity(0.04))
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

    private func field(
        label: String,
        placeholder: String,
        text: Binding<String>,
        secure: Bool
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(label)
                .font(.caption.weight(.medium))
                .foregroundColor(.white.opacity(0.8))

            Group {
                if secure { SecureField(placeholder, text: text) } else { TextField(placeholder, text: text) }
            }
            .textFieldStyle(.plain)
            .font(.body)
            .foregroundColor(.white)
            .padding(.horizontal, 10)
            .frame(minHeight: 44)
            .background(Color.white.opacity(0.07))
        }
    }

    private func action(_ title: String, disabled: Bool, _ run: @escaping () async -> Void) -> some View {
        let inactive = disabled || busy
        return Button {
            busy = true; error = nil
            Task { await run(); busy = false }
        } label: {
            HStack(spacing: 6) {
                if busy { ProgressView().controlSize(.small).tint(inactive ? .white : .black) }
                Text(title).font(.system(size: 12, weight: .semibold))
            }
            .frame(maxWidth: .infinity)
            .frame(minHeight: 44)
            .background(inactive ? Color.white.opacity(0.08) : Color.white)
            .foregroundColor(inactive ? .white.opacity(0.44) : .black)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(inactive)
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
                    .font(.caption).foregroundColor(.white.opacity(0.6))
                if let uri = login.verificationUri, let url = URL(string: uri) {
                    // Grok device flow: clickable verification link (+ code)
                    // so a remote daemon's login is completable from here.
                    Link(uri, destination: url)
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundColor(.white.opacity(0.85))
                        .underline()
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

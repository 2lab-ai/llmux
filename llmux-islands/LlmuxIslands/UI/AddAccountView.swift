import SwiftUI

/// Inline "add account" form (rendered in-panel rather than a `.sheet`, which is
/// unreliable in a borderless panel). API-key accounts post to
/// `/llmux/add-account`; OAuth subscriptions kick off the daemon-run login.
struct AddAccountView: View {
    @EnvironmentObject var model: AccountsViewModel
    let onDone: () -> Void

    enum Kind: String, CaseIterable, Identifiable {
        case apiKey = "API Key"
        case claude = "Claude"
        case codex = "Codex"
        var id: String { rawValue }
    }

    @State private var kind: Kind = .apiKey
    @State private var apiKey = ""
    @State private var name = ""
    @State private var busy = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Add account")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundColor(.white)
                Spacer()
                Button { onDone() } label: {
                    Image(systemName: "xmark").font(.system(size: 11, weight: .semibold))
                }
                .buttonStyle(.plain)
                .foregroundColor(.white.opacity(0.5))
            }

            Picker("", selection: $kind) {
                ForEach(Kind.allCases) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            switch kind {
            case .apiKey:
                styledField("Name (optional)", text: $name, secure: false)
                styledField("Anthropic API key", text: $apiKey, secure: true)
                actionButton("Add API key", disabled: apiKey.isEmpty) { await addKey() }
            case .claude, .codex:
                Text("llmux opens your browser to sign in. The token is held by the daemon — it never reaches this app.")
                    .font(.system(size: 11))
                    .foregroundColor(.white.opacity(0.5))
                    .fixedSize(horizontal: false, vertical: true)
                actionButton(kind == .claude ? "Sign in to Claude" : "Sign in to ChatGPT", disabled: false) {
                    let provider = kind == .claude ? "claude" : "codex"
                    onDone()
                    await model.startLogin(provider: provider)
                }
            }

            if let error {
                Text(error)
                    .font(.system(size: 11))
                    .foregroundColor(TerminalColors.red)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(14)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Color.white.opacity(0.04)))
    }

    private func styledField(_ placeholder: String, text: Binding<String>, secure: Bool) -> some View {
        Group {
            if secure {
                SecureField(placeholder, text: text)
            } else {
                TextField(placeholder, text: text)
            }
        }
        .textFieldStyle(.plain)
        .font(.system(size: 12))
        .foregroundColor(.white)
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Color.white.opacity(0.06)))
    }

    private func actionButton(_ title: String, disabled: Bool, _ action: @escaping () async -> Void) -> some View {
        Button {
            busy = true
            error = nil
            Task {
                await action()
                busy = false
            }
        } label: {
            HStack(spacing: 6) {
                if busy { ProgressView().controlSize(.small) }
                Text(title).font(.system(size: 12, weight: .semibold))
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 8)
            .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(TerminalColors.prompt.opacity(0.28)))
            .foregroundColor(.white)
        }
        .buttonStyle(.plain)
        .disabled(disabled || busy)
    }

    private func addKey() async {
        let ok = await model.addApiKey(name: name, key: apiKey)
        if ok {
            onDone()
        } else {
            error = model.lastError ?? "failed to add account"
        }
    }
}

import SwiftUI

/// The island panel: header (title + connection + add/refresh/close) over the
/// accounts grid, with inline add and OAuth-login states. Hosted in the
/// borderless `NotchPanel`.
struct RootView: View {
    @EnvironmentObject var model: AccountsViewModel
    let onClose: () -> Void

    @State private var adding = false

    private let columns = [
        GridItem(.flexible(), spacing: 10),
        GridItem(.flexible(), spacing: 10),
    ]

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().overlay(Color.white.opacity(0.08))
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(
            RoundedRectangle(cornerRadius: 18, style: .continuous)
                .fill(Color.black.opacity(0.92))
                .overlay(
                    RoundedRectangle(cornerRadius: 18, style: .continuous)
                        .stroke(Color.white.opacity(0.08), lineWidth: 1)
                )
        )
        .clipShape(RoundedRectangle(cornerRadius: 18, style: .continuous))
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "gauge.with.dots.needle.67percent")
                .foregroundColor(TerminalColors.prompt)
            Text("llmux")
                .font(.system(size: 14, weight: .bold))
                .foregroundColor(.white)
            connectionBadge
            Spacer()
            iconButton("plus") { adding.toggle() }
            iconButton("arrow.clockwise") { Task { await model.refresh() } }
            iconButton("xmark") { onClose() }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }

    @ViewBuilder private var connectionBadge: some View {
        switch model.connection {
        case .connecting: badge(.white.opacity(0.4), "connecting…")
        case .online: badge(TerminalColors.green, "\(model.accounts.count) accounts")
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
            AddAccountView(onDone: { adding = false })
                .environmentObject(model)
                .padding(12)
        } else if let login = model.login {
            loginCard(login)
        } else {
            accountsGrid
        }
    }

    @ViewBuilder private var accountsGrid: some View {
        if case .offline = model.connection, model.accounts.isEmpty {
            offlineState
        } else if model.accounts.isEmpty {
            emptyState
        } else {
            ScrollView {
                LazyVGrid(columns: columns, spacing: 10) {
                    ForEach(model.accounts) { account in
                        AccountTile(
                            account: account,
                            isCurrent: account.name == model.current,
                            onRemove: { Task { await model.remove(account.name) } }
                        )
                    }
                }
                .padding(12)
            }
        }
    }

    private func loginCard(_ login: AccountsViewModel.LoginFlow) -> some View {
        let inProgress = login.phase == "pending" || login.phase == "starting"
        return VStack(spacing: 14) {
            switch login.phase {
            case "done":
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 34)).foregroundColor(TerminalColors.green)
                Text("Added \(login.message ?? "account")").foregroundColor(.white)
            case "error":
                Image(systemName: "xmark.octagon.fill")
                    .font(.system(size: 34)).foregroundColor(TerminalColors.red)
                Text(login.message ?? "login failed")
                    .foregroundColor(.white.opacity(0.7))
                    .multilineTextAlignment(.center)
            default:
                ProgressView().controlSize(.large)
                Text(login.message ?? "Waiting for browser…").foregroundColor(.white.opacity(0.75))
                Text("Signing in to \(login.provider == "codex" ? "ChatGPT" : "Claude")")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundColor(.white.opacity(0.4))
            }
            Button {
                Task {
                    if inProgress { await model.cancelLogin() } else { model.dismissLogin() }
                }
            } label: {
                Text(inProgress ? "Cancel" : "Done")
                    .font(.system(size: 12, weight: .semibold))
            }
            .buttonStyle(.plain)
            .foregroundColor(.white.opacity(0.6))
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "tray").font(.system(size: 28)).foregroundColor(.white.opacity(0.3))
            Text("No accounts yet").foregroundColor(.white.opacity(0.6))
            Text("Add one with the + button").font(.system(size: 11)).foregroundColor(.white.opacity(0.35))
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var offlineState: some View {
        VStack(spacing: 8) {
            Image(systemName: "bolt.horizontal.circle")
                .font(.system(size: 28)).foregroundColor(TerminalColors.red.opacity(0.8))
            Text("llmux not reachable").foregroundColor(.white.opacity(0.75))
            Text("start the daemon: llmux run  (:3456)")
                .font(.system(size: 10, design: .monospaced))
                .foregroundColor(.white.opacity(0.4))
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }

    private func iconButton(_ symbol: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 11, weight: .semibold))
                .foregroundColor(.white.opacity(0.7))
                .frame(width: 26, height: 26)
                .background(RoundedRectangle(cornerRadius: 7, style: .continuous).fill(Color.white.opacity(0.06)))
        }
        .buttonStyle(.plain)
    }
}

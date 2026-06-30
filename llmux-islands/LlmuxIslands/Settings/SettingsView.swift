import ServiceManagement
import SwiftUI

/// The standalone Settings window content. llmux connection settings up top,
/// then the same display / sound pickers the in-island menu uses, launch-at-login,
/// and an About section.
struct SettingsView: View {
    @ObservedObject var model: IslandUsageModel
    @ObservedObject private var screenSelector = ScreenSelector.shared
    @ObservedObject private var soundSelector = SoundSelector.shared

    @State private var host = LlmuxSettings.host
    @State private var port = String(LlmuxSettings.port)
    @State private var apiKey = LlmuxSettings.apiKey
    @State private var launchAtLogin = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                section("llmux daemon") {
                    labeledField("Host", text: $host)
                    labeledField("Port", text: $port)
                    labeledField("API key", text: $apiKey, secure: true, hint: "only needed for a remote daemon")
                    HStack(spacing: 10) {
                        connectionStatus
                        Spacer()
                        Button("Apply & Reconnect") { apply() }
                            .keyboardShortcut(.return, modifiers: [])
                    }
                    .padding(.top, 2)
                }

                section("Display") {
                    Text("Which screen the island appears on.")
                        .font(.system(size: 11)).foregroundColor(.white.opacity(0.45))
                    ScreenPickerRow(screenSelector: screenSelector)
                }

                section("Notifications") {
                    SoundPickerRow(soundSelector: soundSelector)
                }

                section("General") {
                    Toggle("Launch at login", isOn: $launchAtLogin)
                        .toggleStyle(SwitchToggleStyle(tint: TerminalColors.green))
                        .onChange(of: launchAtLogin) { _, on in setLaunchAtLogin(on) }
                        .font(.system(size: 13))
                        .foregroundColor(.white.opacity(0.8))
                }

                section("About") {
                    HStack {
                        Text("Version").foregroundColor(.white.opacity(0.55))
                        Spacer()
                        Text(NotchMenuView.appVersion).foregroundColor(.white.opacity(0.85))
                    }
                    .font(.system(size: 12))
                    Link("llmux on GitHub", destination: URL(string: "https://github.com/2lab-ai/llmux")!)
                        .font(.system(size: 12))
                }
            }
            .padding(22)
        }
        .frame(minWidth: 460, minHeight: 540)
        .background(Color(red: 0.06, green: 0.06, blue: 0.07))
        .preferredColorScheme(.dark)
        .onAppear {
            launchAtLogin = SMAppService.mainApp.status == .enabled
        }
    }

    // MARK: - Sections

    @ViewBuilder
    private func section<Content: View>(_ title: String, @ViewBuilder _ content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title.uppercased())
                .font(.system(size: 10, weight: .semibold, design: .monospaced))
                .foregroundColor(.white.opacity(0.35))
            content()
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(Color.white.opacity(0.04)))
    }

    private func labeledField(_ label: String, text: Binding<String>, secure: Bool = false, hint: String? = nil) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(label).font(.system(size: 12)).foregroundColor(.white.opacity(0.6)).frame(width: 70, alignment: .leading)
                Group {
                    if secure { SecureField("", text: text) } else { TextField("", text: text) }
                }
                .textFieldStyle(.plain)
                .font(.system(size: 12, design: .monospaced))
                .foregroundColor(.white)
                .padding(7)
                .background(RoundedRectangle(cornerRadius: 7).fill(Color.white.opacity(0.07)))
            }
            if let hint {
                Text(hint).font(.system(size: 10)).foregroundColor(.white.opacity(0.35)).padding(.leading, 78)
            }
        }
    }

    @ViewBuilder
    private var connectionStatus: some View {
        switch model.connection {
        case .connecting: dot(.white.opacity(0.4), "connecting…")
        case .online: dot(TerminalColors.green, "online · \(model.tiles.count) accounts")
        case .offline: dot(TerminalColors.red, "offline")
        }
    }

    private func dot(_ color: Color, _ text: String) -> some View {
        HStack(spacing: 6) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(text).font(.system(size: 11, design: .monospaced)).foregroundColor(.white.opacity(0.55))
        }
    }

    private func apply() {
        LlmuxSettings.host = host.trimmingCharacters(in: .whitespacesAndNewlines)
        LlmuxSettings.port = Int(port.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 3456
        LlmuxSettings.apiKey = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        Task { await model.refresh() }
    }

    private func setLaunchAtLogin(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
        } catch {
            launchAtLogin = SMAppService.mainApp.status == .enabled
        }
    }
}

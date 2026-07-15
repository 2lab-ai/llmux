//
//  NotchMenuView.swift
//  ClaudeIsland
//
//  Minimal menu matching Dynamic Island aesthetic
//

import ApplicationServices
import Combine
import Darwin
import SwiftUI
import ServiceManagement

// MARK: - NotchMenuView

/// Deterministic projection used only by the offscreen production renderer.
/// Live Settings continues to read and mutate the native OS adapters below.
struct NotchMenuSnapshotState {
    let screenLabel: String
    let soundLabel: String
    let emailAnonymous: Bool
    let showFableWeekly: Bool
    let launchAtLogin: Bool
    let accessibilityTrusted: Bool
    let connectionHost: String
    let connectionPort: Int
    let apiKeyConfigured: Bool
    let releaseChannel: ReleaseChannel

    init(canonicalState state: SharedUiState) {
        let screenID = state.window.selectedScreenID
        screenLabel = state.settings.screens.first(where: { $0.selected || $0.id == screenID })?.label
            ?? Self.fallbackLabel(for: screenID, automaticLabel: "Auto")

        let soundID = state.settings.soundID ?? "default"
        soundLabel = state.settings.sounds.first(where: { $0.selected == true || $0.id == soundID })?.label
            ?? Self.fallbackLabel(for: soundID, automaticLabel: "Default")

        emailAnonymous = state.settings.emailAnonymous
        showFableWeekly = state.settings.showFableWeekly
        launchAtLogin = state.settings.autostart.enabled ?? false
        // Accessibility permission is intentionally shell-owned and absent
        // from UiState. Evidence pins the actionable not-yet-granted state.
        accessibilityTrusted = false

        let endpoint = URLComponents(string: state.connection.endpointDisplay)
        connectionHost = endpoint?.host ?? "127.0.0.1"
        connectionPort = endpoint?.port ?? 3456
        apiKeyConfigured = state.settings.apiKeyConfigured
        // The checked-in dashboard fixture is explicitly a preview build.
        // Keep a deterministic fallback for older UiState payloads that did
        // not yet project the daemon channel into maintenance state.
        releaseChannel = state.settings.maintenance.channel
            .flatMap(ReleaseChannel.init(rawValue:)) ?? .preview
    }

    private static func fallbackLabel(for id: String, automaticLabel: String) -> String {
        guard !id.isEmpty, !["auto", "default"].contains(id.lowercased()) else {
            return automaticLabel
        }
        return id.replacingOccurrences(of: "-", with: " ").capitalized
    }
}

struct NotchMenuView: View {
    @ObservedObject var viewModel: NotchViewModel
    @ObservedObject private var usageModel = IslandUsageModel.shared
    @ObservedObject private var screenSelector = ScreenSelector.shared
    @ObservedObject private var soundSelector = SoundSelector.shared
    @State private var launchAtLogin: Bool
    @State private var accessibilityTrusted: Bool
    @State private var advancedPresented: Bool
    @AppStorage(AppSettings.emailAnonymousEnabledKey) private var emailAnonymousEnabled = false
    @AppStorage(AppSettings.showFableWeeklyKey) private var showFableWeekly = true
    private let snapshotState: NotchMenuSnapshotState?

    init(
        viewModel: NotchViewModel,
        advancedInitiallyPresented: Bool = false,
        snapshotState: NotchMenuSnapshotState? = nil
    ) {
        self.viewModel = viewModel
        self.snapshotState = snapshotState
        _launchAtLogin = State(initialValue: snapshotState?.launchAtLogin ?? false)
        _accessibilityTrusted = State(
            initialValue: snapshotState?.accessibilityTrusted ?? AXIsProcessTrusted()
        )
        _advancedPresented = State(initialValue: advancedInitiallyPresented)
    }

    static var appVersion: String {
        let v = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.0"
        return "v\(v)"
    }

    var body: some View {
        ScrollView(.vertical, showsIndicators: false) {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .center, spacing: 10) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Settings")
                            .font(.headline)
                        Text("Display, sound, privacy, and startup")
                            .font(.caption)
                            .foregroundColor(.white.opacity(0.6))
                    }
                    Spacer()
                    IslandConnectionLabel(connection: usageModel.connection, accountCount: usageModel.tiles.count)
                }

                if case .offline(let reason) = usageModel.connection {
                    IslandSafetyBanner(title: "llmux is offline", detail: reason, critical: true)
                }
                IslandLatestFailureBanner(receipts: usageModel.verificationReceipts)
                if !accessibilityTrusted {
                    AccessibilityRow(isEnabled: false)
                }

                IslandSurface {
                    VStack(spacing: 0) {
                        ScreenPickerRow(
                            screenSelector: screenSelector,
                            snapshotSelectionLabel: snapshotState?.screenLabel
                        )
                        SoundPickerRow(
                            soundSelector: soundSelector,
                            snapshotSelectionLabel: snapshotState?.soundLabel
                        )
                    }
                }

                IslandSurface {
                    VStack(spacing: 0) {
                        MenuToggleRow(
                            icon: "eye.slash",
                            label: "Email anonymous",
                            isOn: snapshotState?.emailAnonymous ?? emailAnonymousEnabled
                        ) {
                            Task { await usageModel.toggleEmailAnonymous() }
                        }
                        MenuToggleRow(icon: "power", label: "Launch at Login", isOn: launchAtLogin) {
                            Task {
                                _ = await usageModel.setLaunchAtLogin(!launchAtLogin)
                                launchAtLogin = SMAppService.mainApp.status == .enabled
                            }
                        }
                    }
                }

                IslandAdvancedDisclosure(isPresented: $advancedPresented) {
                    VStack(spacing: 0) {
                        MenuToggleRow(
                            icon: "calendar",
                            label: "Show Fable weekly (7d)",
                            isOn: snapshotState?.showFableWeekly ?? showFableWeekly
                        ) {
                            Task { await usageModel.setShowFableWeekly(!showFableWeekly) }
                        }
                        LlmuxConnectionSection(snapshotState: snapshotState)
                        LlmuxMaintenanceSection(snapshotState: snapshotState)
                        LlmuxEventsSection()
                        if accessibilityTrusted {
                            AccessibilityRow(isEnabled: true)
                        }

                        MenuRow(icon: "info.circle", label: "llmux-islands \(Self.appVersion)") {
                            if let url = URL(string: "https://github.com/2lab-ai/llmux/releases") {
                                NSWorkspace.shared.open(url)
                            }
                        }
                        MenuRow(icon: "arrow.up.right", label: "llmux source") {
                            if let url = URL(string: "https://github.com/2lab-ai/llmux") {
                                NSWorkspace.shared.open(url)
                            }
                        }
                    }
                }

                IslandSurface {
                    MenuRow(icon: "xmark.circle", label: "Quit", isDestructive: true) {
                        if let delegate = AppDelegate.shared {
                            delegate.requestTerminateFromMenu()
                        } else {
                            NSApplication.shared.terminate(nil)
                            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                                if NSApplication.shared.isRunning {
                                    Darwin.exit(0)
                                }
                            }
                        }
                    }
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 8)
        }
        .scrollBounceBehavior(.basedOnSize)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .onAppear {
            refreshStates()
        }
        .onChange(of: viewModel.contentType) { _, newValue in
            if newValue == .menu {
                refreshStates()
            }
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            refreshStates()
        }
    }

    private func refreshStates() {
        guard snapshotState == nil else { return }
        launchAtLogin = SMAppService.mainApp.status == .enabled
        accessibilityTrusted = AXIsProcessTrusted()
        screenSelector.refreshScreens()
    }
}

// MARK: - Update Row (removed — no Sparkle in llmux-islands)

// MARK: - Accessibility Permission Row

struct AccessibilityRow: View {
    let isEnabled: Bool

    @State private var isHovered = false

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "hand.raised")
                .font(.system(size: 12))
                .foregroundColor(textColor)
                .frame(width: 16)

            Text("Accessibility")
                .font(.system(size: 13, weight: .medium))
                .foregroundColor(textColor)

            Spacer()

            if isEnabled {
                Image(systemName: "checkmark")
                    .font(.caption.weight(.semibold))
                    .foregroundColor(.white.opacity(0.6))

                Text("On")
                    .font(.system(size: 11))
                    .foregroundColor(.white.opacity(0.6))
            } else {
                Button(action: openAccessibilitySettings) {
                    Text("Enable")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundColor(.black)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(
                            Rectangle()
                                .fill(Color.white)
                        )
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 44)
        .background(isHovered ? Color.white.opacity(0.08) : Color.clear)
        .onHover { isHovered = $0 }
    }

    private var textColor: Color {
        .white.opacity(isHovered ? 1.0 : 0.7)
    }

    private func openAccessibilitySettings() {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility") {
            NSWorkspace.shared.open(url)
        }
    }
}

struct MenuRow: View {
    let icon: String
    let label: String
    var isDestructive: Bool = false
    let action: () -> Void

    @State private var isHovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.system(size: 12))
                    .foregroundColor(textColor)
                    .frame(width: 16)

                Text(label)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(textColor)

                Spacer()
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(isHovered ? Color.white.opacity(0.08) : Color.clear)
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
    }

    private var textColor: Color {
        if isDestructive {
            return Color(red: 1.0, green: 0.4, blue: 0.4)
        }
        return .white.opacity(isHovered ? 1.0 : 0.7)
    }
}

struct MenuToggleRow: View {
    let icon: String
    let label: String
    let isOn: Bool
    let action: () -> Void

    @State private var isHovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.system(size: 12))
                    .foregroundColor(textColor)
                    .frame(width: 16)

                Text(label)
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(textColor)

                Spacer()

                Image(systemName: isOn ? "checkmark" : "minus")
                    .font(.caption.weight(.semibold))
                    .foregroundColor(.white.opacity(isOn ? 0.8 : 0.5))

                Text(isOn ? "On" : "Off")
                    .font(.system(size: 11))
                    .foregroundColor(.white.opacity(0.6))
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(isHovered ? Color.white.opacity(0.08) : Color.clear)
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
    }

    private var textColor: Color {
        .white.opacity(isHovered ? 1.0 : 0.7)
    }
}

// MARK: - llmux Connection Section

/// Collapsible llmux daemon connection editor, living inside the ☰ menu (the
/// app has no separate Settings window). Writes `LlmuxSettings` and reconnects.
private struct LlmuxConnectionSection: View {
    private let snapshotState: NotchMenuSnapshotState?
    @State private var host: String
    @State private var port: String
    @State private var apiKey = ""
    @State private var storedKeyConfigured: Bool
    @State private var clearStoredKey = false
    @State private var expanded = false
    @State private var isHovered = false

    init(snapshotState: NotchMenuSnapshotState? = nil) {
        self.snapshotState = snapshotState
        _host = State(initialValue: snapshotState?.connectionHost ?? LlmuxSettings.host)
        _port = State(initialValue: String(snapshotState?.connectionPort ?? LlmuxSettings.port))
        _storedKeyConfigured = State(
            initialValue: snapshotState?.apiKeyConfigured ?? !LlmuxSettings.apiKey.isEmpty
        )
    }

    var body: some View {
        VStack(spacing: 6) {
            Button {
                expanded.toggle()
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "network")
                        .font(.system(size: 13))
                        .foregroundColor(.white.opacity(0.7))
                        .frame(width: 18)
                    Text("llmux connection")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundColor(.white.opacity(isHovered ? 1.0 : 0.7))
                    Spacer()
                    Text("\(host):\(port)")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundColor(.white.opacity(0.6))
                        .lineLimit(1)
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundColor(.white.opacity(0.5))
                }
                .padding(.horizontal, 12)
                .frame(minHeight: 44)
                .background(Color.white.opacity(isHovered ? 0.08 : 0))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { isHovered = $0 }

            if expanded {
                VStack(spacing: 6) {
                    field(placeholder: "Host", text: $host)
                    HStack(spacing: 6) {
                        field(placeholder: "Port", text: $port)
                            .frame(width: 86)
                        field(
                            placeholder: storedKeyConfigured
                                ? "API key (blank keeps stored key)"
                                : "API key (required for remote)",
                            text: $apiKey,
                            secure: true
                        )
                        .disabled(clearStoredKey)
                    }
                    if storedKeyConfigured {
                        Toggle("Clear the stored API key", isOn: $clearStoredKey)
                            .toggleStyle(.checkbox)
                            .font(.system(size: 10))
                            .foregroundColor(.white.opacity(0.65))
                            .onChange(of: clearStoredKey) { _, shouldClear in
                                if shouldClear { apiKey = "" }
                            }
                    }
                    Button { apply() } label: {
                        Text("Apply & reconnect")
                            .font(.system(size: 11, weight: .semibold))
                            .frame(maxWidth: .infinity)
                            .frame(minHeight: 44)
                            .background(Color.white)
                            .foregroundColor(.black)
                    }
                    .buttonStyle(.plain)
                }
                .padding(.horizontal, 12)
                .padding(.bottom, 6)
            }
        }
    }

    @ViewBuilder
    private func field(placeholder: String, text: Binding<String>, secure: Bool = false) -> some View {
        Group {
            if secure {
                SecureField(placeholder, text: text)
            } else {
                TextField(placeholder, text: text)
            }
        }
        .textFieldStyle(.plain)
        .font(.system(size: 11, design: .monospaced))
        .foregroundColor(.white)
        .padding(7)
        .background(Color.white.opacity(0.07))
    }

    private func apply() {
        let h = host.trimmingCharacters(in: .whitespacesAndNewlines)
        let candidateHost = h.isEmpty ? "127.0.0.1" : h
        let candidatePort = Int(port.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 3456
        let replacement = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        let intent: ConnectionApiKeyIntent = if clearStoredKey {
            .clear
        } else if replacement.isEmpty {
            .keep
        } else {
            .replace(replacement)
        }
        Task {
            let applied = await IslandUsageModel.shared.applyConnection(
                host: candidateHost,
                port: candidatePort,
                apiKeyIntent: intent
            )
            guard applied else { return }
            host = LlmuxSettings.host
            port = String(LlmuxSettings.port)
            apiKey = ""
            clearStoredKey = false
            storedKeyConfigured = !LlmuxSettings.apiKey.isEmpty
        }
    }
}

// MARK: - llmux Maintenance Section

/// Runs the `llmux` CLI on the user's behalf: a self-update button and a
/// release-channel switch (Stable / Preview). Both drive the injectable
/// `CLIRunner` off the main thread and surface progress + result inline. A
/// channel switch reinstalls llmux AND this app, so it is gated behind a
/// confirmation dialog; on failure the visible selection reverts to the real
/// channel reported by the CLI.
private struct LlmuxMaintenanceSection: View {
    /// Injectable so previews/hosts could swap the binary path; production uses
    /// the default `/opt/homebrew/bin/llmux` (with `/usr/bin/env` fallback).
    var runner = CLIRunner()
    private let snapshotState: NotchMenuSnapshotState?

    // Update-now state.
    @State private var updateRunning = false
    @State private var updateSummary: String?
    @State private var updateFailed = false

    // Channel state. `channel` is what the segmented control shows; nil until
    // the first `llmux channel` read resolves (control disabled meanwhile).
    @State private var channel: ReleaseChannel?
    @State private var channelKnown = false
    @State private var channelRunning = false
    @State private var channelSummary: String?
    @State private var channelFailed = false

    // Confirmation dialog for the (destructive) channel switch.
    @State private var pendingChannel: ReleaseChannel?
    @State private var showConfirm = false

    init(snapshotState: NotchMenuSnapshotState? = nil, runner: CLIRunner = CLIRunner()) {
        self.snapshotState = snapshotState
        self.runner = runner
        _channel = State(initialValue: snapshotState?.releaseChannel)
        _channelKnown = State(initialValue: snapshotState != nil)
    }

    var body: some View {
        VStack(spacing: 4) {
            updateRow
            channelRow
        }
        .task {
            guard snapshotState == nil else { return }
            // Read the current channel once when the menu mounts.
            let current = await runner.currentChannel()
            channel = current
            channelKnown = true
        }
        .confirmationDialog(
            "Switch release channel?",
            isPresented: $showConfirm,
            titleVisibility: .visible
        ) {
            Button("Switch to \(pendingChannel?.label ?? "")") {
                if let target = pendingChannel { switchChannel(to: target) }
            }
            Button("Cancel", role: .cancel) { pendingChannel = nil }
        } message: {
            Text("This reinstalls llmux and relaunches LlmuxIslands from the \(pendingChannel?.label ?? "") channel.")
        }
    }

    // MARK: Update now row

    private var updateRow: some View {
        VStack(alignment: .leading, spacing: 2) {
            Button(action: runUpdate) {
                HStack(spacing: 10) {
                    Image(systemName: "arrow.down.circle")
                        .font(.system(size: 12))
                        .foregroundColor(.white.opacity(updateRunning ? 0.44 : 0.7))
                        .frame(width: 16)

                    Text("Update now")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundColor(.white.opacity(updateRunning ? 0.44 : 0.7))

                    Spacer()

                    if updateRunning {
                        ProcessingSpinner()
                        Text("Updating…")
                            .font(.system(size: 11))
                            .foregroundColor(.white.opacity(0.6))
                    }
                }
                .padding(.horizontal, 12)
                .frame(minHeight: 44)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(updateRunning)

            if let summary = updateSummary, !updateRunning {
                Text(summary)
                    .font(.system(size: 11))
                    .foregroundColor(updateFailed ? TerminalColors.red : .white.opacity(0.8))
                    .lineLimit(2)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 4)
            }
        }
    }

    // MARK: Release channel row

    private var channelRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Image(systemName: "shippingbox")
                    .font(.system(size: 12))
                    .foregroundColor(.white.opacity(0.7))
                    .frame(width: 16)

                Text("Release channel")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(.white.opacity(0.7))

                Spacer()

                if channelRunning {
                    ProcessingSpinner()
                } else if !channelKnown {
                    Text("…")
                        .font(.system(size: 11))
                        .foregroundColor(.white.opacity(0.5))
                }
            }

            HStack(spacing: 0) {
                ForEach(ReleaseChannel.allCases) { option in
                    let selected = channel == option
                    Button(option.label) {
                        channelBinding.wrappedValue = option
                    }
                    .font(.caption.weight(selected ? .semibold : .regular))
                    .foregroundColor(selected ? .black : .white.opacity(0.6))
                    .frame(maxWidth: .infinity, minHeight: 44)
                    .background(selected ? Color.white : Color.white.opacity(0.04))
                    .buttonStyle(.plain)
                }
            }
            .disabled(channelRunning || !channelKnown)

            if let summary = channelSummary, !channelRunning {
                Text(summary)
                    .font(.system(size: 11))
                    .foregroundColor(channelFailed ? TerminalColors.red : .white.opacity(0.8))
                    .lineLimit(2)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
    }

    /// The picker binds through this so a user change routes to the confirm
    /// dialog rather than switching immediately. Reading returns the current
    /// channel; writing stashes the target and opens the dialog.
    private var channelBinding: Binding<ReleaseChannel?> {
        Binding(
            get: { channel },
            set: { newValue in
                guard let target = newValue, target != channel else { return }
                pendingChannel = target
                showConfirm = true
            }
        )
    }

    // MARK: Actions

    private func runUpdate() {
        guard !updateRunning else { return }
        updateRunning = true
        updateSummary = nil
        Task {
            let outcome = await IslandUsageModel.shared.runMaintenanceUpdate(runner: runner)
            updateRunning = false
            updateSummary = outcome.summary
            updateFailed = outcome.isFailure
        }
    }

    private func switchChannel(to target: ReleaseChannel) {
        pendingChannel = nil
        channelRunning = true
        channelSummary = nil
        Task {
            let outcome = await IslandUsageModel.shared.changeReleaseChannel(to: target, runner: runner)
            channelRunning = false
            if outcome.isFailure {
                channelFailed = true
                channelSummary = outcome.summary
                // Revert the visible selection to the CLI's actual channel.
                channel = await runner.currentChannel()
            } else {
                channelFailed = false
                channel = target
                channelSummary = "Switched to \(target.label)"
            }
        }
    }
}

// MARK: - llmux Events Section

/// Collapsible editor for the daemon-owned operator events list (the
/// replacement for the single event banner). Lists `events[]` from the
/// dashboard doc (from→to rendered in local time, the currently-active window
/// marked green), and drives `POST /llmux/events` through the existing
/// shared semantic core — `{id, from, to, content}` = idempotent upsert,
/// `{"remove": id}` = idempotent remove — never the CLI. Same
/// section/row/inline-result patterns as its neighbors.
private struct LlmuxEventsSection: View {
    @State private var expanded = false
    @State private var isHovered = false

    @State private var events: [LlmuxEvent] = []
    @State private var loading = false
    @State private var busy = false            // an upsert/delete in flight
    @State private var summary: String?
    @State private var failed = false

    // Add/edit form. Prefilled when tapping an existing row.
    @State private var showForm = false
    @State private var formID = ""
    @State private var formFrom = ""
    @State private var formTo = ""
    @State private var formContent = ""

    // Delete confirmation (same pattern as the channel switch).
    @State private var pendingDelete: LlmuxEvent?
    @State private var showDeleteConfirm = false

    var body: some View {
        VStack(spacing: 6) {
            header

            if expanded {
                VStack(alignment: .leading, spacing: 6) {
                    if events.isEmpty && !loading {
                        Text("No events")
                            .font(.system(size: 11))
                            .foregroundColor(.white.opacity(0.6))
                            .padding(.horizontal, 4)
                    }

                    ForEach(events) { event in
                        eventRow(event)
                    }

                    if showForm {
                        form
                    } else {
                        Button { openForm(prefilledWith: nil) } label: {
                            Label("Add event", systemImage: "plus")
                                .font(.system(size: 11, weight: .semibold))
                                .frame(maxWidth: .infinity)
                                .frame(minHeight: 44)
                                .background(Color.white)
                                .foregroundColor(.black)
                        }
                        .buttonStyle(.plain)
                        .disabled(busy)
                    }

                    if let summary, !busy {
                        Text(summary)
                            .font(.system(size: 11))
                            .foregroundColor(failed ? TerminalColors.red : .white.opacity(0.8))
                            .lineLimit(2)
                            .padding(.horizontal, 4)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.bottom, 6)
            }
        }
        .confirmationDialog(
            "Delete event?",
            isPresented: $showDeleteConfirm,
            titleVisibility: .visible
        ) {
            Button("Delete \(pendingDelete?.id ?? "")", role: .destructive) {
                if let target = pendingDelete { deleteEvent(target) }
            }
            Button("Cancel", role: .cancel) { pendingDelete = nil }
        } message: {
            Text("Removes \"\(pendingDelete?.content ?? "")\" from the llmux daemon.")
        }
    }

    // MARK: Header

    private var header: some View {
        Button {
            expanded.toggle()
            if expanded {
                // Instant paint from the last dashboard poll, then refresh.
                events = IslandUsageModel.shared.dashboard?.events ?? events
                refresh()
            }
        } label: {
            HStack(spacing: 10) {
                Image(systemName: "megaphone")
                    .font(.system(size: 12))
                    .foregroundColor(.white.opacity(0.7))
                    .frame(width: 16)
                Text("Events")
                    .font(.system(size: 13, weight: .medium))
                    .foregroundColor(.white.opacity(isHovered ? 1.0 : 0.7))
                Spacer()
                if loading || busy {
                    ProcessingSpinner()
                } else if expanded {
                    Text("\(events.count)")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundColor(.white.opacity(0.6))
                }
                Image(systemName: expanded ? "chevron.up" : "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundColor(.white.opacity(0.5))
            }
            .padding(.horizontal, 12)
            .frame(minHeight: 44)
            .background(Color.white.opacity(isHovered ? 0.08 : 0))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
    }

    // MARK: Event row

    private func eventRow(_ event: LlmuxEvent) -> some View {
        let active = LlmuxEventTime.isActive(from: event.from, to: event.to, now: Date())
        return HStack(spacing: 8) {
            // Tapping the row prefills the edit form.
            Button { openForm(prefilledWith: event) } label: {
                HStack(spacing: 8) {
                    Image(systemName: active ? "bolt.fill" : "circle")
                        .font(.caption2.weight(.semibold))
                        .foregroundColor(active ? .white : .white.opacity(0.5))
                    VStack(alignment: .leading, spacing: 1) {
                        Text(event.content.isEmpty ? event.id : event.content)
                            .font(.system(size: 12, weight: .medium))
                            .foregroundColor(.white.opacity(0.85))
                            .lineLimit(1)
                        Text("\(event.id) · \(LlmuxEventTime.displayRange(from: event.from, to: event.to))\(active ? " · active" : "")")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundColor(.white.opacity(active ? 0.8 : 0.6))
                            .lineLimit(1)
                    }
                    Spacer()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            Button {
                pendingDelete = event
                showDeleteConfirm = true
            } label: {
                Image(systemName: "trash")
                    .font(.system(size: 10))
                    .foregroundColor(.white.opacity(0.5))
            }
            .buttonStyle(.plain)
            .disabled(busy)
        }
        .padding(.horizontal, 8)
        .frame(minHeight: 44)
        .background(Color.white.opacity(0.04))
    }

    // MARK: Form

    private var form: some View {
        VStack(spacing: 6) {
            field(placeholder: "id (e.g. 20260712-fable5)", text: $formID)
            HStack(spacing: 6) {
                field(placeholder: "from", text: $formFrom)
                field(placeholder: "to", text: $formTo)
            }
            field(placeholder: "content", text: $formContent)

            if !formValid {
                Text("id + content required · from/to: RFC3339 with offset or YYYYMMDDHHMM · from < to")
                    .font(.system(size: 10))
                    .foregroundColor(.white.opacity(0.6))
            }

            HStack(spacing: 6) {
                Button { showForm = false } label: {
                    Text("Cancel")
                        .font(.system(size: 11, weight: .semibold))
                        .frame(maxWidth: .infinity)
                        .frame(minHeight: 44)
                        .background(Color.white.opacity(0.07))
                        .foregroundColor(.white.opacity(0.7))
                }
                .buttonStyle(.plain)

                Button { save() } label: {
                    Text("Save")
                        .font(.system(size: 11, weight: .semibold))
                        .frame(maxWidth: .infinity)
                        .frame(minHeight: 44)
                        .background(formValid ? Color.white : Color.white.opacity(0.05))
                        .foregroundColor(formValid ? .black : .white.opacity(0.44))
                }
                .buttonStyle(.plain)
                .disabled(!formValid || busy)
            }
        }
        .padding(8)
        .background(Color.white.opacity(0.04))
    }

    /// Mirrors the daemon's 400-validation: non-empty id/content, parseable
    /// from/to (either accepted format), from < to.
    private var formValid: Bool {
        let id = formID.trimmingCharacters(in: .whitespacesAndNewlines)
        let content = formContent.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !id.isEmpty, !content.isEmpty,
              let from = LlmuxEventTime.parse(formFrom.trimmingCharacters(in: .whitespacesAndNewlines)),
              let to = LlmuxEventTime.parse(formTo.trimmingCharacters(in: .whitespacesAndNewlines))
        else { return false }
        return from < to
    }

    @ViewBuilder
    private func field(placeholder: String, text: Binding<String>) -> some View {
        TextField(placeholder, text: text)
            .textFieldStyle(.plain)
            .font(.system(size: 11, design: .monospaced))
            .foregroundColor(.white)
            .padding(7)
            .background(Color.white.opacity(0.07))
    }

    private func openForm(prefilledWith event: LlmuxEvent?) {
        formID = event?.id ?? ""
        formFrom = event?.from ?? ""
        formTo = event?.to ?? ""
        formContent = event?.content ?? ""
        summary = nil
        showForm = true
    }

    // MARK: Actions

    /// Re-read `events[]` from the dashboard doc.
    private func refresh() {
        guard !loading else { return }
        loading = true
        Task {
            let refreshed = await IslandUsageModel.shared.refreshEvents()
            if case .online = IslandUsageModel.shared.connection {
                events = refreshed
                failed = false
            } else {
                summary = "Could not refresh events."
                failed = true
            }
            loading = false
        }
    }

    /// `POST /llmux/events` — idempotent upsert by id.
    private func save() {
        guard formValid, !busy else { return }
        let event = LlmuxEvent(
            id: formID.trimmingCharacters(in: .whitespacesAndNewlines),
            from: formFrom.trimmingCharacters(in: .whitespacesAndNewlines),
            to: formTo.trimmingCharacters(in: .whitespacesAndNewlines),
            content: formContent.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        busy = true
        summary = nil
        Task {
            let saved = await IslandUsageModel.shared.upsertEvent(event)
            if saved {
                events = IslandUsageModel.shared.dashboard?.events ?? events
                failed = false
                summary = "Saved \(event.id)"
                showForm = false
            } else {
                failed = true
                summary = IslandUsageModel.shared.lastError ?? "Could not save event."
            }
            busy = false
        }
    }

    /// `POST /llmux/events` with `{"remove": id}` — idempotent remove.
    private func deleteEvent(_ event: LlmuxEvent) {
        pendingDelete = nil
        guard !busy else { return }
        busy = true
        summary = nil
        Task {
            let removed = await IslandUsageModel.shared.removeEvent(id: event.id)
            if removed {
                events = IslandUsageModel.shared.dashboard?.events ?? events.filter { $0.id != event.id }
                failed = false
                summary = "Removed \(event.id)"
            } else {
                failed = true
                summary = IslandUsageModel.shared.lastError ?? "Could not remove event."
            }
            busy = false
        }
    }
}

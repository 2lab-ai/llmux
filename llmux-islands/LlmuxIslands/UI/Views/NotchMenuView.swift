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

struct NotchMenuView: View {
    @ObservedObject var viewModel: NotchViewModel
    @ObservedObject private var screenSelector = ScreenSelector.shared
    @ObservedObject private var soundSelector = SoundSelector.shared
    @State private var launchAtLogin: Bool = false
    @AppStorage(AppSettings.emailAnonymousEnabledKey) private var emailAnonymousEnabled = false
    @AppStorage(AppSettings.showFableWeeklyKey) private var showFableWeekly = true

    static var appVersion: String {
        let v = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "0.0"
        return "v\(v)"
    }

    var body: some View {
        ScrollView(.vertical, showsIndicators: false) {
        VStack(spacing: 4) {
            // Navigation
            MenuRow(icon: "gauge.with.dots.needle.67percent", label: "Usage") {
                viewModel.showUsage()
            }

            // Statistics (issue #68 v2): the #62 analytics — overview totals,
            // models, clients, health — live here, NOT in the default Usage
            // panel (which stays the v0.2.14 account tile view).
            MenuRow(icon: "chart.bar.xaxis", label: "Statistics") {
                viewModel.showStats()
            }

            Divider()
                .background(Color.white.opacity(0.08))
                .padding(.vertical, 4)

            // Appearance settings
            ScreenPickerRow(screenSelector: screenSelector)
            SoundPickerRow(soundSelector: soundSelector)

            // Pixelize emails in the Usage area (todo item 3: "email anonymous").
            // The setting is SERVER-owned when the connected daemon supports it
            // (llmux 0.2.10+): the toggle POSTs `/llmux/settings` and the row
            // reflects the daemon's ack (mirrored into the @AppStorage key by
            // IslandUsageModel, so this row and every mosaic re-render). On an
            // older daemon the toggle stays local-only, exactly as before.
            MenuToggleRow(
                icon: "eye.slash",
                label: "Email anonymous",
                isOn: emailAnonymousEnabled
            ) {
                Task { await IslandUsageModel.shared.toggleEmailAnonymous() }
            }

            // Show the temporary Fable weekly (7d) usage row on each tile.
            // Local-only, default on (the upstream limit is short-lived, so the
            // row is opt-out rather than opt-in). Toggling flips the @AppStorage
            // key every UsageProviderColumn observes — no daemon round-trip.
            MenuToggleRow(
                icon: "calendar",
                label: "Show Fable weekly (7d)",
                isOn: showFableWeekly
            ) {
                showFableWeekly.toggle()
            }

            LlmuxConnectionSection()

            Divider()
                .background(Color.white.opacity(0.08))
                .padding(.vertical, 4)

            // llmux CLI maintenance: self-update + release channel switch.
            LlmuxMaintenanceSection()

            // Operator events (daemon-owned list, /llmux/events).
            LlmuxEventsSection()

            Divider()
                .background(Color.white.opacity(0.08))
                .padding(.vertical, 4)

            // System settings
            MenuToggleRow(
                icon: "power",
                label: "Launch at Login",
                isOn: launchAtLogin
            ) {
                do {
                    if launchAtLogin {
                        try SMAppService.mainApp.unregister()
                        launchAtLogin = false
                    } else {
                        try SMAppService.mainApp.register()
                        launchAtLogin = true
                    }
                } catch {
                    print("Failed to toggle launch at login: \(error)")
                }
            }

            AccessibilityRow(isEnabled: AXIsProcessTrusted())

            Divider()
                .background(Color.white.opacity(0.08))
                .padding(.vertical, 4)

            // About
            MenuRow(icon: "info.circle", label: "llmux-islands \(Self.appVersion)") {
                if let url = URL(string: "https://github.com/2lab-ai/llmux/releases") {
                    NSWorkspace.shared.open(url)
                }
            }

            MenuRow(
                icon: "star",
                label: "llmux on GitHub"
            ) {
                if let url = URL(string: "https://github.com/2lab-ai/llmux") {
                    NSWorkspace.shared.open(url)
                }
            }

            Divider()
                .background(Color.white.opacity(0.08))
                .padding(.vertical, 4)

            MenuRow(
                icon: "xmark.circle",
                label: "Quit",
                isDestructive: true
            ) {
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
    }

    private func refreshStates() {
        launchAtLogin = SMAppService.mainApp.status == .enabled
        screenSelector.refreshScreens()
    }
}

// MARK: - Update Row (removed — no Sparkle in llmux-islands)

// MARK: - Accessibility Permission Row

struct AccessibilityRow: View {
    let isEnabled: Bool

    @State private var isHovered = false
    @State private var refreshTrigger = false

    private var currentlyEnabled: Bool {
        // Re-check on each render when refreshTrigger changes
        _ = refreshTrigger
        return isEnabled
    }

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
                Circle()
                    .fill(TerminalColors.green)
                    .frame(width: 6, height: 6)

                Text("On")
                    .font(.system(size: 11))
                    .foregroundColor(.white.opacity(0.4))
            } else {
                Button(action: openAccessibilitySettings) {
                    Text("Enable")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundColor(.black)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(
                            RoundedRectangle(cornerRadius: 5)
                                .fill(Color.white)
                        )
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(
            RoundedRectangle(cornerRadius: 8)
                .fill(isHovered ? Color.white.opacity(0.08) : Color.clear)
        )
        .onHover { isHovered = $0 }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            refreshTrigger.toggle()
        }
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
            .padding(.vertical, 10)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(isHovered ? Color.white.opacity(0.08) : Color.clear)
            )
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

                Circle()
                    .fill(isOn ? TerminalColors.green : Color.white.opacity(0.3))
                    .frame(width: 6, height: 6)

                Text(isOn ? "On" : "Off")
                    .font(.system(size: 11))
                    .foregroundColor(.white.opacity(0.4))
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(isHovered ? Color.white.opacity(0.08) : Color.clear)
            )
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
    @State private var host: String = LlmuxSettings.host
    @State private var port: String = String(LlmuxSettings.port)
    @State private var apiKey: String = LlmuxSettings.apiKey
    @State private var expanded = false
    @State private var isHovered = false

    var body: some View {
        VStack(spacing: 6) {
            Button {
                withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() }
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
                        .foregroundColor(.white.opacity(0.4))
                        .lineLimit(1)
                    Image(systemName: expanded ? "chevron.up" : "chevron.down")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundColor(.white.opacity(0.4))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(
                    RoundedRectangle(cornerRadius: 8)
                        .fill(Color.white.opacity(isHovered ? 0.06 : 0))
                )
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
                        field(placeholder: "API key (optional)", text: $apiKey, secure: true)
                    }
                    Button { apply() } label: {
                        Text("Apply & reconnect")
                            .font(.system(size: 11, weight: .semibold))
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 7)
                            .background(RoundedRectangle(cornerRadius: 6).fill(Color.white.opacity(0.12)))
                            .foregroundColor(.white)
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
        .background(RoundedRectangle(cornerRadius: 6).fill(Color.white.opacity(0.07)))
    }

    private func apply() {
        let h = host.trimmingCharacters(in: .whitespacesAndNewlines)
        LlmuxSettings.host = h.isEmpty ? "127.0.0.1" : h
        LlmuxSettings.port = Int(port.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 3456
        LlmuxSettings.apiKey = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
        host = LlmuxSettings.host
        port = String(LlmuxSettings.port)
        Task { await IslandUsageModel.shared.refresh() }
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

    var body: some View {
        VStack(spacing: 4) {
            updateRow
            channelRow
        }
        .task {
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
                        .foregroundColor(.white.opacity(updateRunning ? 0.4 : 0.7))
                        .frame(width: 16)

                    Text("Update now")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundColor(.white.opacity(updateRunning ? 0.4 : 0.7))

                    Spacer()

                    if updateRunning {
                        ProcessingSpinner()
                        Text("Updating…")
                            .font(.system(size: 11))
                            .foregroundColor(.white.opacity(0.4))
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
                .background(RoundedRectangle(cornerRadius: 8).fill(Color.clear))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(updateRunning)

            if let summary = updateSummary, !updateRunning {
                Text(summary)
                    .font(.system(size: 11))
                    .foregroundColor(updateFailed ? TerminalColors.red : TerminalColors.green)
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
                        .foregroundColor(.white.opacity(0.4))
                }
            }

            Picker("Release channel", selection: channelBinding) {
                ForEach(ReleaseChannel.allCases) { option in
                    Text(option.label).tag(Optional(option))
                }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .disabled(channelRunning || !channelKnown)

            if let summary = channelSummary, !channelRunning {
                Text(summary)
                    .font(.system(size: 11))
                    .foregroundColor(channelFailed ? TerminalColors.red : TerminalColors.green)
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
            let outcome = await runner.update()
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
            let outcome = await runner.setChannel(target)
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
/// `LlmuxClient` — `{id, from, to, content}` = idempotent upsert,
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
                            .foregroundColor(.white.opacity(0.4))
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
                                .padding(.vertical, 7)
                                .background(RoundedRectangle(cornerRadius: 6).fill(Color.white.opacity(0.12)))
                                .foregroundColor(.white)
                        }
                        .buttonStyle(.plain)
                        .disabled(busy)
                    }

                    if let summary, !busy {
                        Text(summary)
                            .font(.system(size: 11))
                            .foregroundColor(failed ? TerminalColors.red : TerminalColors.green)
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
            withAnimation(.easeInOut(duration: 0.15)) { expanded.toggle() }
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
                        .foregroundColor(.white.opacity(0.4))
                }
                Image(systemName: expanded ? "chevron.up" : "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundColor(.white.opacity(0.4))
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color.white.opacity(isHovered ? 0.06 : 0))
            )
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
                    Circle()
                        .fill(active ? TerminalColors.green : Color.white.opacity(0.2))
                        .frame(width: 6, height: 6)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(event.content.isEmpty ? event.id : event.content)
                            .font(.system(size: 12, weight: .medium))
                            .foregroundColor(.white.opacity(0.85))
                            .lineLimit(1)
                        Text("\(event.id) · \(LlmuxEventTime.displayRange(from: event.from, to: event.to))\(active ? " · active" : "")")
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundColor(active ? TerminalColors.green.opacity(0.9) : .white.opacity(0.4))
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
        .padding(.vertical, 5)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color.white.opacity(0.04)))
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
                    .foregroundColor(.white.opacity(0.4))
            }

            HStack(spacing: 6) {
                Button { showForm = false } label: {
                    Text("Cancel")
                        .font(.system(size: 11, weight: .semibold))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: 6).fill(Color.white.opacity(0.07)))
                        .foregroundColor(.white.opacity(0.7))
                }
                .buttonStyle(.plain)

                Button { save() } label: {
                    Text("Save")
                        .font(.system(size: 11, weight: .semibold))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: 6).fill(Color.white.opacity(formValid ? 0.12 : 0.05)))
                        .foregroundColor(.white.opacity(formValid ? 1.0 : 0.4))
                }
                .buttonStyle(.plain)
                .disabled(!formValid || busy)
            }
        }
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 6).fill(Color.white.opacity(0.04)))
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
            .background(RoundedRectangle(cornerRadius: 6).fill(Color.white.opacity(0.07)))
    }

    private func openForm(prefilledWith event: LlmuxEvent?) {
        formID = event?.id ?? ""
        formFrom = event?.from ?? ""
        formTo = event?.to ?? ""
        formContent = event?.content ?? ""
        summary = nil
        withAnimation(.easeInOut(duration: 0.15)) { showForm = true }
    }

    // MARK: Actions

    /// Re-read `events[]` from the dashboard doc.
    private func refresh() {
        guard !loading else { return }
        loading = true
        Task {
            do {
                events = try await LlmuxClient.current().dashboard().events
                failed = false
            } catch {
                summary = error.localizedDescription
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
            do {
                if let echoed = try await LlmuxClient.current().upsertEvent(event) {
                    events = echoed
                } else if let idx = events.firstIndex(where: { $0.id == event.id }) {
                    events[idx] = event
                } else {
                    events.append(event)
                }
                failed = false
                summary = "Saved \(event.id)"
                showForm = false
            } catch {
                failed = true
                summary = error.localizedDescription
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
            do {
                if let echoed = try await LlmuxClient.current().removeEvent(id: event.id) {
                    events = echoed
                } else {
                    events.removeAll { $0.id == event.id }
                }
                failed = false
                summary = "Removed \(event.id)"
            } catch {
                failed = true
                summary = error.localizedDescription
            }
            busy = false
        }
    }
}

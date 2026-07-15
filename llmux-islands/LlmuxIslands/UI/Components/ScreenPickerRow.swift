//
//  ScreenPickerRow.swift
//  ClaudeIsland
//
//  Screen selection picker for settings menu
//

import SwiftUI

struct ScreenPickerRow: View {
    @ObservedObject var screenSelector: ScreenSelector
    var snapshotSelectionLabel: String? = nil
    @State private var isHovered = false

    private var isExpanded: Bool {
        get { snapshotSelectionLabel == nil && screenSelector.isPickerExpanded }
    }

    private func setExpanded(_ value: Bool) {
        guard snapshotSelectionLabel == nil else { return }
        screenSelector.isPickerExpanded = value
    }

    var body: some View {
        VStack(spacing: 0) {
            // Main row - shows current selection
            Button {
                setExpanded(!isExpanded)
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "display")
                        .font(.system(size: 12))
                        .foregroundColor(textColor)
                        .frame(width: 16)

                    Text("Screen")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundColor(textColor)

                    Spacer()

                    Text(currentSelectionLabel)
                        .font(.system(size: 11))
                        .foregroundColor(.white.opacity(0.6))
                        .lineLimit(1)

                    Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                        .font(.system(size: 10))
                        .foregroundColor(.white.opacity(0.5))
                }
                .padding(.horizontal, 12)
                .frame(minHeight: 44)
                .background(isHovered ? Color.white.opacity(0.08) : Color.clear)
            }
            .buttonStyle(.plain)
            .onHover { isHovered = $0 }

            // Expanded screen list
            if isExpanded {
                VStack(spacing: 2) {
                    // Automatic option
                    ScreenOptionRow(
                        label: "Automatic",
                        sublabel: "Built-in or Main",
                        isSelected: screenSelector.selectionMode == .automatic
                    ) {
                        Task { await IslandUsageModel.shared.selectScreen(nil) }
                        collapseAfterDelay()
                    }

                    // Individual screens
                    ForEach(screenSelector.availableScreens, id: \.self) { screen in
                        ScreenOptionRow(
                            label: screen.localizedName,
                            sublabel: screenSublabel(for: screen),
                            isSelected: screenSelector.selectionMode == .specificScreen &&
                                       screenSelector.isSelected(screen)
                        ) {
                            Task { await IslandUsageModel.shared.selectScreen(screen) }
                            collapseAfterDelay()
                        }
                    }
                }
                .padding(.leading, 28)
                .padding(.top, 4)
            }
        }
    }

    private var currentSelectionLabel: String {
        if let snapshotSelectionLabel { return snapshotSelectionLabel }
        switch screenSelector.selectionMode {
        case .automatic:
            return "Auto"
        case .specificScreen:
            if let screen = screenSelector.selectedScreen {
                return screen.localizedName
            }
            return "Auto"
        }
    }

    private var textColor: Color {
        .white.opacity(isHovered ? 1.0 : 0.7)
    }

    private func screenSublabel(for screen: NSScreen) -> String? {
        var parts: [String] = []
        if screen.isBuiltinDisplay {
            parts.append("Built-in")
        }
        if screen == NSScreen.main {
            parts.append("Main")
        }
        return parts.isEmpty ? nil : parts.joined(separator: ", ")
    }

    private func collapseAfterDelay() {
        setExpanded(false)
    }
}

// MARK: - Screen Option Row

private struct ScreenOptionRow: View {
    let label: String
    let sublabel: String?
    let isSelected: Bool
    let action: () -> Void

    @State private var isHovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: isSelected ? "checkmark" : "circle")
                    .font(.caption2.weight(.semibold))
                    .foregroundColor(isSelected ? .black : .white.opacity(0.5))

                VStack(alignment: .leading, spacing: 1) {
                    Text(label)
                        .font(.system(size: 12, weight: .medium))
                        .foregroundColor(isSelected ? .black : .white.opacity(isHovered ? 1.0 : 0.7))

                    if let sublabel = sublabel {
                        Text(sublabel)
                            .font(.system(size: 10))
                            .foregroundColor(isSelected ? .black.opacity(0.6) : .white.opacity(0.6))
                    }
                }

                Spacer()

            }
            .padding(.horizontal, 10)
            .frame(minHeight: 44)
            .background(isSelected ? Color.white : isHovered ? Color.white.opacity(0.08) : Color.clear)
        }
        .buttonStyle(.plain)
        .onHover { isHovered = $0 }
    }
}

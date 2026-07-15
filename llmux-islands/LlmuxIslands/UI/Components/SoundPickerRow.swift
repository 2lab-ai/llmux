//
//  SoundPickerRow.swift
//  ClaudeIsland
//
//  Notification sound selection picker for settings menu
//

import AppKit
import SwiftUI

struct SoundPickerRow: View {
    @ObservedObject var soundSelector: SoundSelector
    var snapshotSelectionLabel: String? = nil
    @State private var isHovered = false
    @State private var selectedSound: NotificationSound = AppSettings.notificationSound

    private var isExpanded: Bool {
        snapshotSelectionLabel == nil && soundSelector.isPickerExpanded
    }

    private func setExpanded(_ value: Bool) {
        guard snapshotSelectionLabel == nil else { return }
        soundSelector.isPickerExpanded = value
    }

    var body: some View {
        VStack(spacing: 0) {
            // Main row - shows current selection
            Button {
                setExpanded(!isExpanded)
            } label: {
                HStack(spacing: 10) {
                    Image(systemName: "speaker.wave.2")
                        .font(.system(size: 12))
                        .foregroundColor(textColor)
                        .frame(width: 16)

                    Text("Notification Sound")
                        .font(.system(size: 13, weight: .medium))
                        .foregroundColor(textColor)

                    Spacer()

                    Text(snapshotSelectionLabel ?? selectedSound.rawValue)
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

            // Expanded sound list
            if isExpanded {
                ScrollView {
                    VStack(spacing: 2) {
                        ForEach(NotificationSound.allCases, id: \.self) { sound in
                            SoundOptionRowInline(
                                sound: sound,
                                isSelected: selectedSound == sound
                            ) {
                                // Play preview sound
                                if let soundName = sound.soundName {
                                    NSSound(named: soundName)?.play()
                                }
                                Task {
                                    await IslandUsageModel.shared.selectSound(sound)
                                    selectedSound = AppSettings.notificationSound
                                }
                            }
                        }
                    }
                }
                .frame(maxHeight: CGFloat(min(NotificationSound.allCases.count, 6)) * 32)
                .padding(.leading, 28)
                .padding(.top, 4)
            }
        }
        .onAppear {
            guard snapshotSelectionLabel == nil else { return }
            selectedSound = AppSettings.notificationSound
        }
    }

    private var textColor: Color {
        .white.opacity(isHovered ? 1.0 : 0.7)
    }
}

// MARK: - Sound Option Row (Inline version)

private struct SoundOptionRowInline: View {
    let sound: NotificationSound
    let isSelected: Bool
    let action: () -> Void

    @State private var isHovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Image(systemName: isSelected ? "checkmark" : "circle")
                    .font(.caption2.weight(.semibold))
                    .foregroundColor(isSelected ? .black : .white.opacity(0.5))

                Text(sound.rawValue)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundColor(isSelected ? .black : .white.opacity(isHovered ? 1.0 : 0.7))

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

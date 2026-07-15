//
//  ProcessingSpinner.swift
//  ClaudeIsland
//
//  Animated symbol spinner for processing state
//

import Combine
import SwiftUI

struct ProcessingSpinner: View {
    @State private var phase: Int = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private let symbols = ["·", "✢", "✳", "∗", "✻", "✽"]

    private let timer = Timer.publish(every: 0.15, on: .main, in: .common).autoconnect()

    var body: some View {
        Text(reduceMotion ? "…" : symbols[phase % symbols.count])
            .font(.system(size: 12, weight: .bold))
            .foregroundColor(.white.opacity(0.6))
            .frame(width: 12, alignment: .center)
            .onReceive(timer) { _ in
                if !reduceMotion {
                    phase = (phase + 1) % symbols.count
                }
            }
    }
}

#if DEBUG && canImport(PreviewsMacros)
#Preview {
    ProcessingSpinner()
        .frame(width: 30, height: 30)
        .background(.black)
}
#endif

//
//  SnapshotMode.swift
//  LlmuxIslands
//
//  Offscreen snapshot mode for visual verification on hosts without Screen
//  Recording permission and without disturbing a running production island.
//  When `LLMUX_ISLANDS_SNAPSHOT_DIR=<abs dir>` is set at launch the app
//  creates NO window and never touches the llmux daemon — it renders the
//  requested PNGs (2x), prints the written paths to stdout, and exits 0.
//  When the variable is absent this is a strict no-op and the app launches
//  normally.
//
//  Dispatch rule (one gate, two artifact families):
//  - `LLMUX_ISLANDS_SNAPSHOT_KIND=label|menu|usage|stats` selects explicitly.
//  - When KIND is unset: the **label** family renders if a label-ish env is
//    present (`LLMUX_ISLANDS_DEMO_INFLIGHT` or `LLMUX_ISLANDS_SNAPSHOT_T`);
//    otherwise the **menu + usage + stats** family renders.
//
//  Label family — the closed-island pill (NotchClosedLabelContent) at 4 fixed
//  animation phases. Session counts come from `LLMUX_ISLANDS_DEMO_INFLIGHT`
//  (DemoMode); relaunch once per counts-state. Output:
//  `label-c{claude}x{codex}-p{0..3}.png` where p0..p3 = phase 0 / 0.25 / 0.5 /
//  0.75 of the rainbow hue loop (issue #68 removed the mascot jump — the
//  phases now vary only the counter hues). Wall-clock mode: setting
//  `LLMUX_ISLANDS_SNAPSHOT_T=<seconds>` renders ONE frame at that absolute
//  time instead of the 4 normalized phases (`rainbowHue(time:)`). Output:
//  `t{t*100 as %03d}-c{claude}.png` (e.g. t=0.3, claude=3 → `t030-c3.png`).
//
//  Menu + usage + stats family — each common-path surface plus an explicitly
//  expanded Advanced counterpart, and a production receipt-detail route.
//  Fixture accounts use demo-safe identities. Force the anonymous state
//  per-process WITHOUT touching the shared defaults domain by launching with
//  `-emailAnonymousEnabled YES` / `NO` (volatile argument domain).
//

import AppKit
import SwiftUI

enum SnapshotMode {
    /// Output directory from the environment; snapshot mode is active iff set.
    static let directory: String? = {
        guard let dir = ProcessInfo.processInfo.environment["LLMUX_ISLANDS_SNAPSHOT_DIR"],
              !dir.isEmpty
        else { return nil }
        return dir
    }()

    /// Read from nonisolated view code (EmailPixelized precomputes its mosaic
    /// synchronously in snapshot mode) — keep this enum nonisolated and put
    /// `@MainActor` on the render functions instead.
    static var isActive: Bool { directory != nil }

    /// PNG scale factor (2x, Retina-like).
    static let scale: CGFloat = 2

    /// Jump-cycle phases rendered per counts-state, in filename order p0..p3.
    static let phases: [Double] = [0, 0.25, 0.5, 0.75]

    /// Artifact families selectable via `LLMUX_ISLANDS_SNAPSHOT_KIND`.
    enum Kind: String {
        case label
        case menu
        case usage
        case stats
    }

    enum SnapshotError: Error, CustomStringConvertible {
        case renderFailed(String)
        case pngEncodeFailed(String)
        case invalidWallClock(String)
        case invalidKind(String)
        case missingFixture(String)
        case unexpectedSurfaceSet

        var description: String {
            switch self {
            case .renderFailed(let file):
                return "renderer produced no image for \(file)"
            case .pngEncodeFailed(let file):
                return "PNG encoding failed for \(file)"
            case .invalidWallClock(let raw):
                return "LLMUX_ISLANDS_SNAPSHOT_T must be a non-negative number of seconds, got \"\(raw)\""
            case .invalidKind(let raw):
                return "LLMUX_ISLANDS_SNAPSHOT_KIND must be label|menu|usage|stats, got \"\(raw)\""
            case .missingFixture(let file):
                return "snapshot fixture is missing from the app bundle: \(file)"
            case .unexpectedSurfaceSet:
                return "opened-panel snapshot output did not match the exact seven-surface contract"
            }
        }
    }

    /// Called first thing in `applicationDidFinishLaunching`, before any
    /// window or daemon work. Returns immediately (no side effects) when
    /// `LLMUX_ISLANDS_SNAPSHOT_DIR` is unset; otherwise writes the PNGs and
    /// terminates the process (exit 0 on success, 1 on failure).
    @MainActor
    static func runIfRequested() {
        guard let dir = directory else { return }

        do {
            let paths = try renderAll(into: URL(fileURLWithPath: dir, isDirectory: true))
            for path in paths {
                print(path)
            }
            exit(0)
        } catch {
            FileHandle.standardError.write(Data("snapshot mode failed: \(error)\n".utf8))
            exit(1)
        }
    }

    /// Render the artifacts selected by the dispatch rule (file header) and
    /// return the absolute paths written.
    @MainActor
    static func renderAll(into dir: URL) throws -> [String] {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)

        let kinds = try requestedKinds()
        if kinds.contains(where: { $0 != .label }) {
            normalizeSnapshotLaunchArguments()
        }

        var written: [String] = []
        for kind in kinds {
            switch kind {
            case .label: written += try renderLabelFrames(into: dir)
            case .menu: written += try renderMenu(into: dir)
            case .usage: written += try renderUsage(into: dir)
            case .stats: written += try renderStats(into: dir)
            }
        }
        if kinds == [.menu, .usage, .stats] {
            let actual = Set(written.map { URL(fileURLWithPath: $0).lastPathComponent })
            let expected = Set(IslandPresentationPolicy.snapshotSurfaceFiles(
                emailAnonymous: AppSettings.emailAnonymousEnabled
            ))
            guard actual == expected, written.count == expected.count else {
                throw SnapshotError.unexpectedSurfaceSet
            }
        }
        return written
    }

    /// The dispatch rule: explicit KIND wins; otherwise label-ish envs select
    /// the label family, and the menu + usage + stats family is the default.
    static func requestedKinds(
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) throws -> [Kind] {
        if let raw = environment["LLMUX_ISLANDS_SNAPSHOT_KIND"], !raw.isEmpty {
            guard let kind = Kind(rawValue: raw.lowercased()) else {
                throw SnapshotError.invalidKind(raw)
            }
            return [kind]
        }
        let labelish = ["LLMUX_ISLANDS_DEMO_INFLIGHT", "LLMUX_ISLANDS_SNAPSHOT_T"]
        if labelish.contains(where: { environment[$0]?.isEmpty == false }) {
            return [.label]
        }
        return [.menu, .usage, .stats]
    }

    // MARK: - Label family (closed-island pill)

    /// Render the requested label frames for the current (env-forced) counts:
    /// one wall-clock frame when `LLMUX_ISLANDS_SNAPSHOT_T` is set, else the
    /// 4 fixed phases.
    @MainActor
    private static func renderLabelFrames(into dir: URL) throws -> [String] {
        let claude = DemoMode.forcedInFlight?.claude ?? 0
        let codex = DemoMode.forcedInFlight?.codex ?? 0

        if let raw = ProcessInfo.processInfo.environment["LLMUX_ISLANDS_SNAPSHOT_T"], !raw.isEmpty {
            guard let t = TimeInterval(raw), t >= 0, t.isFinite else {
                throw SnapshotError.invalidWallClock(raw)
            }
            let name = String(format: "t%03d-c%d.png", Int((t * 100).rounded()), claude)
            let url = dir.appendingPathComponent(name)
            let view = ClosedIslandSnapshotView(
                claudeCount: claude, codexCount: codex, grokCount: 0, clock: .wallClock(t)
            )
            try renderLabel(view, to: url)
            return [url.path]
        }

        var written: [String] = []
        for (index, phase) in phases.enumerated() {
            let url = dir.appendingPathComponent("label-c\(claude)x\(codex)-p\(index).png")
            let view = ClosedIslandSnapshotView(
                claudeCount: claude, codexCount: codex, grokCount: 0, clock: .phase(phase)
            )
            try renderLabel(view, to: url)
            written.append(url.path)
        }

        return written
    }

    @MainActor
    private static func renderLabel(_ view: ClosedIslandSnapshotView, to url: URL) throws {
        let renderer = ImageRenderer(content: view)
        renderer.scale = scale
        guard let cgImage = renderer.cgImage else {
            throw SnapshotError.renderFailed(url.lastPathComponent)
        }
        try writePNG(NSBitmapImageRep(cgImage: cgImage), to: url)
    }

    // MARK: - Menu + usage family (opened panels)

    /// Settings in both its common-path default and explicit Advanced state.
    @MainActor
    private static func renderMenu(into dir: URL) throws -> [String] {
        let model = IslandUsageModel.shared
        let fixture = try fixtureDashboard()
        try installCanonicalStatsFixture(fixture, into: model)

        let viewModel = makeOpenedViewModel(contentType: .menu)
        let url = dir.appendingPathComponent("menu.png")
        try writeHosted(
            view: NotchView(viewModel: viewModel, snapshotNow: fixture.now, forceVisible: true),
            size: viewModel.openedSize,
            to: url
        )

        let advancedViewModel = makeOpenedViewModel(
            contentType: .menu,
            openedSizeOverride: CGSize(width: 500, height: 820)
        )
        let advancedURL = dir.appendingPathComponent("menu-advanced.png")
        try writeHosted(
            view: NotchView(
                viewModel: advancedViewModel,
                snapshotAdvancedInitiallyPresented: true,
                snapshotNow: fixture.now,
                forceVisible: true
            ),
            size: advancedViewModel.openedSize,
            to: advancedURL
        )
        return [url.path, advancedURL.path]
    }

    /// Usage in both its common-path default and explicit Advanced state,
    /// named after the effective email-anonymous state.
    @MainActor
    private static func renderUsage(into dir: URL) throws -> [String] {
        let model = IslandUsageModel.shared
        let fixture = try fixtureDashboard()
        try installCanonicalStatsFixture(fixture, into: model)

        let viewModel = makeOpenedViewModel(contentType: .usage)
        let anonOn = AppSettings.emailAnonymousEnabled
        let url = dir.appendingPathComponent(anonOn ? "usage-anon-on.png" : "usage-anon-off.png")
        try writeHosted(
            view: NotchView(viewModel: viewModel, snapshotNow: fixture.now, forceVisible: true),
            size: viewModel.openedSize,
            to: url
        )

        let advancedViewModel = makeOpenedViewModel(
            contentType: .usage,
            openedSizeOverride: CGSize(width: 560, height: 620)
        )
        let advancedURL = dir.appendingPathComponent("usage-advanced.png")
        try writeHosted(
            view: NotchView(
                viewModel: advancedViewModel,
                snapshotAdvancedInitiallyPresented: true,
                snapshotNow: fixture.now,
                forceVisible: true
            ),
            size: advancedViewModel.openedSize,
            to: advancedURL
        )
        return [url.path, advancedURL.path]
    }

    /// Statistics in common, Advanced analytics, and Advanced receipts states.
    @MainActor
    private static func renderStats(into dir: URL) throws -> [String] {
        // Deterministic dashboard document (issue #62 S4): fallback
        // data-quality labels (no `data_quality` key), an auth_failed account
        // (banner + health dot), same-named models in both groups ((group,
        // model) keying), and zero/absent cache + client cost fields
        // (omitted, never `—`).
        let model = IslandUsageModel.shared
        let fixture = try fixtureDashboard()
        try installCanonicalStatsFixture(fixture, into: model)
        guard model.dashboard != nil else {
            throw SharedUiCoreError.invalidOutput
        }

        let viewModel = makeOpenedViewModel(contentType: .stats)
        let url = dir.appendingPathComponent("stats.png")
        try writeHosted(
            view: NotchView(viewModel: viewModel, snapshotNow: fixture.now, forceVisible: true),
            size: viewModel.openedSize,
            to: url
        )
        var written = [url.path]

        let advancedViewModel = makeOpenedViewModel(
            contentType: .stats,
            openedSizeOverride: CGSize(width: 560, height: 860)
        )
        let advancedURL = dir.appendingPathComponent("stats-advanced.png")
        try writeHosted(
            view: NotchView(
                viewModel: advancedViewModel,
                snapshotAdvancedInitiallyPresented: true,
                snapshotNow: fixture.now,
                forceVisible: true
            ),
            size: advancedViewModel.openedSize,
            to: advancedURL
        )
        written.append(advancedURL.path)

        // The exact production Statistics route opens Advanced and selects its
        // Activity & receipts page, with the same outer shell chrome as a user.
        let receiptsURL = dir.appendingPathComponent("receipts-detail.png")
        let receiptViewModel = makeOpenedViewModel(
            contentType: .stats,
            openedSizeOverride: CGSize(width: 560, height: 700)
        )
        try writeHosted(
            view: NotchView(
                viewModel: receiptViewModel,
                snapshotAdvancedInitiallyPresented: true,
                snapshotStatisticsPage: .receipts,
                snapshotNow: fixture.now,
                forceVisible: true
            ),
            size: receiptViewModel.openedSize,
            to: receiptsURL
        )
        written.append(receiptsURL.path)
        return written
    }

    private struct StatsFixture {
        let now: Date
        let nowMs: UInt64
        let dashboardJSON: Data
    }

    /// Hydrate and mutate the real shared reducer so both receipt families in
    /// the screenshot come from the same bridge path as the live app.
    @MainActor
    private static func installCanonicalStatsFixture(
        _ fixture: StatsFixture,
        into model: IslandUsageModel
    ) throws {
        let runtime = try SharedUiCoreRuntime(configuration: .init(
            endpointDisplay: "http://127.0.0.1:3456",
            remote: false,
            authenticated: true,
            apiKeyConfigured: false,
            selectedScreenID: "snapshot-display",
            soundID: "default",
            showFableWeekly: true,
            presentation: "regular"
        ))
        let refresh = try runtime.dispatch([
            "type": "refresh_requested",
            "source": "startup",
        ])
        guard let initialRequestID = refresh.effects.compactMap(\.dashboardRequestID).first else {
            throw SharedUiCoreError.invalidOutput
        }
        _ = try runtime.applyDashboard(
            requestID: initialRequestID,
            dashboardJSON: fixture.dashboardJSON,
            receivedAtMs: fixture.nowMs
        )

        let operationID = "snapshot-settings-readback"
        _ = try runtime.dispatch([
            "type": "operation_started",
            "id": operationID,
            "request": [
                "kind": "persist_show_fable",
                "enabled": true,
            ],
            "target_display": "Fable weekly quota",
            "started_at_ms": fixture.nowMs + 100,
        ])
        let finished = try runtime.dispatch([
            "type": "operation_finished",
            "id": operationID,
            "outcome": "succeeded",
            "message": "Preference saved and verified by daemon readback",
            "finished_at_ms": fixture.nowMs + 150,
        ])
        guard let readbackRequestID = finished.effects.compactMap(\.dashboardRequestID).first else {
            throw SharedUiCoreError.invalidOutput
        }
        let readback = try runtime.applyDashboard(
            requestID: readbackRequestID,
            dashboardJSON: fixture.dashboardJSON,
            receivedAtMs: fixture.nowMs + 200
        )
        model.installSnapshotFixtureState(readback.state)
    }

    /// Validated shared dashboard contract fixture with one fixed display
    /// clock so "ago" columns and PNG pixels are stable across captures.
    private static func fixtureDashboard() throws -> StatsFixture {
        // This is the same validated document used by the Rust core/bridge
        // contract tests. Keeping it as a bundle resource prevents a visually
        // convenient but schema-invalid Swift-only fixture from drifting.
        let resource = "snapshot-dashboard"
        guard let url = Bundle.main.url(forResource: resource, withExtension: "json") else {
            throw SnapshotError.missingFixture("\(resource).json")
        }
        let nowMs: UInt64 = 1_700_000_010_000
        let now = Date(timeIntervalSince1970: TimeInterval(nowMs) / 1000)
        return StatsFixture(
            now: now,
            nowMs: nowMs,
            dashboardJSON: try Data(contentsOf: url)
        )
    }

    /// Plausible 16" laptop geometry so `openedSize` matches the app.
    @MainActor
    private static func makeOpenedViewModel(
        contentType: NotchContentType,
        openedSizeOverride: CGSize? = nil
    ) -> NotchViewModel {
        let viewModel = NotchViewModel(
            deviceNotchRect: CGRect(x: 764, y: 1085, width: 200, height: 32),
            screenRect: CGRect(x: 0, y: 0, width: 1728, height: 1117),
            windowHeight: 800,
            hasPhysicalNotch: true,
            openedSizeOverride: openedSizeOverride
        )
        viewModel.contentType = contentType
        viewModel.status = .opened
        return viewModel
    }

    /// Snapshot evidence uses only volatile, per-process preferences. Coerce
    /// the requested privacy flag to a real Bool for `@AppStorage`, and pin the
    /// canonical fixture's Fable visibility so host defaults cannot change any
    /// Usage/Statistics pixels. Persisted user defaults are never touched.
    private static func normalizeSnapshotLaunchArguments() {
        let emailAnonymous = AppSettings.emailAnonymousEnabled
        var argumentDomain = UserDefaults.standard.volatileDomain(forName: UserDefaults.argumentDomain)
        argumentDomain[AppSettings.emailAnonymousEnabledKey] = emailAnonymous
        argumentDomain[AppSettings.showFableWeeklyKey] = true
        UserDefaults.standard.setVolatileDomain(argumentDomain, forName: UserDefaults.argumentDomain)
    }

    /// Render `view` at `size` (island content on the island's black backdrop)
    /// into a PNG at `url`.
    ///
    /// Uses an offscreen `NSHostingView` + `cacheDisplay`, not `ImageRenderer`:
    /// `ImageRenderer` cannot rasterize AppKit-backed children, so everything
    /// inside a `ScrollView` (the whole ☰ menu, the usage tile grid) comes out
    /// blank (verified 2026-07-02). The hosting view draws the real hierarchy
    /// straight into a bitmap — no window is created.
    @MainActor
    private static func writeHosted(view: some View, size: CGSize, to url: URL) throws {
        let host = NSHostingView(rootView:
            view
                .frame(width: size.width, height: size.height)
                .background(Color.black)
                .environment(\.colorScheme, .dark)
        )
        host.frame = CGRect(origin: .zero, size: size)
        host.layoutSubtreeIfNeeded()

        guard let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil,
            pixelsWide: Int(size.width * scale),
            pixelsHigh: Int(size.height * scale),
            bitsPerSample: 8,
            samplesPerPixel: 4,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: .calibratedRGB,
            bytesPerRow: 0,
            bitsPerPixel: 0
        ) else {
            throw SnapshotError.renderFailed(url.lastPathComponent)
        }
        rep.size = size
        host.cacheDisplay(in: host.bounds, to: rep)
        try writePNG(rep, to: url)
    }

    /// Single PNG encoder for both families.
    private static func writePNG(_ rep: NSBitmapImageRep, to url: URL) throws {
        guard let data = rep.representation(using: .png, properties: [:]) else {
            throw SnapshotError.pngEncodeFailed(url.lastPathComponent)
        }
        try data.write(to: url)
    }

}

/// The closed island as snapshot mode renders it: `NotchClosedLabelContent`
/// composed with the same chrome the closed `NotchView` applies (min width,
/// row height, 14pt horizontal padding, black fill, NotchShape 6/14 clip).
/// This replicates NotchView's closed-state modifiers instead of instantiating
/// NotchView itself, which requires a live NotchViewModel/window — see the
/// fidelity notes in the PR.
struct ClosedIslandSnapshotView: View {
    /// How the animation instant is specified.
    enum Clock {
        /// Fixed 0..<1 position within the rainbow hue loop.
        case phase(Double)
        /// Absolute wall-clock seconds (`rainbowHue(time:)`).
        case wallClock(TimeInterval)
    }

    let claudeCount: Int
    let codexCount: Int
    let grokCount: Int
    let clock: Clock

    /// Non-notch fallback island size (Ext+NSScreen.notchSize fallback).
    private static let closedNotchSize = CGSize(width: 224, height: 38)

    private func hue(seed: Double) -> Double {
        switch clock {
        case .phase(let phase):
            return NotchClosedLabelView.rainbowHue(phase: phase, seed: seed)
        case .wallClock(let t):
            return NotchClosedLabelView.rainbowHue(time: t, seed: seed)
        }
    }

    var body: some View {
        NotchClosedLabelContent(
            claudeCount: claudeCount,
            codexCount: codexCount,
            grokCount: grokCount,
            // Deterministic frames render the mascot grounded.
            jumpOffset: 0,
            claudeHue: hue(seed: NotchClosedLabelView.claudeHueSeed),
            codexHue: hue(seed: NotchClosedLabelView.codexHueSeed),
            grokHue: hue(seed: NotchClosedLabelView.grokHueSeed)
        )
        .frame(minWidth: Self.closedNotchSize.width - 20)
        .frame(height: max(24, Self.closedNotchSize.height))
        .padding(.horizontal, 14)
        .background(.black)
        .clipShape(NotchShape(topCornerRadius: 6, bottomCornerRadius: 14))
        .environment(\.colorScheme, .dark)
    }
}

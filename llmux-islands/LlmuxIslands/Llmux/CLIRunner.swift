//
//  CLIRunner.swift
//  LlmuxIslands
//
//  A tiny, testable wrapper around the `llmux` CLI. Two responsibilities live
//  here, deliberately split so the parsing is unit-testable without ever
//  spawning a process:
//
//    1. `CLIRunner`  — spawns `llmux <args>` on a background queue (never the
//       main thread) and hands back exit code + captured stdout/stderr.
//    2. `CLIParse`   — pure Foundation string parsing of that output into
//       `ReleaseChannel` / `UpdateOutcome`. No Process, no I/O — fed canned
//       strings by the test suite.
//
//  Contract (the `llmux update` / `llmux channel` subcommands are added on the
//  Rust side): `llmux channel` prints ONE word — `stable` or `preview`;
//  `llmux channel <name>` installs that channel; both exit nonzero on failure.
//

import Foundation

// MARK: - ReleaseChannel

/// The llmux release channel. `rawValue` is exactly the single word the CLI
/// prints for `llmux channel` and accepts for `llmux channel <name>`.
enum ReleaseChannel: String, CaseIterable, Identifiable, Equatable {
    case stable
    case preview

    var id: String { rawValue }

    /// Title-case label for the segmented control.
    var label: String {
        switch self {
        case .stable: return "Stable"
        case .preview: return "Preview"
        }
    }
}

// MARK: - UpdateOutcome

/// The summarized result of `llmux update`, ready to render inline.
enum UpdateOutcome: Equatable {
    /// Update succeeded; `version` is the new version string when the CLI
    /// printed one (e.g. "0.2.4"), nil when it only reported generic success.
    case updated(version: String?)
    /// Update ran, nothing to do — already on the newest build.
    case alreadyUpToDate
    /// Update failed; `message` is the last non-empty output line.
    case failed(message: String)

    /// One-line inline summary shown under the button.
    var summary: String {
        switch self {
        case .updated(let version):
            return version.map { "Updated to \($0)" } ?? "Updated"
        case .alreadyUpToDate:
            return "Already up to date"
        case .failed(let message):
            return message.isEmpty ? "Update failed" : message
        }
    }

    /// Whether this outcome represents a failure (drives the inline color).
    var isFailure: Bool {
        if case .failed = self { return true }
        return false
    }
}

// MARK: - CLIParse (pure, testable)

/// Pure string parsing of `llmux` output. No process spawning, no I/O — every
/// function is total and deterministic so the tests can feed it canned strings.
enum CLIParse {
    /// Parse the single word printed by `llmux channel` into a `ReleaseChannel`.
    /// Defensive: trims surrounding whitespace/newlines, lowercases, and takes
    /// the first whitespace-delimited token, so trailing log noise or a newline
    /// never defeats the match. Returns nil for anything unrecognized.
    static func channel(from stdout: String) -> ReleaseChannel? {
        let token = stdout
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
            .split(whereSeparator: { $0.isWhitespace })
            .first
            .map(String.init) ?? ""
        return ReleaseChannel(rawValue: token)
    }

    /// Summarize a finished `llmux update` invocation.
    ///
    /// - nonzero exit → `.failed` with the error's last output line
    ///   (stderr preferred, falling back to stdout).
    /// - zero exit + an "already up to date" signal → `.alreadyUpToDate`.
    ///   Checked BEFORE version extraction because that line often also
    ///   contains the (unchanged) current version.
    /// - zero exit otherwise → `.updated`, carrying a parsed `X.Y.Z` version
    ///   when the output contains one.
    static func updateOutcome(exitCode: Int32, stdout: String, stderr: String) -> UpdateOutcome {
        guard exitCode == 0 else {
            return .failed(message: lastOutputLine(preferring: stderr, fallback: stdout))
        }
        let haystack = (stdout + "\n" + stderr).lowercased()
        if haystack.contains("up to date")
            || haystack.contains("up-to-date")
            || haystack.contains("already") {
            return .alreadyUpToDate
        }
        let version = extractVersion(from: stdout) ?? extractVersion(from: stderr)
        return .updated(version: version)
    }

    /// The error message to surface for a failed CLI run: the last non-empty
    /// line of `primary` (stderr), falling back to `fallback` (stdout), else "".
    static func lastOutputLine(preferring primary: String, fallback: String) -> String {
        lastNonEmptyLine(primary) ?? lastNonEmptyLine(fallback) ?? ""
    }

    /// The last non-blank, trimmed line of a blob, or nil if there is none.
    static func lastNonEmptyLine(_ text: String) -> String? {
        text
            .split(whereSeparator: { $0.isNewline })
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .last(where: { !$0.isEmpty })
    }

    /// Extract the first `X.Y.Z` (optionally `vX.Y.Z`) version from a blob,
    /// returned without the leading `v`. nil when none is present.
    static func extractVersion(from text: String) -> String? {
        guard let range = text.range(of: #"v?[0-9]+\.[0-9]+\.[0-9]+"#, options: .regularExpression) else {
            return nil
        }
        var match = String(text[range])
        if match.hasPrefix("v") { match.removeFirst() }
        return match
    }
}

// MARK: - CLIRunner (process spawning)

/// Spawns the `llmux` CLI off the main thread. Kept intentionally small and
/// injectable (`binaryPath`) so the parsing layer above can be tested without
/// it. All async methods hop to a background queue for the blocking Process
/// work and never touch the main thread.
struct CLIRunner {
    /// Preferred absolute path to the Homebrew-installed binary.
    static let defaultBinaryPath = "/opt/homebrew/bin/llmux"

    /// Absolute path to `llmux`. If it does not exist we fall back to resolving
    /// `llmux` on PATH via `/usr/bin/env`.
    var binaryPath: String = CLIRunner.defaultBinaryPath

    /// Raw result of one CLI invocation.
    struct Result {
        let exitCode: Int32
        let stdout: String
        let stderr: String
    }

    // MARK: High-level actions

    /// `llmux channel` → the current channel, or nil if the command failed or
    /// printed an unrecognized word.
    func currentChannel() async -> ReleaseChannel? {
        let result = await run(["channel"])
        guard result.exitCode == 0 else { return nil }
        return CLIParse.channel(from: result.stdout)
    }

    /// `llmux channel <name>` → switch channels (reinstalls llmux and this app).
    /// Reuses `UpdateOutcome` for a uniform in-progress/result surface.
    func setChannel(_ channel: ReleaseChannel) async -> UpdateOutcome {
        let result = await run(["channel", channel.rawValue])
        return CLIParse.updateOutcome(exitCode: result.exitCode, stdout: result.stdout, stderr: result.stderr)
    }

    /// `llmux update` → self-update, summarized for inline display.
    func update() async -> UpdateOutcome {
        let result = await run(["update"])
        return CLIParse.updateOutcome(exitCode: result.exitCode, stdout: result.stdout, stderr: result.stderr)
    }

    // MARK: Process plumbing

    /// Run `llmux <args>` on a background queue and return the captured result.
    /// The `await` suspends the caller without blocking its thread; the actual
    /// blocking Process work runs on a global queue.
    func run(_ arguments: [String]) async -> Result {
        let path = binaryPath
        return await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(returning: Self.runSync(binaryPath: path, arguments: arguments))
            }
        }
    }

    /// Synchronous, blocking process launch. MUST be called off the main thread
    /// (see `run(_:)`). Resolves the binary directly when present, else shells
    /// out through `/usr/bin/env llmux` so a PATH install still works.
    private static func runSync(binaryPath: String, arguments: [String]) -> Result {
        let process = Process()
        if FileManager.default.isExecutableFile(atPath: binaryPath) {
            process.executableURL = URL(fileURLWithPath: binaryPath)
            process.arguments = arguments
        } else {
            process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
            process.arguments = ["llmux"] + arguments
        }

        let stdoutPipe = Pipe()
        let stderrPipe = Pipe()
        process.standardOutput = stdoutPipe
        process.standardError = stderrPipe

        do {
            try process.run()
        } catch {
            return Result(exitCode: -1, stdout: "", stderr: error.localizedDescription)
        }

        // Read to EOF before waiting so a large stream can't dead-lock the pipe.
        let outData = stdoutPipe.fileHandleForReading.readDataToEndOfFile()
        let errData = stderrPipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()

        return Result(
            exitCode: process.terminationStatus,
            stdout: String(decoding: outData, as: UTF8.self),
            stderr: String(decoding: errData, as: UTF8.self)
        )
    }
}

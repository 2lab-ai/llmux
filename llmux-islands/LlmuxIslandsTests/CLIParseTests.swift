import XCTest

/// Logic tests for the `llmux` CLI output parsing (CLIParse). These feed canned
/// strings only — no process is ever spawned — matching the contract of the
/// `llmux channel` / `llmux update` subcommands: `channel` prints one word
/// (`stable`|`preview`) and both exit nonzero on failure.
final class CLIParseTests: XCTestCase {
    // MARK: - channel word parsing

    func testChannelParsesBareWord() {
        XCTAssertEqual(CLIParse.channel(from: "stable"), .stable)
        XCTAssertEqual(CLIParse.channel(from: "preview"), .preview)
    }

    func testChannelIsWhitespaceAndCaseTolerant() {
        // Trailing newline (the common CLI case), surrounding spaces, casing,
        // and trailing log noise must all still resolve.
        XCTAssertEqual(CLIParse.channel(from: "stable\n"), .stable)
        XCTAssertEqual(CLIParse.channel(from: "  preview  "), .preview)
        XCTAssertEqual(CLIParse.channel(from: "STABLE"), .stable)
        XCTAssertEqual(CLIParse.channel(from: "Preview\n"), .preview)
        XCTAssertEqual(CLIParse.channel(from: "preview (installed)"), .preview)
    }

    func testChannelUnrecognizedIsNil() {
        XCTAssertNil(CLIParse.channel(from: ""))
        XCTAssertNil(CLIParse.channel(from: "\n"))
        XCTAssertNil(CLIParse.channel(from: "beta"))
        XCTAssertNil(CLIParse.channel(from: "error: not logged in"))
    }

    // MARK: - update outcome summarization

    func testUpdateFailureUsesLastOutputLine() {
        // Nonzero exit → failure carrying the error's LAST non-empty line,
        // stderr preferred over stdout.
        let outcome = CLIParse.updateOutcome(
            exitCode: 1,
            stdout: "fetching...\n",
            stderr: "Error: network unreachable\nupdate aborted\n"
        )
        XCTAssertEqual(outcome, .failed(message: "update aborted"))
        XCTAssertTrue(outcome.isFailure)
        XCTAssertEqual(outcome.summary, "update aborted")
    }

    func testUpdateFailureFallsBackToStdoutThenGeneric() {
        // Empty stderr → fall back to stdout's last line.
        XCTAssertEqual(
            CLIParse.updateOutcome(exitCode: 2, stdout: "something broke\n", stderr: "   \n"),
            .failed(message: "something broke")
        )
        // No output at all → generic "Update failed" summary.
        let empty = CLIParse.updateOutcome(exitCode: 1, stdout: "", stderr: "")
        XCTAssertEqual(empty, .failed(message: ""))
        XCTAssertEqual(empty.summary, "Update failed")
    }

    func testUpdateAlreadyUpToDate() {
        // Various phrasings of "no change", incl. ones that ALSO print the
        // (unchanged) current version — the already-check must win.
        XCTAssertEqual(
            CLIParse.updateOutcome(exitCode: 0, stdout: "llmux is already up to date\n", stderr: ""),
            .alreadyUpToDate
        )
        XCTAssertEqual(
            CLIParse.updateOutcome(exitCode: 0, stdout: "Already up-to-date (0.2.3)\n", stderr: ""),
            .alreadyUpToDate
        )
        XCTAssertEqual(
            CLIParse.updateOutcome(exitCode: 0, stdout: "", stderr: "Nothing to do; already current\n"),
            .alreadyUpToDate
        )
        XCTAssertEqual(UpdateOutcome.alreadyUpToDate.summary, "Already up to date")
    }

    func testUpdateSucceededWithVersion() {
        let outcome = CLIParse.updateOutcome(
            exitCode: 0,
            stdout: "Updated llmux to 0.2.4\n",
            stderr: ""
        )
        XCTAssertEqual(outcome, .updated(version: "0.2.4"))
        XCTAssertEqual(outcome.summary, "Updated to 0.2.4")
        XCTAssertFalse(outcome.isFailure)
    }

    func testUpdateSucceededStripsLeadingV() {
        XCTAssertEqual(
            CLIParse.updateOutcome(exitCode: 0, stdout: "upgraded to v1.10.0\n", stderr: ""),
            .updated(version: "1.10.0")
        )
    }

    func testUpdateSucceededWithoutVersion() {
        // Zero exit, no version, no "already" signal → generic updated.
        let outcome = CLIParse.updateOutcome(exitCode: 0, stdout: "done\n", stderr: "")
        XCTAssertEqual(outcome, .updated(version: nil))
        XCTAssertEqual(outcome.summary, "Updated")
    }

    // MARK: - helper primitives

    func testVersionExtraction() {
        XCTAssertEqual(CLIParse.extractVersion(from: "llmux 0.2.14 (preview)"), "0.2.14")
        XCTAssertEqual(CLIParse.extractVersion(from: "v2026.07.09"), "2026.07.09")
        XCTAssertNil(CLIParse.extractVersion(from: "no version here"))
        XCTAssertNil(CLIParse.extractVersion(from: "only 1.2 here"))   // needs X.Y.Z
    }

    func testLastNonEmptyLine() {
        XCTAssertEqual(CLIParse.lastNonEmptyLine("a\nb\n\n  \n"), "b")
        XCTAssertNil(CLIParse.lastNonEmptyLine("\n  \n"))
        XCTAssertEqual(CLIParse.lastOutputLine(preferring: "", fallback: "x\ny\n"), "y")
        XCTAssertEqual(CLIParse.lastOutputLine(preferring: "", fallback: ""), "")
    }

    // MARK: - ReleaseChannel

    func testReleaseChannelRawValuesMatchCLIContract() {
        // The rawValue is passed verbatim as `llmux channel <name>` — it must
        // stay the exact single word the CLI prints/accepts.
        XCTAssertEqual(ReleaseChannel.stable.rawValue, "stable")
        XCTAssertEqual(ReleaseChannel.preview.rawValue, "preview")
        XCTAssertEqual(ReleaseChannel.allCases.map(\.label), ["Stable", "Preview"])
    }
}

import XCTest

/// The island maps the daemon's login `error` text to fixed sentences; the
/// raw text never reaches the UI.
final class LoginFailureMessageTests: XCTestCase {
    func testPortInUseNamesTheFix() {
        // The exact text the daemon returned on 2026-11-26 when an earlier,
        // cancelled login still held the Gemini callback port.
        let message = LoginFailureMessage.from(daemonError: "io error: Address already in use (os error 48)")
        XCTAssertTrue(message.contains("callback port is in use"), message)
        XCTAssertTrue(message.contains("llmux restart"), message)
    }

    func testKnownFailuresGetTheirOwnSentence() {
        XCTAssertEqual(
            LoginFailureMessage.from(daemonError: "login flow aborted: login timed out after 2 minutes"),
            "login timed out — finish the sign-in in the browser within 2 minutes"
        )
        XCTAssertEqual(LoginFailureMessage.from(daemonError: "cancelled"), "login cancelled")
        XCTAssertTrue(
            LoginFailureMessage.from(daemonError: "oauth state mismatch (possible CSRF, aborting)")
                .contains("old sign-in page")
        )
        XCTAssertTrue(
            LoginFailureMessage.from(daemonError: "login flow aborted: oauth error: access_denied")
                .contains("declined")
        )
        XCTAssertTrue(
            LoginFailureMessage.from(
                daemonError: "gemini auth data invalid: login token response missing refresh_token (offline access not granted?)"
            ).contains("offline access was not granted")
        )
    }

    func testUnknownOrMissingErrorStaysGenericAndNeverEchoesTheRawText() {
        let raw = "profile endpoint returned 500: {\"secret\":\"do-not-show\"}"
        XCTAssertEqual(LoginFailureMessage.from(daemonError: raw), LoginFailureMessage.generic)
        XCTAssertEqual(LoginFailureMessage.from(daemonError: nil), LoginFailureMessage.generic)
        XCTAssertEqual(LoginFailureMessage.from(daemonError: ""), LoginFailureMessage.generic)
    }
}

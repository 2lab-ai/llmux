import XCTest

final class DaemonLauncherTests: XCTestCase {
    func testConfiguredLoopbackPortIsUsedForProbeAndLaunch() {
        let endpoint = DaemonLauncher.localEndpoint(
            for: LlmuxClient(host: "127.0.0.1", port: 4567)
        )

        XCTAssertEqual(
            endpoint,
            DaemonLauncher.LocalEndpoint(baseURL: "http://127.0.0.1:4567", port: 4567)
        )
        XCTAssertEqual(
            DaemonLauncher.launchArguments(port: 4567),
            ["server", "--port", "4567", "--no-tui"]
        )
    }

    func testBracketedIPv6LoopbackPreservesConfiguredPort() {
        XCTAssertEqual(
            DaemonLauncher.localEndpoint(for: LlmuxClient(host: "[::1]", port: 4568)),
            DaemonLauncher.LocalEndpoint(baseURL: "http://[::1]:4568", port: 4568)
        )
    }

    func testRemoteOrExplicitHTTPSEndpointNeverStartsPlainLocalDaemon() {
        XCTAssertNil(
            DaemonLauncher.localEndpoint(
                for: LlmuxClient(host: "https://example.com", port: 443, apiKey: "configured")
            )
        )
        XCTAssertNil(
            DaemonLauncher.localEndpoint(
                for: LlmuxClient(host: "https://127.0.0.1", port: 4567)
            )
        )
    }
}

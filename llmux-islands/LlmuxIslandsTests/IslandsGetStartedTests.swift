import XCTest

final class IslandsGetStartedTests: XCTestCase {
    private let endpoint = "http://127.0.0.1:3456"
    private func payload(_ key: String = "synthetic-control", port: Int = 3456) -> Data {
        try! JSONSerialization.data(withJSONObject: ["endpoint": "http://127.0.0.1:\(port)", "api_key": key])
    }
    func testHandoffNeverAuthorizesAnotherEndpoint() throws {
        let handoff = try JSONDecoder().decode(LocalControlHandoff.self, from: payload())
        for url in ["https://remote.example:3456/llmux/status", "http://127.0.0.1:3457/llmux/status"] {
            XCTAssertThrowsError(try handoff.authorize(URLRequest(url: URL(string: url)!), expectedEndpoint: endpoint))
        }
        let request = try handoff.authorize(URLRequest(url: URL(string: endpoint + "/llmux/status")!), expectedEndpoint: endpoint)
        XCTAssertEqual(request.value(forHTTPHeaderField: "x-api-key"), "synthetic-control")
    }
    func testFreshCredentialAndReconfigureRace() async throws {
        let auth = LocalControlAuth()
        let endpoint = endpoint
        let request = URLRequest(url: URL(string: endpoint + "/llmux/status")!)
        for key in ["synthetic-old", "synthetic-new"] {
            let data = payload(key)
            let authorized = try await auth.authorize(request, endpoint: endpoint, selectedEndpoint: { endpoint }, load: { _ in data })
            XCTAssertEqual(authorized.value(forHTTPHeaderField: "x-api-key"), key)
        }
        let data = payload()
        do {
            _ = try await auth.authorize(request, endpoint: endpoint, selectedEndpoint: { endpoint }, load: { _ in
                await auth.invalidate()
                return data
            })
            XCTFail("stale handoff must be rejected")
        } catch { XCTAssertTrue(error is LocalConnectionError) }
    }
    func testHandoffRejectsMalformedNewlineAndPort() throws {
        for data in [payload("bad\nheader"), payload(port: 3457)] {
            let handoff = try JSONDecoder().decode(LocalControlHandoff.self, from: data)
            XCTAssertThrowsError(try handoff.authorize(URLRequest(url: URL(string: endpoint)!), expectedEndpoint: endpoint))
        }
    }
    func testUnauthorizedDaemonIsRunningAndMustNotBeSpawnedAgain() {
        for code in [200, 204, 401, 403] { XCTAssertTrue(DaemonLauncher.isRunningStatus(code)) }
        XCTAssertFalse(DaemonLauncher.isRunningStatus(502))
    }
    func testBoundedHelperFailureIsSanitizedAndTerminates() {
        let start = Date()
        XCTAssertThrowsError(try BoundedLocalProcess.output(executable: "/bin/sleep", arguments: ["5"], timeout: 0.05)) {
            XCTAssertEqual(($0 as? LocalConnectionError), .timedOut)
        }
        XCTAssertLessThan(Date().timeIntervalSince(start), 2)
        XCTAssertThrowsError(try BoundedLocalProcess.output(executable: "/usr/bin/false", arguments: []))
    }
    func testRemoteCredentialCannotFollowAChangedEndpointOrLocalSwitch() throws {
        XCTAssertThrowsError(try ConnectionApiKeyIntent.keep.resolveForPersistence(
            existingKey: "synthetic-remote", existingEndpoint: "https://a.example:3456",
            existingKeyWasExplicitlyCleared: false, candidateEndpoint: "https://b.example:3456", candidateIsRemote: true))
        let local = try ConnectionApiKeyIntent.keep.resolveForPersistence(
            existingKey: "synthetic-remote", existingEndpoint: "https://a.example:3456",
            existingKeyWasExplicitlyCleared: false, candidateEndpoint: endpoint, candidateIsRemote: false)
        XCTAssertEqual(local.key, "")
    }
    func testLaunchCommandRunsLiteralHostileFolderAndCorrectArguments() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let project = root.appendingPathComponent("project ' \" $(touch INJECTED)\nnext")
        try FileManager.default.createDirectory(at: project, withIntermediateDirectories: true)
        let tool = root.appendingPathComponent("fake llmux'")
        try Data("#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\"\n".utf8).write(to: tool)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: tool.path)
        for client in CodingClient.allCases {
            let plan = try CodingLaunchPlan(client: client, project: project, isRemote: false, findTool: { _ in tool.path })
            let process = Process(); let pipe = Pipe()
            process.executableURL = URL(fileURLWithPath: "/bin/sh")
            process.arguments = ["-c", plan.command]
            process.standardOutput = pipe
            try process.run(); process.waitUntilExit()
            XCTAssertEqual(process.terminationStatus, 0)
            let output = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            XCTAssertEqual(output, project.path + "\nrun\n" + (client == .codex ? "--codex\n" : ""))
            XCTAssertFalse(FileManager.default.fileExists(atPath: project.appendingPathComponent("INJECTED").path))
        }
    }
    func testLaunchRejectsMissingFolderToolsAndRemoteWithoutDispatch() {
        XCTAssertThrowsError(try CodingLaunchPlan(client: .codex, project: nil, isRemote: true))
        XCTAssertThrowsError(try CodingLaunchPlan(client: .claude, project: nil, isRemote: false, findTool: { _ in nil }))
        XCTAssertThrowsError(try CodingLaunchPlan(client: .claude, project: nil, isRemote: false, findTool: { _ in "/fake" }))
        XCTAssertTrue(CodingLaunchPlan.terminalScript.contains("item 1 of argv"))
    }
    func testHelperDeadlineIncludesDescendantHeldOutputAndLimitsAllocation() {
        let start = Date()
        XCTAssertThrowsError(try BoundedLocalProcess.output(executable: "/bin/sh", arguments: ["-c", "sleep 1 & exit 0"], timeout: 0.05)) {
            XCTAssertEqual($0 as? LocalConnectionError, .timedOut)
        }
        XCTAssertLessThan(Date().timeIntervalSince(start), 0.8)
        XCTAssertThrowsError(try BoundedLocalProcess.output(executable: "/usr/bin/head", arguments: ["-c", "1000000", "/dev/zero"])) {
            XCTAssertEqual($0 as? LocalConnectionError, .invalid)
        }
    }
    func testRecoveryMatchesAppChannelAndUpdatesExistingInstall() {
        XCTAssertEqual(IslandsRecovery.command(appVersion: "2026.10.08.1234", llmuxInstalled: true), "llmux channel preview && llmux update")
        XCTAssertEqual(IslandsRecovery.command(appVersion: "0.2.25", llmuxInstalled: true), "llmux channel stable && llmux update")
        XCTAssertTrue(IslandsRecovery.command(appVersion: "2026.10.08.1234", llmuxInstalled: false).contains("llmux-islands-preview"))
    }
}

import XCTest

final class IslandPresentationPolicyTests: XCTestCase {
    func testCommonPathItemsStayVisibleWhenAdvancedIsCollapsed() {
        let commonPath: [IslandPresentationItem] = [
            .navigation, .connectionStatus, .attentionReason, .operationFailure, .primaryQuota,
            .summaryMetrics, .accountOverview, .addAccount, .refresh,
            .screen, .sound, .privacy, .launchAtLogin,
        ]

        XCTAssertTrue(commonPath.allSatisfy {
            IslandPresentationPolicy.isVisible($0, advancedPresented: false)
        })
    }

    func testTechnicalDetailRequiresExplicitAdvancedDisclosure() {
        let detail: [IslandPresentationItem] = [
            .credentialMetadata, .accountControls, .analyticsDetail,
            .requestReceipts, .endpointCredentials, .platformDiagnostics,
            .events, .maintenance, .buildMetadata,
        ]

        XCTAssertTrue(detail.allSatisfy {
            !IslandPresentationPolicy.isVisible($0, advancedPresented: false)
        })
        XCTAssertTrue(detail.allSatisfy {
            IslandPresentationPolicy.isVisible($0, advancedPresented: true)
        })
    }

    func testAdvancedLabelIsExplicitAndStable() {
        XCTAssertEqual(IslandPresentationPolicy.advancedLabel, "Advanced")
    }

    func testPrivateAccessibilityAliasesAreSafeAndDistinct() {
        let first = IslandPresentationPolicy.privateAccountLabel(providerName: "Claude", ordinal: 1)
        let second = IslandPresentationPolicy.privateAccountLabel(providerName: "Claude", ordinal: 2)

        XCTAssertEqual(first, "Claude account 1")
        XCTAssertNotEqual(first, second)
        XCTAssertFalse(first.contains("@"))
    }

    func testSnapshotEvidenceIsExactlySevenDistinctProductionStates() {
        let files = IslandPresentationPolicy.snapshotSurfaceFiles(emailAnonymous: true)

        XCTAssertEqual(files.count, 7)
        XCTAssertEqual(Set(files).count, 7)
        XCTAssertTrue(files.contains("usage-advanced.png"))
        XCTAssertTrue(files.contains("stats-advanced.png"))
        XCTAssertTrue(files.contains("menu-advanced.png"))
        XCTAssertTrue(files.contains("receipts-detail.png"))
    }
}

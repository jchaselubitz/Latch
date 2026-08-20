import Foundation
import XCTest

@testable import LatchMobileKit

/// App-target declarations that platform privacy enforcement treats as part
/// of the paired transport contract.
final class AppConfigurationTests: XCTestCase {
    private var infoPlist: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // LatchMobileKitTests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // LatchMobile
            .appendingPathComponent("App/LatchMobile/Info.plist")
    }

    func testPairedBonjourServiceIsDeclaredForLocalNetworkPrivacy() throws {
        let data = try Data(contentsOf: infoPlist)
        let properties = try XCTUnwrap(
            try PropertyListSerialization.propertyList(from: data, format: nil)
                as? [String: Any]
        )
        let services = try XCTUnwrap(properties["NSBonjourServices"] as? [String])

        XCTAssertTrue(
            services.contains(BonjourMacDiscovery.serviceType),
            "iOS blocks Bonjour browsing when the service type is absent from Info.plist"
        )
        XCTAssertFalse(
            (properties["NSLocalNetworkUsageDescription"] as? String)?
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .isEmpty ?? true,
            "local-network access requires a user-facing privacy explanation"
        )
    }
}

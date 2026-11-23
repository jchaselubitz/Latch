import XCTest
@testable import LatchMobileKit

final class ConversationSessionFactsTests: XCTestCase {
    private func state(_ json: String) throws -> ConversationState {
        let base = #"{"phase":"idle","sendMessage":{"enabled":true},"resolveRequest":{"enabled":false},"pendingRequest":null,"connector":null"#
        return try JSONDecoder().decode(ConversationState.self, from: Data((base + json + "}").utf8))
    }

    func testNoFactsIsNoLine() throws {
        XCTAssertNil(ConversationSessionFacts.line(try state("")))
        XCTAssertNil(ConversationSessionFacts.line(nil))
    }

    func testModelEffortModeContextLimitAndCostShareOneLine() throws {
        let line = ConversationSessionFacts.line(try state(#"""
        ,"model":"claude-haiku-5-5","effort":"medium","permissionMode":"plan",
        "usage":{"contextTokens":50204,"contextWindow":200000,"contextPercent":25,
        "rateLimits":[{"kind":"five_hour","percentUsed":12},{"kind":"seven_day","percentUsed":81}],"costCents":42}
        """#))
        XCTAssertEqual(line, "Haiku 5.5 · medium effort · Plan mode · 25% context · 7-day limit 81% · $0.42")
    }

    func testTheDefaultModeAndAQuietLimitSayNothing() throws {
        let line = ConversationSessionFacts.line(try state(#"""
        ,"model":"opus[1m]","permissionMode":"default",
        "usage":{"contextTokens":1000,"contextWindow":4000,"rateLimits":[{"kind":"five_hour","percentUsed":10}]}
        """#))
        XCTAssertEqual(line, "opus[1m] · 25% context")
    }
}

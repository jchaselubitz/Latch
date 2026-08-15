import XCTest

@testable import LatchMobileKit

/// The phrase is the user's only check on a substituted identity, and it is
/// worth nothing unless the Mac computes the same words. The vector below is
/// asserted identically by `pairing_phrase_is_transcript_bound_and_stable` in
/// `crates/latch/src/cli/remote_access.rs`.
final class PairingPhraseTests: XCTestCase {
    private let pairingId = "0123456789abcdef0123456789abcdef"
    private let macKey = String(repeating: "1", count: 64)
    private let deviceKey = String(repeating: "2", count: 64)

    func testMatchesTheDesktopVector() {
        XCTAssertEqual(
            PairingPhrase.derive(
                pairingId: pairingId,
                macPublicKey: macKey,
                devicePublicKey: deviceKey
            ),
            "sable-apple-maple-garnet-maple-flint"
        )
    }

    func testProducesSixWordsFromTheList() {
        let words = PairingPhrase.derive(
            pairingId: pairingId,
            macPublicKey: macKey,
            devicePublicKey: deviceKey
        ).split(separator: "-").map(String.init)
        XCTAssertEqual(words.count, PairingPhrase.wordCount)
        for word in words {
            XCTAssertTrue(PairingPhrase.words.contains(word), word)
        }
    }

    /// The list's size and order are part of the contract: an insertion would
    /// silently change every phrase both sides derive.
    func testWordListIsSixtyFourDistinctWords() {
        XCTAssertEqual(PairingPhrase.words.count, 1 << PairingPhrase.bitsPerWord)
        XCTAssertEqual(Set(PairingPhrase.words).count, PairingPhrase.words.count)
        XCTAssertEqual(PairingPhrase.words.first, "anchor")
        XCTAssertEqual(PairingPhrase.words.last, "willow")
    }

    /// A relay or control plane that swaps either key has to produce different
    /// words, or the comparison the user makes proves nothing.
    func testEveryTranscriptFieldIsBound() {
        let base = PairingPhrase.derive(
            pairingId: pairingId,
            macPublicKey: macKey,
            devicePublicKey: deviceKey
        )
        XCTAssertNotEqual(
            base,
            PairingPhrase.derive(
                pairingId: "0123456789abcdef0123456789abcde0",
                macPublicKey: macKey,
                devicePublicKey: deviceKey
            )
        )
        XCTAssertNotEqual(
            base,
            PairingPhrase.derive(
                pairingId: pairingId,
                macPublicKey: String(repeating: "1", count: 63) + "2",
                devicePublicKey: deviceKey
            )
        )
        XCTAssertNotEqual(
            base,
            PairingPhrase.derive(
                pairingId: pairingId,
                macPublicKey: macKey,
                devicePublicKey: String(repeating: "2", count: 63) + "3"
            )
        )
    }

    /// Hex case is not meaningful. A phrase that changed with it would show up
    /// as an unexplained mismatch rather than as the encoding difference it is.
    func testHexCaseDoesNotChangeThePhrase() {
        XCTAssertEqual(
            PairingPhrase.derive(
                pairingId: pairingId.uppercased(),
                macPublicKey: macKey,
                devicePublicKey: deviceKey.uppercased()
            ),
            PairingPhrase.derive(
                pairingId: pairingId,
                macPublicKey: macKey,
                devicePublicKey: deviceKey
            )
        )
    }

    func testComparisonIgnoresHowAPersonTypesIt() {
        XCTAssertTrue(PairingPhrase.matches("sable-apple-maple", "Sable Apple  Maple"))
        XCTAssertTrue(PairingPhrase.matches("sable-apple-maple", "sable, apple, maple."))
        XCTAssertFalse(PairingPhrase.matches("sable-apple-maple", "sable-apple-marble"))
        XCTAssertFalse(PairingPhrase.matches("sable-apple-maple", "apple-sable-maple"))
    }

    /// The payload overload has to bind the same transcript as the explicit one.
    func testPayloadOverloadDerivesTheSamePhrase() throws {
        let json = """
        {
          "formatVersion": 1,
          "pairingId": "\(pairingId)",
          "secret": "\(String(repeating: "a", count: 64))",
          "macPublicKey": "\(macKey)",
          "expiresAt": \(Date().addingTimeInterval(120).timeIntervalSince1970)
        }
        """
        let payload = try PairingPayload.parse(json)
        XCTAssertEqual(
            PairingPhrase.derive(payload: payload, devicePublicKey: deviceKey),
            "sable-apple-maple-garnet-maple-flint"
        )
    }
}

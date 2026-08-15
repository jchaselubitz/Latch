import XCTest

@testable import LatchMobileKit

/// The compliance mechanism itself.
///
/// `Tools/generate-contract.py` derives the Swift contract from the canonical
/// schemas and records their digests. These tests fail when the committed Swift
/// no longer matches the schemas it claims to come from, which is what makes a
/// new contract version visible instead of silently unimplemented.
final class ContractFreshnessTests: XCTestCase {
    /// The app folder, found from this source file so the check works wherever
    /// the folder is checked out — including after a move to its own repository.
    private var app: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()  // LatchMobileKitTests
            .deletingLastPathComponent()  // Tests
            .deletingLastPathComponent()  // LatchMobile
    }

    private func manifest() throws -> [String: Any] {
        let data = try Data(contentsOf: app.appendingPathComponent("Contract/manifest.json"))
        return try XCTUnwrap(
            try JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
    }

    func testTheManifestRecordsTheProtocolThisBuildImplements() throws {
        let manifest = try manifest()
        XCTAssertEqual(
            manifest["protocolVersion"] as? Int,
            LatchContract.protocolVersion,
            "the generated constant and the vendored schema disagree; regenerate"
        )
    }

    func testTheGeneratedSwiftMatchesItsRecordedDigest() throws {
        // A hand edit to the generated file is the failure this catches: the
        // file says "do not edit by hand", and this is what enforces it.
        let manifest = try manifest()
        let generated = try XCTUnwrap(manifest["generated"] as? [String: Any])
        let path = try XCTUnwrap(generated["path"] as? String)
        let expected = try XCTUnwrap(generated["sha256"] as? String)
        let source = try Data(contentsOf: app.appendingPathComponent(path))
        XCTAssertEqual(
            Self.sha256(source),
            expected,
            "\(path) was edited by hand or left stale; run Tools/generate-contract.py"
        )
    }

    func testTheVendoredSchemasMatchTheirRecordedDigests() throws {
        let manifest = try manifest()
        let schemas = try XCTUnwrap(manifest["schemas"] as? [[String: Any]])
        XCTAssertFalse(schemas.isEmpty)
        for schema in schemas {
            let source = try XCTUnwrap(schema["source"] as? String)
            let expected = try XCTUnwrap(schema["sha256"] as? String)
            let name = URL(fileURLWithPath: source).lastPathComponent
            let vendored = app.appendingPathComponent("Contract/schemas/\(name)")
            let data = try Data(contentsOf: vendored)
            XCTAssertEqual(
                Self.sha256(data),
                expected,
                "Contract/schemas/\(name) does not match the digest recorded for \(source)"
            )
        }
    }

    func testTheGeneratedEventListCoversTheVendoredSchema() throws {
        let data = try Data(
            contentsOf: app.appendingPathComponent("Contract/schemas/harness-event.v1.json")
        )
        let schema = try XCTUnwrap(
            try JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        let properties = try XCTUnwrap(schema["properties"] as? [String: Any])
        let type = try XCTUnwrap(properties["type"] as? [String: Any])
        let declared = try XCTUnwrap(type["enum"] as? [String])
        XCTAssertEqual(
            LatchContract.harnessEventTypes,
            declared,
            "a harness event type was added upstream; run Tools/generate-contract.py"
        )
    }

    /// A local SHA-256, so the check does not need CryptoKit on every platform
    /// the package builds for.
    static func sha256(_ data: Data) -> String {
        var state: [UInt32] = [
            0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a,
            0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19
        ]
        let k: [UInt32] = [
            0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5, 0x3956_c25b, 0x59f1_11f1,
            0x923f_82a4, 0xab1c_5ed5, 0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3,
            0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174, 0xe49b_69c1, 0xefbe_4786,
            0x0fc1_9dc6, 0x240c_a1cc, 0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
            0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7, 0xc6e0_0bf3, 0xd5a7_9147,
            0x06ca_6351, 0x1429_2967, 0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13,
            0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85, 0xa2bf_e8a1, 0xa81a_664b,
            0xc24b_8b70, 0xc76c_51a3, 0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
            0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5, 0x391c_0cb3, 0x4ed8_aa4a,
            0x5b9c_ca4f, 0x682e_6ff3, 0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208,
            0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2
        ]
        var message = [UInt8](data)
        let length = UInt64(message.count) * 8
        message.append(0x80)
        while message.count % 64 != 56 { message.append(0) }
        for shift in stride(from: 56, through: 0, by: -8) {
            message.append(UInt8((length >> UInt64(shift)) & 0xff))
        }
        for chunk in stride(from: 0, to: message.count, by: 64) {
            var w = [UInt32](repeating: 0, count: 64)
            for index in 0..<16 {
                let base = chunk + index * 4
                w[index] = UInt32(message[base]) << 24
                    | UInt32(message[base + 1]) << 16
                    | UInt32(message[base + 2]) << 8
                    | UInt32(message[base + 3])
            }
            for index in 16..<64 {
                let s0 = rotate(w[index - 15], 7) ^ rotate(w[index - 15], 18) ^ (w[index - 15] >> 3)
                let s1 = rotate(w[index - 2], 17) ^ rotate(w[index - 2], 19) ^ (w[index - 2] >> 10)
                w[index] = w[index - 16] &+ s0 &+ w[index - 7] &+ s1
            }
            var (a, b, c, d, e, f, g, h) = (
                state[0], state[1], state[2], state[3],
                state[4], state[5], state[6], state[7]
            )
            for index in 0..<64 {
                let s1 = rotate(e, 6) ^ rotate(e, 11) ^ rotate(e, 25)
                let ch = (e & f) ^ (~e & g)
                let temp1 = h &+ s1 &+ ch &+ k[index] &+ w[index]
                let s0 = rotate(a, 2) ^ rotate(a, 13) ^ rotate(a, 22)
                let maj = (a & b) ^ (a & c) ^ (b & c)
                let temp2 = s0 &+ maj
                h = g; g = f; f = e; e = d &+ temp1
                d = c; c = b; b = a; a = temp1 &+ temp2
            }
            state[0] = state[0] &+ a
            state[1] = state[1] &+ b
            state[2] = state[2] &+ c
            state[3] = state[3] &+ d
            state[4] = state[4] &+ e
            state[5] = state[5] &+ f
            state[6] = state[6] &+ g
            state[7] = state[7] &+ h
        }
        return state.map { String(format: "%08x", $0) }.joined()
    }

    private static func rotate(_ value: UInt32, _ count: UInt32) -> UInt32 {
        (value >> count) | (value << (32 - count))
    }
}

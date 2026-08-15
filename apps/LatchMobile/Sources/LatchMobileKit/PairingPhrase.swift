import CryptoKit
import Foundation

/// The short authentication phrase both screens show during pairing.
///
/// The phrase is the user's part of the security argument. Everything else in
/// pairing is machine-checked, but nothing the phone can check by itself rules
/// out a relay or control plane that swapped the Mac's key for its own: both
/// sides would still agree, just with the attacker in the middle. The phrase is
/// derived from the pairing transcript — the pairing identifier and *both*
/// pinned public keys — so a substituted key produces a different phrase, and
/// the person comparing the two screens is the one who notices.
///
/// The derivation is a contract, not an implementation detail: the Mac must
/// compute the identical phrase from the identical transcript, so the domain
/// separator, the field order, the separators, and the word list below are all
/// fixed. `crates/latch/src/cli/remote_access.rs` holds the matching Rust.
public enum PairingPhrase {
    /// Bound into the digest so this hash can never be confused with the
    /// pairing-secret digest, which is over a nearly identical transcript.
    static let domain = "latch-remote-pairing-phrase-v1"
    /// Six words of six bits: 36 bits of transcript, which is far past what an
    /// online, one-shot, five-minute pairing attempt can search.
    static let wordCount = 6
    static let bitsPerWord = 6

    /// 64 short, visually distinct words. Order is part of the contract: an
    /// insertion would silently change every phrase.
    static let words: [String] = [
        "anchor", "apple", "arrow", "atlas", "badge", "baker",
        "beacon", "birch", "cable", "cactus", "candle", "cedar",
        "chalk", "cliff", "clover", "comet", "coral", "crane",
        "delta", "dune", "ember", "falcon", "fern", "flint",
        "forge", "garnet", "glacier", "granite", "harbor", "hazel",
        "ivory", "jade", "kettle", "lantern", "ledger", "lily",
        "lumen", "maple", "marble", "meadow", "mesa", "nickel",
        "nomad", "oak", "onyx", "orbit", "otter", "pebble",
        "pilot", "prism", "quartz", "quill", "raven", "ribbon",
        "saffron", "sable", "spruce", "summit", "thistle", "timber",
        "tundra", "velvet", "walnut", "willow"
    ]

    /// Derives the phrase for one pairing.
    ///
    /// Keys are compared case-insensitively by lowercasing here, because hex
    /// case is not meaningful and a mismatch would show up as an unexplained
    /// phrase disagreement rather than as the encoding difference it is.
    public static func derive(
        pairingId: String,
        macPublicKey: String,
        devicePublicKey: String
    ) -> String {
        var transcript = Data(domain.utf8)
        for field in [pairingId, macPublicKey, devicePublicKey] {
            transcript.append(0)
            transcript.append(Data(field.lowercased().utf8))
        }
        return phrase(from: Data(SHA256.hash(data: transcript)))
    }

    /// The phrase for a scanned payload and this phone's identity.
    public static func derive(payload: PairingPayload, devicePublicKey: String) -> String {
        derive(
            pairingId: payload.pairingId,
            macPublicKey: payload.macPublicKey,
            devicePublicKey: devicePublicKey
        )
    }

    /// Whether two phrases are the same, ignoring the punctuation and case a
    /// person might type differently when checking one by hand.
    public static func matches(_ left: String, _ right: String) -> Bool {
        normalize(left) == normalize(right)
    }

    static func normalize(_ phrase: String) -> [String] {
        phrase
            .lowercased()
            .split(whereSeparator: { !$0.isLetter })
            .map(String.init)
    }

    /// Big-endian bit extraction from the front of the digest, which is the
    /// convention the Rust side implements.
    static func phrase(from digest: Data) -> String {
        var chosen: [String] = []
        var accumulator = 0
        var bits = 0
        for byte in digest {
            accumulator = accumulator << 8 | Int(byte)
            bits += 8
            while bits >= bitsPerWord, chosen.count < wordCount {
                bits -= bitsPerWord
                let index = (accumulator >> bits) & ((1 << bitsPerWord) - 1)
                chosen.append(words[index])
            }
            if chosen.count == wordCount { break }
        }
        return chosen.joined(separator: "-")
    }
}

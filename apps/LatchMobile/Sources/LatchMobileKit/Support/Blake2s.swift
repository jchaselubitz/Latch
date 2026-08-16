import Foundation

/// BLAKE2s-256, hand-written because the platform does not ship it.
///
/// The Mac's transport is `Noise_XX_25519_ChaChaPoly_BLAKE2s`
/// (`crates/latch/src/cli/remote_access.rs`, `NOISE_PATTERN`), so the phone
/// needs BLAKE2s to interoperate at all. Neither CryptoKit nor swift-crypto
/// provides it, and LatchMobileKit deliberately has no dependencies, so this
/// is RFC 7693 transcribed directly. It is unkeyed and always 32 bytes out,
/// which is the only configuration Noise asks for; the keyed mode Noise needs
/// is HMAC over this hash, in `NoiseHash`.
struct Blake2s {
    /// HASHLEN in the Noise spec.
    static let digestLength = 32
    /// BLOCKLEN in the Noise spec, and the HMAC block size.
    static let blockLength = 64

    private static let initializationVector: [UInt32] = [
        0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a,
        0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19
    ]

    private static let sigma: [[UInt8]] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
        [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
        [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
        [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
        [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
        [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
        [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
        [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
        [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0]
    ]

    private var state: [UInt32]
    private var block = [UInt8](repeating: 0, count: Blake2s.blockLength)
    private var blockCount = 0
    private var bytesCompressed: UInt64 = 0

    init() {
        state = Self.initializationVector
        // Parameter block: digest length 32, key length 0, fanout 1, depth 1.
        state[0] ^= 0x0101_0020
    }

    /// Absorbs input. A full block is only compressed once more input arrives,
    /// because BLAKE2 marks the final block and cannot know it is final early.
    mutating func update<Bytes: Sequence>(_ bytes: Bytes) where Bytes.Element == UInt8 {
        for byte in bytes {
            if blockCount == Self.blockLength {
                bytesCompressed &+= UInt64(Self.blockLength)
                compress(last: false)
                blockCount = 0
            }
            block[blockCount] = byte
            blockCount += 1
        }
    }

    mutating func finalize() -> Data {
        bytesCompressed &+= UInt64(blockCount)
        for index in blockCount..<Self.blockLength { block[index] = 0 }
        compress(last: true)
        var digest = Data(capacity: Self.digestLength)
        for word in state {
            digest.append(UInt8(truncatingIfNeeded: word))
            digest.append(UInt8(truncatingIfNeeded: word >> 8))
            digest.append(UInt8(truncatingIfNeeded: word >> 16))
            digest.append(UInt8(truncatingIfNeeded: word >> 24))
        }
        return digest
    }

    static func hash(_ data: Data) -> Data {
        var hasher = Blake2s()
        hasher.update(data)
        return hasher.finalize()
    }

    private mutating func compress(last: Bool) {
        var message = [UInt32](repeating: 0, count: 16)
        for index in 0..<16 {
            let offset = index * 4
            message[index] =
                UInt32(block[offset])
                | UInt32(block[offset + 1]) << 8
                | UInt32(block[offset + 2]) << 16
                | UInt32(block[offset + 3]) << 24
        }

        var working = [UInt32](repeating: 0, count: 16)
        for index in 0..<8 { working[index] = state[index] }
        for index in 0..<8 { working[8 + index] = Self.initializationVector[index] }
        working[12] ^= UInt32(truncatingIfNeeded: bytesCompressed)
        working[13] ^= UInt32(truncatingIfNeeded: bytesCompressed >> 32)
        if last { working[14] = ~working[14] }

        for round in 0..<10 {
            let schedule = Self.sigma[round]
            mix(&working, 0, 4, 8, 12, message[Int(schedule[0])], message[Int(schedule[1])])
            mix(&working, 1, 5, 9, 13, message[Int(schedule[2])], message[Int(schedule[3])])
            mix(&working, 2, 6, 10, 14, message[Int(schedule[4])], message[Int(schedule[5])])
            mix(&working, 3, 7, 11, 15, message[Int(schedule[6])], message[Int(schedule[7])])
            mix(&working, 0, 5, 10, 15, message[Int(schedule[8])], message[Int(schedule[9])])
            mix(&working, 1, 6, 11, 12, message[Int(schedule[10])], message[Int(schedule[11])])
            mix(&working, 2, 7, 8, 13, message[Int(schedule[12])], message[Int(schedule[13])])
            mix(&working, 3, 4, 9, 14, message[Int(schedule[14])], message[Int(schedule[15])])
        }

        for index in 0..<8 {
            state[index] ^= working[index] ^ working[index + 8]
        }
    }

    private func mix(
        _ v: inout [UInt32],
        _ a: Int, _ b: Int, _ c: Int, _ d: Int,
        _ x: UInt32, _ y: UInt32
    ) {
        v[a] = v[a] &+ v[b] &+ x
        v[d] = rotateRight(v[d] ^ v[a], 16)
        v[c] = v[c] &+ v[d]
        v[b] = rotateRight(v[b] ^ v[c], 12)
        v[a] = v[a] &+ v[b] &+ y
        v[d] = rotateRight(v[d] ^ v[a], 8)
        v[c] = v[c] &+ v[d]
        v[b] = rotateRight(v[b] ^ v[c], 7)
    }

    private func rotateRight(_ value: UInt32, _ bits: UInt32) -> UInt32 {
        (value >> bits) | (value << (32 - bits))
    }
}

/// The hash-derived primitives Noise names: HASH, HMAC-HASH, and HKDF.
///
/// Noise's HKDF is not RFC 5869's two-stage form with an info string — it is
/// the three-output variant in section 5.3 of the Noise spec, and the counter
/// bytes are the only "info" there is. Writing it out here keeps that visible
/// rather than hiding it behind a general-purpose KDF that would need the
/// wrong arguments.
enum NoiseHash {
    static func hash(_ data: Data) -> Data {
        Blake2s.hash(data)
    }

    /// HMAC over BLAKE2s. Noise uses HMAC rather than BLAKE2's native keyed
    /// mode, so this is plain RFC 2104 with a 64-byte block.
    static func hmac(key: Data, data: Data) -> Data {
        var paddedKey = [UInt8](repeating: 0, count: Blake2s.blockLength)
        if key.count > Blake2s.blockLength {
            let digest = Blake2s.hash(key)
            for (index, byte) in digest.enumerated() { paddedKey[index] = byte }
        } else {
            for (index, byte) in key.enumerated() { paddedKey[index] = byte }
        }

        var inner = Blake2s()
        inner.update(paddedKey.map { $0 ^ 0x36 })
        inner.update(data)
        let innerDigest = inner.finalize()

        var outer = Blake2s()
        outer.update(paddedKey.map { $0 ^ 0x5c })
        outer.update(innerDigest)
        return outer.finalize()
    }

    /// Noise HKDF. Returns two or three outputs depending on `outputs`; the
    /// third is only asked for by patterns with a pre-shared key, which this
    /// one does not have, but generating it is cheap and keeps the function
    /// honest to the spec.
    static func hkdf(chainingKey: Data, material: Data, outputs: Int) -> [Data] {
        precondition((2...3).contains(outputs), "Noise HKDF produces two or three outputs")
        let tempKey = hmac(key: chainingKey, data: material)
        let first = hmac(key: tempKey, data: Data([0x01]))
        let second = hmac(key: tempKey, data: first + Data([0x02]))
        if outputs == 2 { return [first, second] }
        let third = hmac(key: tempKey, data: second + Data([0x03]))
        return [first, second, third]
    }
}

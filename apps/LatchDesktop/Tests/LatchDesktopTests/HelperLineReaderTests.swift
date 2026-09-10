import XCTest
@testable import LatchDesktop

final class HelperLineReaderTests: XCTestCase {
    func testEnrollmentEventsArriveWhileAnExistingSessionIsIdle() async throws {
        let session = Pipe()
        let enrollment = Pipe()
        let sessionReader = HelperLineReader(handle: session.fileHandleForReading)
        let enrollmentReader = HelperLineReader(handle: enrollment.fileHandleForReading)
        defer {
            sessionReader.stop()
            enrollmentReader.stop()
            try? session.fileHandleForWriting.close()
            try? enrollment.fileHandleForWriting.close()
        }
        let sessionReady = expectation(description: "existing session is reading events")
        let comparison = expectation(description: "second device requests comparison while session stays open")
        let sessionTask = Task {
            for await line in sessionReader.lines {
                XCTAssertEqual(line, "ready")
                sessionReady.fulfill()
            }
        }
        let enrollmentTask = Task {
            for await line in enrollmentReader.lines {
                XCTAssertEqual(line, "enrollment_pending")
                comparison.fulfill()
            }
        }
        defer { sessionTask.cancel(); enrollmentTask.cancel() }
        try session.fileHandleForWriting.write(contentsOf: Data("ready\n".utf8))
        await fulfillment(of: [sessionReady], timeout: 2)
        try enrollment.fileHandleForWriting.write(contentsOf: Data("enrollment_pending\n".utf8))
        await fulfillment(of: [comparison], timeout: 2)
    }

    func testFragmentedUTF8MultipleLinesAndFinalUnterminatedLine() async throws {
        let pipe = Pipe()
        let reader = HelperLineReader(handle: pipe.fileHandleForReading)
        defer { reader.stop() }
        let collected = Task { () -> [String] in
            var lines: [String] = []
            for await line in reader.lines { lines.append(line) }
            return lines
        }
        let bytes = Data("Téléphone\r\ncommitted\ncomplete".utf8)
        // Split in the middle of the first multibyte character.
        try pipe.fileHandleForWriting.write(contentsOf: bytes.prefix(2))
        try pipe.fileHandleForWriting.write(contentsOf: bytes.dropFirst(2))
        try pipe.fileHandleForWriting.close()
        let lines = await collected.value
        XCTAssertEqual(lines, ["Téléphone", "committed", "complete"])
    }

    func testStoppingAnIdleReaderFinishesItsStream() async {
        let pipe = Pipe()
        let reader = HelperLineReader(handle: pipe.fileHandleForReading)
        defer { try? pipe.fileHandleForWriting.close() }
        let finished = expectation(description: "stopped stream finishes")
        let task = Task {
            for await _ in reader.lines { XCTFail("idle pipe should have no lines") }
            finished.fulfill()
        }
        defer { task.cancel() }
        reader.stop()
        await fulfillment(of: [finished], timeout: 2)
    }
}

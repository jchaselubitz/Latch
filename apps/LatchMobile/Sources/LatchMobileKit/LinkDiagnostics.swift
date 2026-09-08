import Foundation
import Observation

/// One measured stage. Names are a fixed vocabulary and values are durations;
/// nothing here names a session, a path, a network, or a person.
public struct LinkStageSample: Codable, Equatable, Hashable, Sendable {
    public enum Stage: String, Codable, Sendable, CaseIterable {
        /// Control-plane ticket request.
        case admission
        /// Transport connect (TLS/WebSocket or LAN TCP).
        case connect
        /// Waiting for the relay to report the Mac present.
        case peerWait
        /// Noise XX plus LinkHello.
        case authenticate
        /// Tap or foreground to an authenticated link.
        case linkReady
        /// `/v2/capabilities` after link ready.
        case discovery
        /// Link ready plus discovery: the gateway is usable.
        case applicationReady
        /// Opening one logical gateway stream.
        case streamOpen
        /// First byte to last byte of one gateway response.
        case responseComplete
        /// Session list request.
        case sessionList
        /// Preview capture request.
        case preview
        /// Terminal attach to the first pane byte.
        case terminalFirstOutput
        /// Cold open only: process launch to a usable gateway (or to the
        /// failure that ended the attempt). Measured from the kernel's
        /// process start time, so pre-main work is included.
        case launch
    }

    public enum Outcome: String, Codable, Sendable {
        case ok
        case failed
        case skipped
    }

    public var stage: Stage
    public var milliseconds: UInt64
    public var outcome: Outcome

    public init(stage: Stage, milliseconds: UInt64, outcome: Outcome = .ok) {
        self.stage = stage
        self.milliseconds = milliseconds
        self.outcome = outcome
    }
}

/// One diagnostics attempt: a foreground/reconnect cycle plus the listed
/// operations, or a single instrumented open.
public struct DiagnosticsAttempt: Codable, Equatable, Sendable, Identifiable {
    public enum Kind: String, Codable, Sendable {
        /// Deliberate suspend and resume of the link owner.
        case reconnectCycle = "reconnect_cycle"
        /// The app's own first connection after launch.
        case coldOpen = "cold_open"
    }

    public var id: UUID
    public var attempt: Int
    public var kind: Kind
    public var startedAt: Date
    /// `local`, `relay`, or nil when no link authenticated.
    public var path: String?
    /// Diagnostics-only: the LAN attempt was skipped so the relay was measured.
    public var skippedLAN: Bool
    public var stages: [LinkStageSample]
    public var succeeded: Bool
    /// The first stage that failed, when one did.
    public var failureStage: LinkStageSample.Stage?

    public init(
        id: UUID = UUID(),
        attempt: Int,
        kind: Kind,
        startedAt: Date,
        path: String?,
        skippedLAN: Bool,
        stages: [LinkStageSample],
        succeeded: Bool,
        failureStage: LinkStageSample.Stage?
    ) {
        self.id = id
        self.attempt = attempt
        self.kind = kind
        self.startedAt = startedAt
        self.path = path
        self.skippedLAN = skippedLAN
        self.stages = stages
        self.succeeded = succeeded
        self.failureStage = failureStage
    }

    public func milliseconds(for stage: LinkStageSample.Stage) -> UInt64? {
        stages.first { $0.stage == stage && $0.outcome == .ok }?.milliseconds
    }
}

/// Diagnostics-only settings. `skipLANAttempt` selects between two entry
/// points of the same protocol so the physical matrix can measure the relay
/// from a network where the Mac is also on the LAN; it is not a transport
/// flag and defaults to off.
public struct DiagnosticsSettings: Equatable, Sendable, Codable {
    public var skipLANAttempt: Bool
    public var cycles: Int
    public var includeTerminal: Bool
    /// Pause between cycles so the relay's per-device admission budget is
    /// measured as recovery, not throttled as abuse.
    public var pauseSeconds: Int

    public init(skipLANAttempt: Bool = false, cycles: Int = 30, includeTerminal: Bool = false, pauseSeconds: Int = 2) {
        self.skipLANAttempt = skipLANAttempt
        self.cycles = min(max(cycles, 1), 500)
        self.includeTerminal = includeTerminal
        self.pauseSeconds = min(max(pauseSeconds, 0), 60)
    }
}

public protocol DiagnosticsSettingsStoring: Sendable {
    func load() -> DiagnosticsSettings
    func save(_ settings: DiagnosticsSettings)
}

public struct UserDefaultsDiagnosticsSettingsStore: DiagnosticsSettingsStoring {
    private nonisolated(unsafe) let defaults: UserDefaults
    private let key: String

    public init(defaults: UserDefaults = .standard, key: String = "remoteLinkDiagnostics") {
        self.defaults = defaults
        self.key = key
    }

    public func load() -> DiagnosticsSettings {
        guard let data = defaults.data(forKey: key),
              let settings = try? JSONDecoder().decode(DiagnosticsSettings.self, from: data)
        else { return DiagnosticsSettings() }
        return settings
    }

    public func save(_ settings: DiagnosticsSettings) {
        guard let data = try? JSONEncoder().encode(settings) else { return }
        defaults.set(data, forKey: key)
    }
}

public final class MemoryDiagnosticsSettingsStore: DiagnosticsSettingsStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var settings: DiagnosticsSettings

    public init(_ settings: DiagnosticsSettings = DiagnosticsSettings()) { self.settings = settings }
    public func load() -> DiagnosticsSettings { lock.withLock { settings } }
    public func save(_ settings: DiagnosticsSettings) { lock.withLock { self.settings = settings } }
}

/// Where attempts are written so Objective 3 can read them off the device.
public protocol DiagnosticsWriting: Sendable {
    /// A description of where the run is stored, for the Settings screen.
    var location: String? { get }
    func begin(runStartedAt: Date) throws
    func write(_ attempt: DiagnosticsAttempt) throws
}

/// JSON Lines under the app's Documents directory, one file per run, so the
/// Files app (and a USB copy) can export them. Nothing else is written.
public final class FileDiagnosticsWriter: DiagnosticsWriting, @unchecked Sendable {
    private let directory: URL
    private let lock = NSLock()
    private var handle: FileHandle?
    private var path: URL?

    public init(directory: URL? = nil) {
        self.directory = directory
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first!
                .appendingPathComponent("latch-diagnostics", isDirectory: true)
    }

    public var location: String? { lock.withLock { path?.lastPathComponent } }

    public func begin(runStartedAt: Date) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withFullDate, .withTime, .withTimeZone]
        let stamp = formatter.string(from: runStartedAt)
            .replacingOccurrences(of: ":", with: "")
            .replacingOccurrences(of: "-", with: "")
        let url = directory.appendingPathComponent("run-\(stamp).jsonl")
        FileManager.default.createFile(atPath: url.path, contents: nil)
        lock.withLock {
            handle = try? FileHandle(forWritingTo: url)
            path = url
        }
    }

    public func write(_ attempt: DiagnosticsAttempt) throws {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.sortedKeys]
        var line = try encoder.encode(attempt)
        line.append(0x0a)
        try lock.withLock {
            guard let handle else { return }
            try handle.seekToEnd()
            try handle.write(contentsOf: line)
        }
    }
}

public final class MemoryDiagnosticsWriter: DiagnosticsWriting, @unchecked Sendable {
    private let lock = NSLock()
    public private(set) var attempts: [DiagnosticsAttempt] = []
    public var location: String? { "memory" }

    public init() {}
    public func begin(runStartedAt: Date) throws {}
    public func write(_ attempt: DiagnosticsAttempt) throws { lock.withLock { attempts.append(attempt) } }
}

/// What the runner drives. `AppModel` conforms; tests use a fake.
@MainActor
public protocol DiagnosticsSubject: AnyObject {
    /// Suspends and resumes the link owner as the app would across a
    /// background/foreground transition, returning the link and discovery
    /// stages. Throws when the link did not become usable.
    func diagnosticsRecoveryCycle(skipLAN: Bool) async throws -> (path: String?, stages: [LinkStageSample])
    func diagnosticsSessionList() async throws -> LinkStageSample
    /// nil when there is no session to preview.
    func diagnosticsPreview() async throws -> LinkStageSample?
    /// nil when the terminal is not permitted or there is no running session.
    func diagnosticsTerminalFirstOutput() async throws -> LinkStageSample?
}

/// Opt-in, in-app driver for the foreground/reconnect part of the physical
/// matrix. It performs real cycles through the real link owner and records
/// content-free per-attempt stage timings; it never invents a result.
@MainActor
@Observable
public final class DiagnosticsRunner {
    public private(set) var attempts: [DiagnosticsAttempt] = []
    public private(set) var isRunning = false
    public private(set) var lastError: String?
    public var settings: DiagnosticsSettings {
        didSet { store.save(settings) }
    }

    private let store: any DiagnosticsSettingsStoring
    private let writer: any DiagnosticsWriting
    private let clock: @Sendable () -> Date
    private let pause: @Sendable (Duration) async throws -> Void
    private var task: Task<Void, Never>?

    public init(
        store: any DiagnosticsSettingsStoring = UserDefaultsDiagnosticsSettingsStore(),
        writer: any DiagnosticsWriting = FileDiagnosticsWriter(),
        clock: @escaping @Sendable () -> Date = { Date() },
        pause: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    ) {
        self.store = store
        self.writer = writer
        self.clock = clock
        self.pause = pause
        self.settings = store.load()
    }

    public var location: String? { writer.location }

    /// The p95 of a stage across successful attempts, or nil below two samples.
    public func p95(_ stage: LinkStageSample.Stage) -> UInt64? {
        let values = attempts.compactMap { $0.milliseconds(for: stage) }.sorted()
        guard values.count >= 2 else { return nil }
        let index = Int((Double(values.count - 1) * 0.95).rounded(.up))
        return values[min(index, values.count - 1)]
    }

    public var successCount: Int { attempts.filter(\.succeeded).count }

    public func run(subject: DiagnosticsSubject) {
        guard !isRunning else { return }
        isRunning = true
        lastError = nil
        attempts = []
        let settings = self.settings
        task = Task { [weak self, weak subject] in
            guard let self else { return }
            do { try writer.begin(runStartedAt: clock()) } catch { lastError = error.localizedDescription }
            for index in 1...settings.cycles {
                guard !Task.isCancelled, let subject else { break }
                let attempt = await self.cycle(index: index, subject: subject, settings: settings)
                attempts.append(attempt)
                do { try writer.write(attempt) } catch { lastError = error.localizedDescription }
                if index < settings.cycles, settings.pauseSeconds > 0 {
                    try? await pause(.seconds(settings.pauseSeconds))
                }
            }
            isRunning = false
        }
    }

    public func cancel() {
        task?.cancel()
        task = nil
        isRunning = false
    }

    private func cycle(index: Int, subject: DiagnosticsSubject, settings: DiagnosticsSettings) async -> DiagnosticsAttempt {
        let startedAt = clock()
        var stages: [LinkStageSample] = []
        var path: String?
        var failure: LinkStageSample.Stage?
        do {
            let recovered = try await subject.diagnosticsRecoveryCycle(skipLAN: settings.skipLANAttempt)
            path = recovered.path
            stages.append(contentsOf: recovered.stages)
        } catch {
            failure = .linkReady
            stages.append(LinkStageSample(stage: .linkReady, milliseconds: elapsed(since: startedAt), outcome: .failed))
        }
        if failure == nil {
            do { stages.append(try await subject.diagnosticsSessionList()) } catch {
                failure = .sessionList
                stages.append(LinkStageSample(stage: .sessionList, milliseconds: 0, outcome: .failed))
            }
        }
        if failure == nil {
            do {
                if let sample = try await subject.diagnosticsPreview() { stages.append(sample) }
                else { stages.append(LinkStageSample(stage: .preview, milliseconds: 0, outcome: .skipped)) }
            } catch {
                failure = .preview
                stages.append(LinkStageSample(stage: .preview, milliseconds: 0, outcome: .failed))
            }
        }
        if failure == nil, settings.includeTerminal {
            do {
                if let sample = try await subject.diagnosticsTerminalFirstOutput() { stages.append(sample) }
                else { stages.append(LinkStageSample(stage: .terminalFirstOutput, milliseconds: 0, outcome: .skipped)) }
            } catch {
                failure = .terminalFirstOutput
                stages.append(LinkStageSample(stage: .terminalFirstOutput, milliseconds: 0, outcome: .failed))
            }
        }
        return DiagnosticsAttempt(
            attempt: index,
            kind: .reconnectCycle,
            startedAt: startedAt,
            path: path,
            skippedLAN: settings.skipLANAttempt,
            stages: stages,
            succeeded: failure == nil,
            failureStage: failure
        )
    }

    private func elapsed(since date: Date) -> UInt64 {
        UInt64(max(0, clock().timeIntervalSince(date) * 1000))
    }
}

/// Measures one operation as a stage sample.
public func measureStage<T>(
    _ stage: LinkStageSample.Stage,
    clock: () -> Date = { Date() },
    _ body: () async throws -> T
) async throws -> (T, LinkStageSample) {
    let started = clock()
    let value = try await body()
    let milliseconds = UInt64(max(0, clock().timeIntervalSince(started) * 1000))
    return (value, LinkStageSample(stage: stage, milliseconds: milliseconds))
}


/// JSON Lines appended to one fixed file across launches. Cold opens are one
/// record per process, so a per-run file would be a file per launch; the
/// harness that drives launches reads this one file instead.
public final class AppendingFileDiagnosticsWriter: DiagnosticsWriting, @unchecked Sendable {
    private let url: URL
    private let lock = NSLock()

    public init(directory: URL? = nil, fileName: String = "cold-opens.jsonl") {
        let base = directory
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first!
                .appendingPathComponent("latch-diagnostics", isDirectory: true)
        url = base.appendingPathComponent(fileName)
    }

    public var location: String? { url.lastPathComponent }

    public func begin(runStartedAt: Date) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true
        )
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
    }

    public func write(_ attempt: DiagnosticsAttempt) throws {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        encoder.outputFormatting = [.sortedKeys]
        var line = try encoder.encode(attempt)
        line.append(0x0a)
        try lock.withLock {
            let handle = try FileHandle(forWritingTo: url)
            defer { try? handle.close() }
            try handle.seekToEnd()
            try handle.write(contentsOf: line)
        }
    }
}

/// When this process started, from the kernel rather than from whichever
/// Swift initializer happened to run first. Falls back to "now" if the
/// sysctl is refused, which makes a cold open read shorter, never longer.
public enum ProcessLaunch {
    public static let date: Date = startDate()

    static func startDate() -> Date {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&mib, UInt32(mib.count), &info, &size, nil, 0) == 0 else { return Date() }
        let start = info.kp_proc.p_starttime
        return Date(timeIntervalSince1970: TimeInterval(start.tv_sec) + TimeInterval(start.tv_usec) / 1_000_000)
    }
}

/// Records the app's own first connection after launch as one `cold_open`
/// attempt: the plan's definition of a cold open is the process launched
/// from not-running to a usable gateway. The recorder is armed once per
/// process and writes exactly one line, on the first `applicationReady`
/// stage or on the first state that ends the attempt (Mac offline, revoked,
/// pairing required, or a third failed connect). It never launches anything
/// itself; the USB harness on the Mac does that.
@MainActor
public final class ColdOpenRecorder {
    public private(set) var recorded: DiagnosticsAttempt?
    private let writer: any DiagnosticsWriting
    private let launchedAt: Date
    private let clock: () -> Date
    private let enabled: Bool
    private var stages: [LinkStageSample] = []
    private var path: String?
    private var skippedLAN = false

    /// `enabled` defaults to the launch-argument switch so an ordinary
    /// launch writes nothing.
    public init(
        writer: any DiagnosticsWriting = AppendingFileDiagnosticsWriter(),
        launchedAt: Date = ProcessLaunch.date,
        enabled: Bool = DiagnosticsLaunchOptions.current.recordColdOpen,
        clock: @escaping () -> Date = { Date() }
    ) {
        self.writer = writer
        self.launchedAt = launchedAt
        self.enabled = enabled
        self.clock = clock
    }

    public var isArmed: Bool { enabled && recorded == nil }

    public func observe(skipLAN: Bool) { skippedLAN = skipLAN }

    public func observe(path: RemotePath?) {
        guard isArmed, let path else { return }
        self.path = path.rawValue
    }

    public func observe(_ sample: LinkStageSample) {
        guard isArmed else { return }
        stages.append(sample)
        if sample.stage == .applicationReady, sample.outcome == .ok { finish(failure: nil) }
    }

    public func observe(_ state: RemoteLinkState) {
        guard isArmed else { return }
        switch state {
        case .macOffline, .revoked, .pairingRequired:
            finish(failure: .linkReady)
        case .backoff(let attempt, _, _) where attempt >= 3:
            finish(failure: .linkReady)
        default:
            break
        }
    }

    private func finish(failure: LinkStageSample.Stage?) {
        let total = UInt64(max(0, clock().timeIntervalSince(launchedAt) * 1000))
        stages.append(LinkStageSample(stage: .launch, milliseconds: total, outcome: failure == nil ? .ok : .failed))
        let attempt = DiagnosticsAttempt(
            attempt: 1,
            kind: .coldOpen,
            startedAt: launchedAt,
            path: path,
            skippedLAN: skippedLAN,
            stages: stages,
            succeeded: failure == nil,
            failureStage: failure
        )
        recorded = attempt
        do {
            try writer.begin(runStartedAt: launchedAt)
            try writer.write(attempt)
        } catch {
            // A diagnostics write failure must never touch the link.
        }
    }
}

/// Diagnostics switches passed on the command line by the USB harness
/// (`xcrun devicectl device process launch ... -- -latchDiagnostics...`).
/// They are read once, never persisted, and absent on an ordinary launch.
public struct DiagnosticsLaunchOptions: Equatable, Sendable {
    /// Write one `cold_open` record for this process.
    public var recordColdOpen: Bool
    /// Start the reconnect-cycle runner as soon as the paired route is up.
    public var autoRunCycles: Int?
    public var skipLAN: Bool?
    public var includeTerminal: Bool?
    public var pauseSeconds: Int?

    public init(
        recordColdOpen: Bool = false,
        autoRunCycles: Int? = nil,
        skipLAN: Bool? = nil,
        includeTerminal: Bool? = nil,
        pauseSeconds: Int? = nil
    ) {
        self.recordColdOpen = recordColdOpen
        self.autoRunCycles = autoRunCycles
        self.skipLAN = skipLAN
        self.includeTerminal = includeTerminal
        self.pauseSeconds = pauseSeconds
    }

    public static let current = DiagnosticsLaunchOptions(arguments: ProcessInfo.processInfo.arguments)

    public init(arguments: [String]) {
        var values: [String: String] = [:]
        var index = 0
        while index < arguments.count {
            let key = arguments[index]
            if key.hasPrefix("-latchDiagnostics"), index + 1 < arguments.count {
                values[String(key.dropFirst())] = arguments[index + 1]
                index += 2
            } else {
                index += 1
            }
        }
        func flag(_ name: String) -> Bool? {
            guard let raw = values[name] else { return nil }
            return ["1", "true", "yes"].contains(raw.lowercased())
        }
        func number(_ name: String) -> Int? {
            values[name].flatMap(Int.init)
        }
        self.init(
            recordColdOpen: flag("latchDiagnosticsColdOpen") ?? false,
            autoRunCycles: number("latchDiagnosticsCycles").map { min(max($0, 1), 500) },
            skipLAN: flag("latchDiagnosticsSkipLAN"),
            includeTerminal: flag("latchDiagnosticsTerminal"),
            pauseSeconds: number("latchDiagnosticsPause").map { min(max($0, 0), 60) }
        )
    }

    public var autoRuns: Bool { autoRunCycles != nil }

    /// Settings for an automated run: launch values override the saved ones.
    public func applied(to settings: DiagnosticsSettings) -> DiagnosticsSettings {
        DiagnosticsSettings(
            skipLANAttempt: skipLAN ?? settings.skipLANAttempt,
            cycles: autoRunCycles ?? settings.cycles,
            includeTerminal: includeTerminal ?? settings.includeTerminal,
            pauseSeconds: pauseSeconds ?? settings.pauseSeconds
        )
    }
}

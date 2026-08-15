import Foundation
import AppKit

// Latch Desktop is distributed as a notarized ZIP on the same GitHub release
// as the CLI, so updating it means replacing one app bundle with another one
// from that release. Sparkle would bring an appcast, a signing scheme, and an
// XPC installer; none of that buys anything here, because the archive is
// already Developer ID signed and Apple-notarized and macOS will verify both
// for us. What this file has to get right is narrower:
//
//   * never replace the running bundle with something signed by somebody else;
//   * fail before downloading when the bundle cannot be written anyway;
//   * leave the installed app untouched when any step fails.

/// A three-component Latch version.
///
/// Latch versions are `0.YYMMDDHHMM.0`, so the middle component is a
/// timestamp: comparing components as numbers is comparing release order,
/// while comparing the strings would call `0.99.0` newer than `0.100.0`.
struct ReleaseVersion: Comparable, CustomStringConvertible, Sendable {
    let components: [UInt64]

    init?(_ raw: String) {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let digits = trimmed.hasPrefix("v") ? String(trimmed.dropFirst()) : trimmed
        let parts = digits.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 3 else { return nil }
        var parsed: [UInt64] = []
        for part in parts {
            guard let value = UInt64(part) else { return nil }
            parsed.append(value)
        }
        components = parsed
    }

    static func == (lhs: ReleaseVersion, rhs: ReleaseVersion) -> Bool {
        lhs.components == rhs.components
    }

    static func < (lhs: ReleaseVersion, rhs: ReleaseVersion) -> Bool {
        for (left, right) in zip(lhs.components, rhs.components) where left != right {
            return left < right
        }
        return false
    }

    var description: String { components.map(String.init).joined(separator: ".") }
}

/// One file attached to a release.
struct ReleaseAsset: Decodable, Sendable {
    let name: String
    let downloadURL: URL

    enum CodingKeys: String, CodingKey {
        case name
        case downloadURL = "browser_download_url"
    }
}

/// A published release, as much of it as the updater needs.
struct PublishedRelease: Decodable, Sendable {
    let tag: String
    let releasePage: URL
    let notes: String?
    let assets: [ReleaseAsset]

    enum CodingKeys: String, CodingKey {
        case tag = "tag_name"
        case releasePage = "html_url"
        case notes = "body"
        case assets
    }

    var version: ReleaseVersion? { ReleaseVersion(tag) }

    /// The macOS application archive, matched on the shape the release script
    /// produces (`Latch-<version>-macos.zip`) rather than on an exact name, so
    /// a version that does not match the tag still resolves.
    var desktopArchive: ReleaseAsset? {
        assets.first { $0.name.hasPrefix("Latch-") && $0.name.hasSuffix("-macos.zip") }
    }
}

/// A newer release than the one running.
struct AvailableUpdate: Equatable, Sendable {
    let version: ReleaseVersion
    let archive: URL
    let releasePage: URL
    let notes: String?

    static func == (lhs: AvailableUpdate, rhs: AvailableUpdate) -> Bool {
        lhs.version == rhs.version && lhs.archive == rhs.archive
    }
}

/// Resolves the newest release, or nothing when the running build is current.
///
/// Separated from downloading and installing because this is the part with a
/// decision in it, and the only part that can be asserted without a signed
/// bundle and a writable /Applications.
enum UpdateResolution {
    static func newerRelease(
        in release: PublishedRelease,
        thanInstalled installed: ReleaseVersion
    ) throws -> AvailableUpdate? {
        guard let version = release.version else {
            throw UpdateError.unreadableFeed("release \(release.tag) is not a Latch version")
        }
        guard version > installed else { return nil }
        guard let archive = release.desktopArchive else {
            throw UpdateError.noArchive(version.description, release.releasePage)
        }
        return AvailableUpdate(
            version: version,
            archive: archive.downloadURL,
            releasePage: release.releasePage,
            notes: release.notes
        )
    }
}

enum UpdateError: LocalizedError, Equatable {
    case unknownInstalledVersion
    case unreadableFeed(String)
    case noArchive(String, URL)
    case translocated
    case notWritable(String)
    case unpackFailed(String)
    case noApplicationInArchive
    case identityMismatch(String)
    case unsignedDownload
    case installFailed(String)
    case relaunchFailed(String)

    var errorDescription: String? {
        switch self {
        case .unknownInstalledVersion:
            return "This build of Latch does not report a version, so it cannot be updated in place."
        case .unreadableFeed(let detail):
            return "Latch could not read the release feed: \(detail)"
        case .noArchive(let version, let page):
            return "Latch \(version) is available but publishes no macOS app archive. Download it from \(page.absoluteString)."
        case .translocated:
            return "Latch is running from a temporary quarantine location. Move Latch to your Applications folder, reopen it, and try again."
        case .notWritable(let path):
            return "Latch cannot write to \(path). Move Latch to a folder you own, or download the update by hand."
        case .unpackFailed(let detail):
            return "Latch could not expand the downloaded archive: \(detail)"
        case .noApplicationInArchive:
            return "The downloaded archive did not contain a Latch application."
        case .identityMismatch(let detail):
            return "The downloaded update is signed by a different developer (\(detail)) and was not installed."
        case .unsignedDownload:
            return "The downloaded update is not validly signed and was not installed."
        case .installFailed(let detail):
            return "Latch could not replace the installed application: \(detail)"
        case .relaunchFailed(let detail):
            return "Latch installed the update but could not schedule its relaunch: \(detail)"
        }
    }
}

/// Where release metadata comes from.
protocol ReleaseFeed {
    func latestRelease() async throws -> PublishedRelease
}

/// The GitHub releases of one repository.
struct GitHubReleaseFeed: ReleaseFeed {
    /// A compiled-in constant, not a preference: an updater that can be
    /// pointed at another origin is a way to be handed a different app.
    static let defaultRepository = "jchaselubitz/Latch"

    let repository: String
    let session: URLSession

    init(repository: String = GitHubReleaseFeed.defaultRepository, session: URLSession = .shared) {
        self.repository = repository
        self.session = session
    }

    func latestRelease() async throws -> PublishedRelease {
        let url = URL(string: "https://api.github.com/repos/\(repository)/releases/latest")!
        var request = URLRequest(url: url)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.timeoutInterval = 30
        let (data, response) = try await session.data(for: request)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            throw UpdateError.unreadableFeed("the release feed answered HTTP \(http.statusCode)")
        }
        do {
            return try JSONDecoder().decode(PublishedRelease.self, from: data)
        } catch {
            throw UpdateError.unreadableFeed(error.localizedDescription)
        }
    }
}

/// Downloads an update and swaps it in for the running application bundle.
enum UpdateInstaller {
    /// Replaces `bundle` with the application inside `update`'s archive and
    /// returns the installed bundle.
    ///
    /// Every step happens in a replacement directory on the same volume as the
    /// installed app, so the final swap is `replaceItemAt` — the installed
    /// path is either the old bundle or the new one, never a half-written mix.
    static func install(
        _ update: AvailableUpdate,
        replacing bundle: URL,
        session: URLSession = .shared
    ) async throws -> URL {
        try preflight(bundle)

        let (downloaded, response) = try await session.download(from: update.archive)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            try? FileManager.default.removeItem(at: downloaded)
            throw UpdateError.unreadableFeed("the download answered HTTP \(http.statusCode)")
        }

        let manager = FileManager.default
        let staging = try manager.url(
            for: .itemReplacementDirectory,
            in: .userDomainMask,
            appropriateFor: bundle,
            create: true
        )
        defer { try? manager.removeItem(at: staging) }

        let archive = staging.appendingPathComponent("Latch.zip")
        try manager.moveItem(at: downloaded, to: archive)

        let expanded = staging.appendingPathComponent("expanded", isDirectory: true)
        try manager.createDirectory(at: expanded, withIntermediateDirectories: true)
        // `ditto` is what produced the archive; `unzip` would drop the
        // extended attributes a notarization ticket is stapled into.
        let unpack = try run("/usr/bin/ditto", ["-x", "-k", archive.path, expanded.path])
        guard unpack.status == 0 else { throw UpdateError.unpackFailed(unpack.diagnostic) }

        guard let replacement = applicationBundle(in: expanded, manager: manager) else {
            throw UpdateError.noApplicationInArchive
        }
        try verify(replacement, matching: bundle)

        do {
            _ = try manager.replaceItemAt(bundle, withItemAt: replacement)
        } catch {
            throw UpdateError.installFailed(error.localizedDescription)
        }
        return bundle
    }

    /// Refuses, before anything is downloaded, the two cases where the swap
    /// could not succeed: a quarantined copy running from a read-only
    /// translocation mount, and a bundle in a directory the user cannot write.
    static func preflight(_ bundle: URL) throws {
        if bundle.path.contains("/AppTranslocation/") {
            throw UpdateError.translocated
        }
        let parent = bundle.deletingLastPathComponent().path
        guard FileManager.default.isWritableFile(atPath: parent) else {
            throw UpdateError.notWritable(parent)
        }
    }

    /// Requires the downloaded app to be signed by whoever signed the running
    /// one.
    ///
    /// Conditional on the running bundle being signed at all, so a local
    /// `build-app.sh` build can still update itself, but a Developer ID
    /// install can never be replaced by something signed by somebody else.
    /// HTTPS says the bytes came from GitHub; only the signature says who
    /// built them.
    static func verify(_ replacement: URL, matching current: URL) throws {
        guard let expected = signingTeam(of: current) else { return }
        guard let actual = signingTeam(of: replacement) else {
            throw UpdateError.unsignedDownload
        }
        guard actual == expected else {
            throw UpdateError.identityMismatch("team \(actual), expected \(expected)")
        }
        let assessment = try run("/usr/sbin/spctl", ["--assess", "--type", "execute", replacement.path])
        guard assessment.status == 0 else {
            throw UpdateError.identityMismatch("rejected by Gatekeeper: \(assessment.diagnostic)")
        }
    }

    /// The Team ID of a validly signed bundle, or nil when it is unsigned or
    /// ad-hoc signed.
    static func signingTeam(of bundle: URL) -> String? {
        guard let verified = try? run("/usr/bin/codesign", ["--verify", "--strict", bundle.path]),
              verified.status == 0,
              let described = try? run("/usr/bin/codesign", ["--display", "--verbose=4", bundle.path])
        else { return nil }
        // `codesign --display` writes its description to stderr.
        return teamIdentifier(inCodesignOutput: described.diagnostic)
    }

    /// Reads `TeamIdentifier=` out of `codesign --display` output.
    static func teamIdentifier(inCodesignOutput output: String) -> String? {
        for line in output.split(separator: "\n") {
            guard line.hasPrefix("TeamIdentifier=") else { continue }
            let team = line.dropFirst("TeamIdentifier=".count).trimmingCharacters(in: .whitespaces)
            return team.isEmpty || team == "not set" ? nil : team
        }
        return nil
    }

    /// The first `.app` at the top level of an expanded archive.
    static func applicationBundle(in directory: URL, manager: FileManager) -> URL? {
        let entries = (try? manager.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        )) ?? []
        return entries.first { $0.pathExtension == "app" }
    }

    /// Quits and reopens the app once this process is gone.
    ///
    /// The relaunch has to outlive the process being relaunched, so it is a
    /// detached shell that waits for this PID to disappear. `open` is used
    /// without `-n`: forcing a new instance creates an extra Dock icon and
    /// bypasses Launch Services' normal single-instance behavior. The PID and
    /// bundle path are passed as arguments rather than interpolated into the
    /// script, so a path with quotes or spaces cannot become shell syntax.
    static let relaunchScript = """
    while /bin/kill -0 "$1" 2>/dev/null; do /bin/sleep 0.2; done
    exec /usr/bin/open "$2"
    """

    @MainActor
    static func relaunch(_ bundle: URL) throws {
        let pid = ProcessInfo.processInfo.processIdentifier
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = ["-c", relaunchScript, "latch-relaunch", String(pid), bundle.path]
        do {
            try process.run()
        } catch {
            throw UpdateError.relaunchFailed(error.localizedDescription)
        }

        // Closing the sheet before asking AppKit to terminate avoids issuing a
        // termination request re-entrantly from its button action. Scheduling
        // it on the next main-loop turn lets the helper observe a clean exit.
        DispatchQueue.main.async {
            NSApp.terminate(nil)
        }
    }

    private struct ToolResult {
        let status: Int32
        let diagnostic: String
    }

    private static func run(_ executable: String, _ arguments: [String]) throws -> ToolResult {
        let process = Process()
        let errors = Pipe()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.standardOutput = Pipe()
        process.standardError = errors
        try process.run()
        let diagnostic = errors.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return ToolResult(
            status: process.terminationStatus,
            diagnostic: String(decoding: diagnostic.suffix(8_192), as: UTF8.self)
                .trimmingCharacters(in: .whitespacesAndNewlines)
        )
    }
}

/// Drives update checks and installation for the app.
@MainActor
final class UpdateController: ObservableObject {
    /// What the update UI is showing.
    enum Phase: Equatable {
        case idle
        case checking
        case upToDate
        case available(AvailableUpdate)
        case downloading
        case failed(String)
    }

    @Published private(set) var phase: Phase = .idle
    @Published var isPresented = false
    @Published var automaticChecks: Bool {
        didSet { LatchClient.preferences.set(automaticChecks, forKey: Self.automaticKey) }
    }

    /// Version of the running bundle, or nil for a build with no Info.plist
    /// version — `swift run` during development.
    let installedVersion: ReleaseVersion?
    /// The version string as the bundle reports it, shown even when it does
    /// not parse so the reader can see what it is.
    let installedVersionLabel: String

    private static let automaticKey = "automaticUpdateChecks"
    private static let lastCheckKey = "lastUpdateCheckAt"
    private static let checkInterval: TimeInterval = 24 * 60 * 60

    private let feed: ReleaseFeed
    private let bundleURL: URL
    private var checkTask: Task<Void, Never>?

    init(
        feed: ReleaseFeed = GitHubReleaseFeed(),
        bundleURL: URL = Bundle.main.bundleURL,
        installedVersion: String? = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String
    ) {
        self.feed = feed
        self.bundleURL = bundleURL
        self.installedVersionLabel = installedVersion ?? "unknown"
        self.installedVersion = installedVersion.flatMap(ReleaseVersion.init)
        self.automaticChecks = LatchClient.preferences.object(forKey: Self.automaticKey) as? Bool ?? true
    }

    /// True while an update is available or being installed, so the UI can
    /// offer it without inspecting the phase.
    var pendingUpdate: AvailableUpdate? {
        if case .available(let update) = phase { return update }
        return nil
    }

    /// Checks on launch and once a day after that, when the user allows it.
    ///
    /// A background check never opens a window: it sets the phase, and the
    /// menu-bar extra and Settings show that a version is waiting.
    func startAutomaticChecks() {
        guard checkTask == nil else { return }
        checkTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                if self.automaticChecks, self.isDueForCheck {
                    await self.check(userInitiated: false)
                }
                try? await Task.sleep(nanoseconds: 60 * 60 * 1_000_000_000)
            }
        }
    }

    private var isDueForCheck: Bool {
        let last = LatchClient.preferences.object(forKey: Self.lastCheckKey) as? Date
        guard let last else { return true }
        return Date().timeIntervalSince(last) >= Self.checkInterval
    }

    /// Checks for a newer release. A user-initiated check always shows its
    /// result, including "you are up to date".
    func check(userInitiated: Bool) async {
        if userInitiated { isPresented = true }
        guard let installed = installedVersion else {
            phase = .failed(UpdateError.unknownInstalledVersion.localizedDescription)
            return
        }
        phase = .checking
        do {
            let release = try await feed.latestRelease()
            LatchClient.preferences.set(Date(), forKey: Self.lastCheckKey)
            if let update = try UpdateResolution.newerRelease(in: release, thanInstalled: installed) {
                phase = .available(update)
                if !userInitiated { isPresented = true }
            } else {
                phase = .upToDate
            }
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    /// Downloads, verifies, and installs an update, then relaunches.
    func install(_ update: AvailableUpdate) async {
        phase = .downloading
        do {
            _ = try await UpdateInstaller.install(update, replacing: bundleURL)
            isPresented = false
            try UpdateInstaller.relaunch(bundleURL)
        } catch {
            phase = .failed(error.localizedDescription)
        }
    }

    func dismiss() {
        isPresented = false
        phase = .idle
    }
}

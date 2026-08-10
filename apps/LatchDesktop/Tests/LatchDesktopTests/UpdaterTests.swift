import XCTest
@testable import LatchDesktop

/// The updater's decisions, separated from the network and from a signed
/// bundle: which release is newer, which asset is the app, and which installs
/// must be refused before anything is downloaded.
final class UpdaterTests: XCTestCase {
    private let feed = """
    {
      "tag_name": "v0.2608101202.0",
      "html_url": "https://github.com/jchaselubitz/Latch/releases/tag/v0.2608101202.0",
      "body": "Notes.",
      "assets": [
        {"name": "checksums.txt",
         "browser_download_url": "https://example.invalid/checksums.txt"},
        {"name": "latch-0.2608101202.0-aarch64-apple-darwin.zip",
         "browser_download_url": "https://example.invalid/cli.zip"},
        {"name": "Latch-0.2608101202.0-macos.zip",
         "browser_download_url": "https://example.invalid/app.zip"}
      ]
    }
    """

    private func decodeFeed() throws -> PublishedRelease {
        try JSONDecoder().decode(PublishedRelease.self, from: Data(feed.utf8))
    }

    func testVersionsOrderByNumberAndNotByString() throws {
        let older = try XCTUnwrap(ReleaseVersion("0.2608100841.0"))
        let newer = try XCTUnwrap(ReleaseVersion("0.2608101202.0"))
        XCTAssertTrue(newer > older)
        XCTAssertEqual(ReleaseVersion("v1.2.3"), ReleaseVersion("1.2.3"))
        // Lexicographic ordering would get this backwards.
        let hundred = try XCTUnwrap(ReleaseVersion("0.100.0"))
        let ninetyNine = try XCTUnwrap(ReleaseVersion("0.99.0"))
        XCTAssertTrue(hundred > ninetyNine)
    }

    func testAVersionThatIsNotThreeNumbersDoesNotParse() {
        for raw in ["", "1.2", "1.2.3.4", "1.2.x", "latest", "0.2608101202.0-beta"] {
            XCTAssertNil(ReleaseVersion(raw), "\(raw) must not parse")
        }
    }

    func testTheFeedResolvesTheApplicationArchiveAndNotTheCLIOne() throws {
        let release = try decodeFeed()
        XCTAssertEqual(release.version, ReleaseVersion("0.2608101202.0"))
        XCTAssertEqual(release.desktopArchive?.name, "Latch-0.2608101202.0-macos.zip")
        XCTAssertEqual(release.notes, "Notes.")
    }

    func testANewerReleaseIsOffered() throws {
        let release = try decodeFeed()
        let installed = try XCTUnwrap(ReleaseVersion("0.2608100841.0"))
        let update = try XCTUnwrap(
            UpdateResolution.newerRelease(in: release, thanInstalled: installed)
        )
        XCTAssertEqual(update.version, ReleaseVersion("0.2608101202.0"))
        XCTAssertEqual(update.archive.absoluteString, "https://example.invalid/app.zip")
    }

    func testTheSameOrANewerInstallIsNotOfferedAnUpdate() throws {
        let release = try decodeFeed()
        for installed in ["0.2608101202.0", "0.2608110000.0"] {
            let version = try XCTUnwrap(ReleaseVersion(installed))
            XCTAssertNil(try UpdateResolution.newerRelease(in: release, thanInstalled: version))
        }
    }

    func testAReleaseWithNoApplicationArchiveReportsWhereToGetItByHand() throws {
        let data = Data("""
        {"tag_name":"v0.2608101202.0","html_url":"https://example.invalid/tag","assets":[
          {"name":"latch-0.2608101202.0-aarch64-apple-darwin.zip",
           "browser_download_url":"https://example.invalid/cli.zip"}]}
        """.utf8)
        let release = try JSONDecoder().decode(PublishedRelease.self, from: data)
        let installed = try XCTUnwrap(ReleaseVersion("0.2608100841.0"))
        XCTAssertThrowsError(
            try UpdateResolution.newerRelease(in: release, thanInstalled: installed)
        ) { error in
            guard let updateError = error as? UpdateError,
                  case .noArchive(let version, _) = updateError
            else {
                XCTFail("expected noArchive, got \(error)")
                return
            }
            XCTAssertEqual(version, "0.2608101202.0")
        }
    }

    func testATagThatIsNotAVersionIsAnErrorAndNotAnUpdate() throws {
        let data = Data(#"{"tag_name":"nightly","html_url":"https://example.invalid","assets":[]}"#.utf8)
        let release = try JSONDecoder().decode(PublishedRelease.self, from: data)
        let installed = try XCTUnwrap(ReleaseVersion("0.1.0"))
        XCTAssertThrowsError(try UpdateResolution.newerRelease(in: release, thanInstalled: installed))
    }

    func testATranslocatedCopyIsRefusedBeforeAnythingIsDownloaded() {
        let translocated = URL(fileURLWithPath:
            "/private/var/folders/ab/AppTranslocation/1234/d/Latch.app")
        XCTAssertThrowsError(try UpdateInstaller.preflight(translocated)) { error in
            XCTAssertEqual(error as? UpdateError, .translocated)
        }
    }

    func testABundleInAReadOnlyDirectoryIsRefusedBeforeAnythingIsDownloaded() throws {
        let directory = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("latch-updater-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer {
            try? FileManager.default.setAttributes(
                [.posixPermissions: 0o755], ofItemAtPath: directory.path
            )
            try? FileManager.default.removeItem(at: directory)
        }
        try FileManager.default.setAttributes(
            [.posixPermissions: 0o555], ofItemAtPath: directory.path
        )

        let bundle = directory.appendingPathComponent("Latch.app")
        XCTAssertThrowsError(try UpdateInstaller.preflight(bundle)) { error in
            XCTAssertEqual(error as? UpdateError, .notWritable(directory.path))
        }
    }

    func testAWritableInstallPassesPreflight() throws {
        let directory = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("latch-updater-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        XCTAssertNoThrow(
            try UpdateInstaller.preflight(directory.appendingPathComponent("Latch.app"))
        )
    }

    func testTheTeamIdentifierIsReadOutOfCodesignOutput() {
        let output = """
        Executable=/Applications/Latch.app/Contents/MacOS/LatchDesktop
        Identifier=co.cooperativ.latch.desktop
        Authority=Developer ID Application: Cooperativ (AB12CD34EF)
        TeamIdentifier=AB12CD34EF
        Sealed Resources version=2 rules=13 files=4
        """
        XCTAssertEqual(UpdateInstaller.teamIdentifier(inCodesignOutput: output), "AB12CD34EF")
    }

    func testAnAdHocSignatureHasNoTeamToMatchAgainst() {
        XCTAssertNil(
            UpdateInstaller.teamIdentifier(inCodesignOutput: "Identifier=x\nTeamIdentifier=not set")
        )
        XCTAssertNil(UpdateInstaller.teamIdentifier(inCodesignOutput: "Identifier=x"))
    }

    func testTheApplicationIsFoundInsideAnExpandedArchive() throws {
        let directory = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("latch-updater-\(UUID().uuidString)")
        let manager = FileManager.default
        try manager.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? manager.removeItem(at: directory) }
        try manager.createDirectory(
            at: directory.appendingPathComponent("__MACOSX"), withIntermediateDirectories: true
        )
        try manager.createDirectory(
            at: directory.appendingPathComponent("Latch.app"), withIntermediateDirectories: true
        )
        XCTAssertEqual(
            UpdateInstaller.applicationBundle(in: directory, manager: manager)?.lastPathComponent,
            "Latch.app"
        )
    }
}

@MainActor
final class UpdateControllerTests: XCTestCase {
    private struct StubFeed: ReleaseFeed {
        let document: String
        func latestRelease() async throws -> PublishedRelease {
            try JSONDecoder().decode(PublishedRelease.self, from: Data(document.utf8))
        }
    }

    private func controller(installed: String?, tag: String) -> UpdateController {
        let document = """
        {"tag_name":"\(tag)","html_url":"https://example.invalid/tag","assets":[
          {"name":"Latch-1.0.0-macos.zip","browser_download_url":"https://example.invalid/app.zip"}]}
        """
        return UpdateController(
            feed: StubFeed(document: document),
            bundleURL: URL(fileURLWithPath: "/Applications/Latch.app"),
            installedVersion: installed
        )
    }

    func testAUserInitiatedCheckReportsThatThereIsNothingNewer() async {
        let updates = controller(installed: "0.2608101202.0", tag: "v0.2608101202.0")
        await updates.check(userInitiated: true)
        XCTAssertEqual(updates.phase, .upToDate)
        XCTAssertTrue(updates.isPresented, "a check the user asked for must show its result")
        XCTAssertNil(updates.pendingUpdate)
    }

    func testABackgroundCheckSurfacesOnlyWhenSomethingIsAvailable() async {
        let quiet = controller(installed: "0.2608101202.0", tag: "v0.2608101202.0")
        await quiet.check(userInitiated: false)
        XCTAssertFalse(quiet.isPresented, "a background check must not interrupt for no reason")

        let available = controller(installed: "0.2608100841.0", tag: "v0.2608101202.0")
        await available.check(userInitiated: false)
        XCTAssertTrue(available.isPresented)
        XCTAssertEqual(available.pendingUpdate?.version, ReleaseVersion("0.2608101202.0"))
    }

    func testABuildWithNoVersionSaysSoRatherThanOfferingAnUpdate() async {
        let updates = controller(installed: nil, tag: "v0.2608101202.0")
        await updates.check(userInitiated: true)
        XCTAssertEqual(
            updates.phase,
            .failed(UpdateError.unknownInstalledVersion.localizedDescription)
        )
    }
}

import AppKit
import Foundation

enum PreferredTerminal: String, CaseIterable, Identifiable {
    case iTerm = "iTerm2"
    case terminal = "Terminal"
    case ghostty = "Ghostty"
    case custom = "Custom"

    var id: String { rawValue }
}

@MainActor
enum TerminalLauncher {
    static func executablePath(forApplicationURL url: URL) throws -> String {
        guard url.pathExtension == "app",
              let executableURL = Bundle(url: url)?.executableURL else {
            throw TerminalLaunchError.invalidApplication(url.path)
        }
        return executableURL.path
    }

    static func open(
        command: [String],
        in terminal: PreferredTerminal,
        customExecutable: String = "",
        customTemplate: String = ""
    ) throws {
        guard command.count == 3 else { throw TerminalLaunchError.invalidCommand }
        switch terminal {
        case .iTerm:
            try runAppleScript(application: "iTerm", statement: "create window with default profile command")
        case .terminal:
            try runAppleScript(application: "Terminal", statement: "do script")
        case .ghostty:
            guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.mitchellh.ghostty") else {
                throw TerminalLaunchError.notInstalled(terminal.rawValue)
            }
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.activates = true
            configuration.arguments = ["-e"] + command
            NSWorkspace.shared.openApplication(at: url, configuration: configuration) { _, error in
                if let error {
                    NSApp.presentError(error)
                }
            }
        case .custom:
            guard FileManager.default.isExecutableFile(atPath: customExecutable) else {
                throw TerminalLaunchError.notExecutable(customExecutable)
            }
            guard customTemplate.contains("{latch}"), customTemplate.contains("{session}") else {
                throw TerminalLaunchError.missingPlaceholders
            }
            let arguments = try parseArguments(customTemplate).map {
                $0.replacingOccurrences(of: "{latch}", with: command[0])
                    .replacingOccurrences(of: "{session}", with: command[2])
            }
            let process = Process()
            process.executableURL = URL(fileURLWithPath: customExecutable)
            process.arguments = arguments
            try process.run()
        }

        func runAppleScript(application: String, statement: String) throws {
            let shellCommand = command.map(shellQuote).joined(separator: " ")
            let source = """
            tell application "\(application)"
                activate
                \(statement) "\(appleScriptEscape(shellCommand))"
            end tell
            """
            var error: NSDictionary?
            guard NSAppleScript(source: source)?.executeAndReturnError(&error) != nil else {
                throw TerminalLaunchError.appleEvent(
                    error?[NSAppleScript.errorMessage] as? String ?? "macOS denied the terminal launch"
                )
            }
        }
    }

    static func isInstalled(_ terminal: PreferredTerminal) -> Bool {
        switch terminal {
        case .iTerm:
            return NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.googlecode.iterm2") != nil
        case .terminal:
            return NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal") != nil
        case .ghostty:
            return NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.mitchellh.ghostty") != nil
        case .custom:
            return true
        }
    }

    static func shellQuote(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "'\"'\"'") + "'"
    }

    static func appleScriptEscape(_ value: String) -> String {
        value
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
            .replacingOccurrences(of: "\n", with: "\\n")
    }

    /// Splits a user-authored argument template without invoking a shell.
    static func parseArguments(_ template: String) throws -> [String] {
        enum Quote { case none, single, double }
        var quote = Quote.none
        var escaping = false
        var current = ""
        var arguments: [String] = []
        var tokenStarted = false

        for character in template {
            if escaping {
                current.append(character)
                tokenStarted = true
                escaping = false
                continue
            }
            switch (quote, character) {
            case (.none, "\\"), (.double, "\\"):
                escaping = true
                tokenStarted = true
            case (.none, "'"):
                quote = .single
                tokenStarted = true
            case (.single, "'"):
                quote = .none
            case (.none, "\""):
                quote = .double
                tokenStarted = true
            case (.double, "\""):
                quote = .none
            case (.none, let value) where value.isWhitespace:
                if tokenStarted {
                    arguments.append(current)
                    current = ""
                    tokenStarted = false
                }
            default:
                current.append(character)
                tokenStarted = true
            }
        }
        guard quote == .none, !escaping else { throw TerminalLaunchError.malformedTemplate }
        if tokenStarted { arguments.append(current) }
        return arguments
    }
}

enum TerminalLaunchError: LocalizedError, Equatable {
    case invalidCommand
    case notInstalled(String)
    case appleEvent(String)
    case notExecutable(String)
    case missingPlaceholders
    case malformedTemplate
    case invalidApplication(String)

    var errorDescription: String? {
        switch self {
        case .invalidCommand: return "The attachment command was invalid."
        case .notInstalled(let name): return "\(name) is not installed."
        case .appleEvent(let detail): return "The terminal could not be opened: \(detail)"
        case .notExecutable(let path): return "The custom terminal executable is not valid: \(path)"
        case .missingPlaceholders: return "The custom argument template must include {latch} and {session}."
        case .malformedTemplate: return "The custom argument template has an unmatched quote or escape."
        case .invalidApplication(let path): return "The selected application at \(path) does not contain a launchable executable."
        }
    }
}

import AppKit
import Foundation

enum PreferredTerminal: String, CaseIterable, Identifiable {
    case iTerm = "iTerm2"
    case terminal = "Terminal"
    case ghostty = "Ghostty"
    case custom = "Custom"

    var id: String { rawValue }

    /// The launch shapes this terminal can actually be driven into from another app.
    var supportedOpenBehaviors: [TerminalOpenBehavior] {
        switch self {
        case .iTerm, .terminal: return TerminalOpenBehavior.allCases
        case .ghostty: return [.newWindow]
        // A custom terminal is launched entirely by the user's argument template,
        // so Latch has nothing left to decide.
        case .custom: return []
        }
    }

    func supports(_ behavior: TerminalOpenBehavior) -> Bool {
        supportedOpenBehaviors.contains(behavior)
    }

    /// Why an option is unavailable, phrased for the split-button menu.
    func unsupportedReason(for behavior: TerminalOpenBehavior) -> String? {
        guard !supports(behavior) else { return nil }
        switch self {
        case .custom: return "set by your argument template"
        default: return "not supported by \(rawValue)"
        }
    }

    /// The behavior actually used when the requested one is unavailable.
    func resolvedOpenBehavior(_ behavior: TerminalOpenBehavior) -> TerminalOpenBehavior {
        supports(behavior) ? behavior : .newWindow
    }
}

/// How Latch asks the preferred terminal to present a session attachment.
enum TerminalOpenBehavior: String, CaseIterable, Identifiable {
    case newWindow
    case newTab

    var id: String { rawValue }

    var label: String {
        switch self {
        case .newWindow: return "New Window"
        case .newTab: return "New Tab"
        }
    }

    var systemImage: String {
        switch self {
        case .newWindow: return "macwindow.badge.plus"
        case .newTab: return "plus.rectangle.on.rectangle"
        }
    }
}

@MainActor
enum TerminalLauncher {
    static func runInTerminal(_ shellCommand: String) throws {
        try runAppleScript(
            application: "Terminal",
            statement: "do script",
            shellCommand: shellCommand
        )
    }

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
        behavior: TerminalOpenBehavior = .newWindow,
        customExecutable: String = "",
        customTemplate: String = ""
    ) throws {
        guard command.count == 3 else { throw TerminalLaunchError.invalidCommand }
        let shellCommand = command.map(shellQuote).joined(separator: " ")
        // Falls back to a new window rather than refusing when the terminal cannot
        // honour the requested shape; attaching the session matters more.
        switch (terminal, terminal.resolvedOpenBehavior(behavior)) {
        case (.iTerm, .newWindow):
            try runAppleScript(
                application: "iTerm",
                statement: "create window with default profile command",
                shellCommand: shellCommand
            )
        case (.iTerm, .newTab):
            try execute("""
            tell application "iTerm"
                activate
                if (count of windows) is 0 then
                    create window with default profile command "\(appleScriptEscape(shellCommand))"
                else
                    tell current window to create tab with default profile command "\(appleScriptEscape(shellCommand))"
                end if
            end tell
            """)
        case (.terminal, .newWindow):
            try runAppleScript(
                application: "Terminal",
                statement: "do script",
                shellCommand: shellCommand
            )
        case (.terminal, .newTab):
            try openTerminalAppTab(shellCommand: shellCommand)
        case (.ghostty, _):
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
        case (.custom, _):
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

    }

    /// Terminal.app's scripting dictionary cannot create a tab, so the tab comes from
    /// the Command-T keystroke and `do script` then runs in the tab that selects.
    private static func openTerminalAppTab(shellCommand: String) throws {
        guard hasOpenTerminalAppWindow() else {
            try runAppleScript(application: "Terminal", statement: "do script", shellCommand: shellCommand)
            return
        }
        do {
            try execute("""
            tell application "Terminal" to activate
            tell application "System Events" to tell process "Terminal"
                keystroke "t" using command down
            end tell
            delay 0.2
            tell application "Terminal"
                do script "\(appleScriptEscape(shellCommand))" in front window
            end tell
            """)
        } catch {
            // Sending keystrokes needs a separate Automation grant for System Events;
            // a new window still attaches the session when that is refused.
            try runAppleScript(application: "Terminal", statement: "do script", shellCommand: shellCommand)
        }
    }

    /// Whether there is a window for a new tab to join.
    ///
    /// The running check comes first so that a launch is never provoked here: a
    /// Terminal that has to be started opens its own window, and adding a tab to
    /// that would leave the launch window sitting there unused.
    private static func hasOpenTerminalAppWindow() -> Bool {
        guard NSWorkspace.shared.runningApplications.contains(
            where: { $0.bundleIdentifier == "com.apple.Terminal" }
        ) else { return false }
        let source = """
        tell application "Terminal" to return (count of windows) > 0
        """
        var error: NSDictionary?
        guard let descriptor = NSAppleScript(source: source)?.executeAndReturnError(&error) else {
            return false
        }
        return descriptor.booleanValue
    }

    private static func runAppleScript(
        application: String,
        statement: String,
        shellCommand: String
    ) throws {
        try execute("""
        tell application "\(application)"
            activate
            \(statement) "\(appleScriptEscape(shellCommand))"
        end tell
        """)
    }

    private static func execute(_ source: String) throws {
        var error: NSDictionary?
        guard NSAppleScript(source: source)?.executeAndReturnError(&error) != nil else {
            throw TerminalLaunchError.appleEvent(
                error?[NSAppleScript.errorMessage] as? String ?? "macOS denied the terminal launch"
            )
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

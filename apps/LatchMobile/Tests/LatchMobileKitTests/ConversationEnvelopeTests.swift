import XCTest

@testable import LatchMobileKit

/// Harness markup filed as user turns is recognised and described; prose,
/// including prose that happens to contain tags, is left alone. Payloads are
/// copied from real Claude transcripts.
final class ConversationEnvelopeTests: XCTestCase {
    func testAFailedAgentNotificationNamesTheAgentAndTheFailure() throws {
        let text = """
        <task-notification>
        <task-id>a753f1967a1f2b6d3</task-id>
        <tool-use-id>toolu_018ig2PzS7YeFtNz8K1zGKKA</tool-use-id>
        <output-file>/private/tmp/claude-501/tasks/a753f1967a1f2b6d3.output</output-file>
        <status>failed</status>
        <summary>Agent "P6b fork C: shifts/orders/ranges" failed: Agent terminated early due to an API error: You've hit your session limit · resets 4pm (Europe/Berlin) (error type rate_limit, HTTP 429)</summary>
        <note>A task-notification fires each time this agent stops with no live background children of its own.</note>
        <result>Fork C: shifts/orders/stats range pickers, hydration, lead chip, routes (items 1-11).</result>
        </task-notification>
        """
        let envelope = try XCTUnwrap(ConversationEnvelope.parse(text))

        XCTAssertEqual(envelope.kind, .backgroundTask(.agent))
        XCTAssertEqual(envelope.eyebrow, "Agent")
        XCTAssertEqual(envelope.title, "P6b fork C: shifts/orders/ranges")
        XCTAssertEqual(envelope.status, "Failed")
        XCTAssertEqual(envelope.tone, .failure)
        XCTAssertEqual(envelope.headline?.hasPrefix("Agent terminated early"), true)
        XCTAssertEqual(envelope.body, "Fork C: shifts/orders/stats range pickers, hydration, lead chip, routes (items 1-11).")
        XCTAssertEqual(envelope.origin, .agent)
        XCTAssertEqual(envelope.details.map(\.label), ["Note", "Task", "Tool call", "Output file"])
        XCTAssertEqual(envelope.details.first { $0.label == "Task" }?.value, "a753f1967a1f2b6d3")
    }

    func testAStoppedBackgroundCommandCarriesItsGuidance() throws {
        let text = """
        <task-notification>
        <task-id>br0hdlnq7</task-id>
        <tool-use-id>toolu_014egiosqFjAoR8XyG5VMqaN</tool-use-id>
        <output-file>/private/tmp/tasks/br0hdlnq7.output</output-file>
        <status>killed</status>
        <summary>Background command "Screenshot previews at desktop and phone widths" was stopped after reaching its background time limit</summary>
        </task-notification>
        If the work in progress still needs it, start it again.
        """
        let envelope = try XCTUnwrap(ConversationEnvelope.parse(text))

        XCTAssertEqual(envelope.kind, .backgroundTask(.command))
        XCTAssertEqual(envelope.title, "Screenshot previews at desktop and phone widths")
        XCTAssertEqual(envelope.status, "Stopped")
        XCTAssertEqual(envelope.tone, .attention)
        XCTAssertEqual(envelope.headline, "After reaching its background time limit")
        XCTAssertNil(envelope.body)
    }

    func testACompletedAgentWithUsageAndQuotesInItsName() throws {
        let text = """
        <task-notification>
        <task-id>a1</task-id>
        <summary>Agent "Fix "quoted" parser" finished</summary>
        <result>Done.

        - item one
        - item two</result>
        <usage><subagent_tokens>51234</subagent_tokens><tool_uses>17</tool_uses><duration_ms>125000</duration_ms></usage>
        <worktree><worktreePath>/tmp/wt</worktreePath><worktreeBranch>fix/parser</worktreeBranch></worktree>
        </task-notification>
        """
        let envelope = try XCTUnwrap(ConversationEnvelope.parse(text))

        XCTAssertEqual(envelope.title, "Fix \"quoted\" parser")
        XCTAssertEqual(envelope.status, "Completed")
        XCTAssertEqual(envelope.tone, .success)
        XCTAssertNil(envelope.headline)
        XCTAssertEqual(envelope.body?.hasSuffix("- item two"), true)
        let details = Dictionary(uniqueKeysWithValues: envelope.details.map { ($0.label, $0.value) })
        XCTAssertEqual(details["Tokens"], "51234")
        XCTAssertEqual(details["Tool uses"], "17")
        XCTAssertEqual(details["Duration"], "2m 5s")
        XCTAssertEqual(details["Branch"], "fix/parser")
    }

    func testTheHeadlineDropsTheVerbTheBadgeSays() {
        XCTAssertEqual(ConversationEnvelope.headline("completed (exit code 0)", status: "Completed"), "Exit code 0")
        XCTAssertEqual(ConversationEnvelope.headline("is still running after 5m", status: "Still running"), "Is still running after 5m")
        XCTAssertNil(ConversationEnvelope.headline("finished", status: "Completed"))
    }

    func testAMonitorEventShowsTheEvent() throws {
        let text = """
        <task-notification>
        <task-id>w1</task-id>
        <summary>Monitor event: "deploy log"</summary>
        <event>Build finished in 42s</event>
        </task-notification>
        """
        let envelope = try XCTUnwrap(ConversationEnvelope.parse(text))

        XCTAssertEqual(envelope.kind, .backgroundTask(.monitor))
        XCTAssertEqual(envelope.title, "deploy log")
        XCTAssertEqual(envelope.body, "Build finished in 42s")
    }

    func testPeerMessagesNameTheirSender() throws {
        let teammate = try XCTUnwrap(ConversationEnvelope.parse(
            "<teammate-message teammate_id=\"researcher\" summary=\"Found it\">The bug is in parse().</teammate-message>"
        ))
        XCTAssertEqual(teammate.kind, .peerMessage)
        XCTAssertEqual(teammate.title, "researcher")
        XCTAssertEqual(teammate.headline, "Found it")
        XCTAssertEqual(teammate.body, "The bug is in parse().")

        let session = try XCTUnwrap(ConversationEnvelope.parse(
            "<cross-session-message from=\"abc\" from-name=\"Latch build\">\nShip it\n</cross-session-message>"
        ))
        XCTAssertEqual(session.eyebrow, "Session message")
        XCTAssertEqual(session.title, "Latch build")
        XCTAssertEqual(session.body, "Ship it")

        let channel = try XCTUnwrap(ConversationEnvelope.parse("<channel source=\"slack\" user=\"jo\">\nhello\n</channel>"))
        XCTAssertEqual(channel.title, "slack")
        XCTAssertEqual(channel.details.map(\.label), ["User"])
    }

    func testSeveralTeammateMessagesAreShownTogether() throws {
        let envelope = try XCTUnwrap(ConversationEnvelope.parse(
            "<teammate-message teammate_id=\"a\">one</teammate-message>\n<teammate-message teammate_id=\"b\">two</teammate-message>"
        ))
        XCTAssertEqual(envelope.eyebrow, "2 teammate messages")
        XCTAssertEqual(envelope.body, "one\n\ntwo")
    }

    func testShellAndSlashCommandsBelongToThePerson() throws {
        let shell = try XCTUnwrap(ConversationEnvelope.parse(
            "<bash-input>ls -la</bash-input><bash-stdout>total 0</bash-stdout><bash-stderr></bash-stderr>"
        ))
        XCTAssertEqual(shell.kind, .shellCommand)
        XCTAssertEqual(shell.origin, .person)
        XCTAssertEqual(shell.title, "$ ls -la")
        XCTAssertEqual(shell.body, "total 0")
        XCTAssertTrue(shell.bodyIsCode)

        let slash = try XCTUnwrap(ConversationEnvelope.parse("""
        <command-name>/model</command-name>
                    <command-message>model</command-message>
                    <command-args>claude-fable-5-1</command-args>
        """))
        XCTAssertEqual(slash.kind, .slashCommand)
        XCTAssertEqual(slash.title, "/model claude-fable-5-1")
        XCTAssertEqual(slash.origin, .person)

        let reordered = try XCTUnwrap(ConversationEnvelope.parse("<command-message>run</command-message>\n<command-name>/run</command-name>"))
        XCTAssertEqual(reordered.title, "/run")

        let output = try XCTUnwrap(ConversationEnvelope.parse("<local-command-stdout>Set model to `claude-fable-5-1`</local-command-stdout>"))
        XCTAssertEqual(output.kind, .commandOutput)
        XCTAssertEqual(output.title, "Set model to `claude-fable-5-1`")
        XCTAssertNil(output.body)
    }

    func testKnownAndWholeMessageNotesAreRecognised() throws {
        let hook = try XCTUnwrap(ConversationEnvelope.parse("<user-prompt-submit-hook>blocked</user-prompt-submit-hook>"))
        XCTAssertEqual(hook.kind, .harnessNote)
        XCTAssertEqual(hook.title, "Prompt hook output")

        let custom = try XCTUnwrap(ConversationEnvelope.parse("<create-pr-command>\n## Overview\nCreate a PR.\n</create-pr-command>"))
        XCTAssertEqual(custom.title, "Create Pr Command")
        XCTAssertEqual(custom.body, "## Overview\nCreate a PR.")

        let tick = try XCTUnwrap(ConversationEnvelope.parse("<tick>2026-10-12T14:00:00Z</tick>"))
        XCTAssertEqual(tick.kind, .scheduledWake)
    }

    func testProseStaysProse() {
        XCTAssertNil(ConversationEnvelope.parse("Please fix the bug"))
        XCTAssertNil(ConversationEnvelope.parse("<div>hello</div>"))
        XCTAssertNil(ConversationEnvelope.parse("<my-tag>wrapped</my-tag> and then more prose"))
        XCTAssertNil(ConversationEnvelope.parse("<Task-Notification>x</Task-Notification>"))
        XCTAssertNil(ConversationEnvelope.parse("<task-notification> never closed"))
        XCTAssertNil(ConversationEnvelope.parse("why does <task-notification> show raw?"))
    }

    func testOnlyUserMessagesCarryAnEnvelope() {
        let text = "<local-command-stdout>ok</local-command-stdout>"
        let user = ConversationMessagePresentation(id: "u", ordinal: 1, role: .user, text: text, status: .observed)
        let assistant = ConversationMessagePresentation(id: "a", ordinal: 2, role: .assistant, text: text, status: .complete)
        XCTAssertNotNil(user.envelope)
        XCTAssertNil(assistant.envelope)
    }

    // MARK: Tool catalog

    func testKnownToolsGetATitleAndSymbol() {
        let edit = ConversationToolCatalog.descriptor(for: "MultiEdit")
        XCTAssertEqual(edit.title, "Edit")
        XCTAssertEqual(edit.symbolName, "pencil")
        XCTAssertEqual(ConversationToolCatalog.descriptor(for: "Bash").title, "Bash")
        XCTAssertEqual(ConversationToolCatalog.descriptor(for: "ScheduleWakeup").title, "Schedule wake-up")
    }

    func testMcpToolsAreTitledByToolAndAttributedToTheirServer() {
        let tool = ConversationToolCatalog.descriptor(for: "mcp__claude-in-chrome__read_page")
        XCTAssertEqual(tool.title, "Read page")
        XCTAssertEqual(tool.provider, "claude-in-chrome")

        let hosted = ConversationToolCatalog.descriptor(for: "mcp__claude_ai_Overlord__overlord_add_artifact")
        XCTAssertEqual(hosted.provider, "Overlord")
        XCTAssertEqual(hosted.title, "Overlord add artifact")
    }

    func testUnknownToolsKeepTheirName() {
        let unknown = ConversationToolCatalog.descriptor(for: "FrobnicateV2")
        XCTAssertEqual(unknown.title, "FrobnicateV2")
        XCTAssertNil(unknown.provider)
        XCTAssertEqual(ConversationToolCatalog.descriptor(for: "mcp__").title, "mcp__")
    }
}

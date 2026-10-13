import Foundation

/// How a tool call is named and drawn. Tool names are free text on the wire;
/// this maps the ones agents are known to use to a readable title and an SF
/// Symbol, and gives every other name an honest generic presentation.
///
/// MCP tools arrive as `mcp__<server>__<tool>`; they are titled by the tool
/// and attributed to the server rather than shown as one long identifier.
public struct ConversationToolDescriptor: Equatable, Sendable {
    public let title: String
    public let symbolName: String
    /// The MCP server or other provider the tool belongs to, when known.
    public let provider: String?

    public init(title: String, symbolName: String, provider: String? = nil) {
        self.title = title
        self.symbolName = symbolName
        self.provider = provider
    }
}

public enum ConversationToolCatalog {
    public static func descriptor(for name: String) -> ConversationToolDescriptor {
        if let known = known[name] {
            return ConversationToolDescriptor(title: known.title, symbolName: known.symbol)
        }
        if let mcp = mcpTool(name) { return mcp }
        return ConversationToolDescriptor(title: name, symbolName: "wrench.and.screwdriver")
    }

    private static func mcpTool(_ name: String) -> ConversationToolDescriptor? {
        guard name.hasPrefix("mcp__") else { return nil }
        let parts = name.dropFirst("mcp__".count).components(separatedBy: "__")
        guard parts.count >= 2, let server = parts.first, !server.isEmpty else { return nil }
        let tool = parts.dropFirst().joined(separator: "__")
        let title = tool.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ")
        // Hosted connectors arrive as `<host>_ai_<Name>` and plugin servers
        // as `plugin_<plugin>_<name>`; the reader wants the name.
        var provider = Substring(server)
        if let hosted = provider.range(of: "_ai_") { provider = provider[hosted.upperBound...] }
        if provider.hasPrefix("plugin_") { provider = provider.dropFirst("plugin_".count) }
        let providerName = provider.replacingOccurrences(of: "_", with: " ")
        return ConversationToolDescriptor(
            title: title.isEmpty ? name : title.prefix(1).uppercased() + title.dropFirst(),
            symbolName: "puzzlepiece.extension",
            provider: providerName.isEmpty ? server : providerName
        )
    }

    /// Tool names agents are known to call: the built-in file, shell, agent,
    /// planning, scheduling, web, and artifact tools, plus the snake_case
    /// shell and patch tools other agents use. An unknown name is never an
    /// error.
    static let known: [String: (title: String, symbol: String)] = [
        // Files
        "Read": ("Read", "doc.text"),
        "Write": ("Write", "square.and.pencil"),
        "Edit": ("Edit", "pencil"),
        "MultiEdit": ("Edit", "pencil"),
        "NotebookEdit": ("Edit notebook", "book"),
        "NotebookRead": ("Read notebook", "book"),
        "Glob": ("Find files", "folder.badge.questionmark"),
        "Grep": ("Search", "magnifyingglass"),
        "LS": ("List files", "folder"),
        "LSP": ("Code intelligence", "chevron.left.forwardslash.chevron.right"),
        // Shell and background work
        "Bash": ("Bash", "terminal"),
        "BashOutput": ("Command output", "text.alignleft"),
        "KillBash": ("Stop command", "stop.circle"),
        "KillShell": ("Stop command", "stop.circle"),
        "TaskOutput": ("Task output", "text.alignleft"),
        "TaskStop": ("Stop task", "stop.circle"),
        "Monitor": ("Monitor", "waveform.path.ecg"),
        "PowerShell": ("PowerShell", "terminal"),
        // Agents and coordination
        "Agent": ("Agent", "person.2.circle"),
        "Task": ("Agent", "person.2.circle"),
        "SendMessage": ("Send message", "paperplane"),
        "ListAgents": ("List agents", "person.3"),
        "TeamCreate": ("Create team", "person.3"),
        "TeamDelete": ("Delete team", "person.3"),
        "Workflow": ("Workflow", "flowchart"),
        "RemoteTrigger": ("Remote trigger", "icloud"),
        // Planning and asking
        "TodoWrite": ("Update to-dos", "checklist"),
        "TodoRead": ("Read to-dos", "checklist"),
        "TaskCreate": ("Add task", "checklist"),
        "TaskUpdate": ("Update task", "checklist"),
        "TaskList": ("List tasks", "checklist"),
        "TaskGet": ("Read task", "checklist"),
        "AskUserQuestion": ("Question", "questionmark.bubble"),
        "EnterPlanMode": ("Enter plan mode", "list.bullet.clipboard"),
        "ExitPlanMode": ("Plan ready", "list.bullet.clipboard"),
        "EnterWorktree": ("Enter worktree", "arrow.triangle.branch"),
        "ExitWorktree": ("Leave worktree", "arrow.triangle.branch"),
        // Scheduling
        "ScheduleWakeup": ("Schedule wake-up", "alarm"),
        "CronCreate": ("Schedule job", "calendar.badge.clock"),
        "CronDelete": ("Remove job", "calendar.badge.minus"),
        "CronList": ("List jobs", "calendar"),
        "PushNotification": ("Notify", "bell"),
        // Web
        "WebFetch": ("Fetch page", "globe"),
        "WebSearch": ("Web search", "magnifyingglass.circle"),
        // Skills, tools, and MCP plumbing
        "Skill": ("Skill", "sparkles"),
        "SlashCommand": ("Command", "slash.circle"),
        "ToolSearch": ("Load tools", "shippingbox"),
        "ListMcpResourcesTool": ("List MCP resources", "puzzlepiece.extension"),
        "ReadMcpResourceTool": ("Read MCP resource", "puzzlepiece.extension"),
        "ReadMcpResourceDirTool": ("Read MCP resources", "puzzlepiece.extension"),
        // Artifacts and reporting
        "Artifact": ("Artifact", "doc.richtext"),
        "ArtifactComments": ("Artifact comments", "text.bubble"),
        "ArtifactData": ("Artifact data", "tablecells"),
        "DesignSync": ("Design sync", "paintpalette"),
        "ReportFindings": ("Report findings", "exclamationmark.magnifyingglass"),
        "SendFeedback": ("Draft feedback", "envelope"),
        "StructuredOutput": ("Structured output", "curlybraces"),
        "EndConversation": ("End conversation", "xmark.octagon"),
        // snake_case shell and patch tools
        "shell": ("Shell", "terminal"),
        "Shell": ("Shell", "terminal"),
        "local_shell": ("Shell", "terminal"),
        "exec_command": ("Shell", "terminal"),
        "write_stdin": ("Shell input", "keyboard"),
        "apply_patch": ("Apply patch", "pencil"),
        "update_plan": ("Update plan", "checklist"),
        "view_image": ("View image", "photo"),
        "web_search": ("Web search", "magnifyingglass.circle"),
    ]
}

extension ConversationToolPresentation {
    public var descriptor: ConversationToolDescriptor { ConversationToolCatalog.descriptor(for: name) }
}

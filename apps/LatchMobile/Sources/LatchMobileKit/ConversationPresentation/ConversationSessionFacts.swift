import Foundation

/// One quiet line about the agent session: its model, effort, permission
/// mode, how full its context is, a rate limit that is close, and its cost.
/// Every part is optional; with none known there is no line.
public enum ConversationSessionFacts {
    public static func line(_ state: ConversationState?) -> String? {
        guard let state else { return nil }
        var parts: [String] = []
        if let model = state.model, !model.isEmpty { parts.append(modelName(model)) }
        if let effort = state.effort, !effort.isEmpty { parts.append("\(effort) effort") }
        if let mode = state.permissionMode.flatMap(modeName) { parts.append(mode) }
        if let usage = state.usage {
            if let percent = usage.contextPercent {
                parts.append("\(percent)% context")
            } else if let tokens = usage.contextTokens, let window = usage.contextWindow, window > 0 {
                parts.append("\(tokens * 100 / window)% context")
            }
            // Only a limit worth noticing earns room on the line.
            if let limit = usage.rateLimits.max(by: { $0.percentUsed < $1.percentUsed }), limit.percentUsed >= 50 {
                parts.append("\(limitName(limit.kind)) limit \(limit.percentUsed)%")
            }
            if let cents = usage.costCents, cents > 0 {
                parts.append(String(format: "$%.2f", Double(cents) / 100))
            }
        }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// `claude-haiku-5-5` reads as "Haiku 5.5"; anything else as given.
    static func modelName(_ model: String) -> String {
        let pieces = model.split(separator: "-").map(String.init)
        guard pieces.count >= 3, pieces[0] == "claude", let family = pieces.dropFirst().first,
              family.allSatisfy(\.isLetter) else { return model }
        let version = pieces.dropFirst(2).prefix { $0.count <= 2 && $0.allSatisfy(\.isNumber) }
        guard !version.isEmpty else { return model }
        return "\(family.prefix(1).uppercased())\(family.dropFirst()) \(version.joined(separator: "."))"
    }

    /// The default mode says nothing; the others are worth a glance.
    static func modeName(_ mode: String) -> String? {
        switch mode {
        case "", "default": nil
        case "plan": "Plan mode"
        case "acceptEdits": "Accepting edits"
        case "bypassPermissions": "Bypassing permissions"
        case "dontAsk": "Not asking"
        case "auto": "Auto mode"
        default: mode
        }
    }

    static func limitName(_ kind: String) -> String {
        switch kind {
        case "five_hour": "5-hour"
        case "seven_day": "7-day"
        default: kind.replacingOccurrences(of: "_", with: " ")
        }
    }
}

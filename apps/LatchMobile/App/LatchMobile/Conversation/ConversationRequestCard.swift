import LatchMobileKit
import SwiftUI

/// A request as it sits in the transcript.
///
/// While it is the one the host waits on, the row is only a marker: the full
/// prompt and its choices are in the controls that replace the composer, so
/// the text is not shown twice. Once settled, the row keeps the prompt, how it
/// settled, and what became of an answer this phone gave that did not apply.
struct ConversationRequestCard: View, Equatable {
    let request: ConversationRequestPresentation

    var body: some View {
        if request.isAwaitingAnswer {
            HStack(spacing: 6) {
                Image(systemName: request.symbol)
                Text(request.title)
                Text(request.metadata?.answerable == false ? "· Answer at terminal" : "· Answer below").foregroundStyle(.secondary)
                Spacer(minLength: 0)
            }
            .font(.caption.weight(.medium))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(.yellow.opacity(0.12), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("conversation.request.marker")
        } else {
            VStack(alignment: .leading, spacing: 4) {
                Label(request.title, systemImage: request.symbol)
                    .font(.caption.weight(.medium))
                Text(request.prompt)
                    .font(.callout)
                    .textSelection(.enabled)
                ConversationRequestDetails(request: request)
                if let outcome = request.resolutionCaption {
                    Text(outcome)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                if let answer = request.answer, answer.explainsSettledRequest {
                    ConversationAnswerStatus(answer: answer)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.yellow.opacity(0.08), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .accessibilityIdentifier("conversation.request.card")
        }
    }
}

/// The input surface while the host waits on a request. It offers exactly the
/// choices the host sent, or structured questions with free text, and every
/// answer targets the request's exact `requestId`. An answer on its way or
/// applied is not offered again; any other outcome needs a new explicit tap.
struct ConversationRequestControls: View {
    let request: ConversationRequestPresentation
    let canResolve: Bool
    let reason: String?
    let resolve: (_ requestID: String, _ choice: String) -> Void

    var resolveQuestions: (_ requestID: String, _ answers: [String: String]) -> Void = { _, _ in }
    var resolveOption: (String, String, String?, [String: ConversationJSONValue]?) -> Void = { _, _, _, _ in }

    @Environment(\.openURL) private var openURL
    @State private var feedback = ""
    @State private var formValues: [String: ConversationJSONValue] = [:]

    @State private var draft = ConversationQuestionDraft()
    @State private var showsFullPrompt = false

    var body: some View {
        ViewThatFits(in: .vertical) {
            contents
            ScrollView { contents }
        }
        .frame(maxHeight: 390)
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.thinMaterial)
        .accessibilityIdentifier("conversation.request.controls")
    }

    private var contents: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(request.title, systemImage: request.symbol)
                .font(.caption.weight(.medium))
                .foregroundStyle(.secondary)
            if request.questions.isEmpty {
                Text(request.prompt)
                    .font(.callout)
                    .lineLimit(showsFullPrompt ? nil : 8)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("conversation.request.prompt")
                if isLongPrompt {
                    Button(showsFullPrompt ? "Show less" : "Show full request") { showsFullPrompt.toggle() }
                        .font(.caption)
                        .accessibilityIdentifier("conversation.request.expand")
                }
            }
            if request.semanticKind != nil {
                ConversationRequestDetails(request: request, includesLink: false)
            }
            if let answer = request.answer {
                ConversationAnswerStatus(answer: answer)
            }
            if request.answer?.allowsAnotherAnswer ?? true {
                if request.questions.isEmpty {
                    if request.metadata?.answerable != false {
                        choices
                    }
                    if let url = request.actionURL {
                        Button(request.options.first { $0.id == "open_url" }?.label ?? "Open link") {
                            openURL(url) { accepted in
                                if accepted && canResolve && request.options.contains(where: { $0.id == "open_url" }) {
                                    resolveOption(request.requestId, "open_url", nil, nil)
                                }
                            }
                        }
                        .buttonStyle(.bordered)
                        Text(url.host ?? "").font(.caption).foregroundStyle(.secondary)
                    }
                } else {
                    questionForm
                }
                if !canResolve, let reason = request.metadata?.reason ?? reason {
                    Label(reason, systemImage: "info.circle").font(.caption).foregroundStyle(.secondary)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var answers: [String: String] { draft.answers(for: request.questions) }

    private var questionForm: some View {
        VStack(alignment: .leading, spacing: 12) {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(request.questions, id: \.question) { question in
                        VStack(alignment: .leading, spacing: 6) {
                            if let header = question.header { Text(header).font(.caption.weight(.semibold)) }
                            Text(question.question).font(.callout)
                            if question.multiSelect { Text("Select all that apply").font(.caption).foregroundStyle(.secondary) }
                            ForEach(question.options, id: \.label) { option in
                                Button {
                                    draft.toggle(option.label, for: question)
                                } label: {
                                    HStack(alignment: .top) {
                                        Image(systemName: draft.selected[question.question, default: []].contains(option.label) ? "checkmark.circle.fill" : "circle")
                                        VStack(alignment: .leading) {
                                            Text(option.label)
                                            if !option.description.isEmpty { Text(option.description).font(.caption).foregroundStyle(.secondary) }
                                        }
                                        Spacer(minLength: 0)
                                    }
                                    .frame(minHeight: 44)
                                }
                                .buttonStyle(.bordered)
                                .disabled(!canResolve)
                                .accessibilityAddTraits(draft.selected[question.question, default: []].contains(option.label) ? .isSelected : [])
                            }
                            TextField("Other answer", text: Binding(
                                get: { draft.text[question.question, default: ""] },
                                set: { value in
                                    draft.setText(value, for: question)
                                }
                            ), axis: .vertical)
                            .lineLimit(1...3)
                            .textFieldStyle(.roundedBorder)
                            .disabled(!canResolve)
                            .accessibilityLabel("Other answer for \(question.question)")
                        }
                    }
                }
            }
            .frame(maxHeight: 320)
            Button("Submit answers") { resolveQuestions(request.requestId, answers) }
                .buttonStyle(.borderedProminent)
                .disabled(!canResolve || answers.count != request.questions.count || answers.values.contains(where: { $0.isEmpty || $0.utf8.count > 4096 }))
                .accessibilityIdentifier("conversation.request.submit")
        }
    }

    @ViewBuilder
    private var choices: some View {
        let options = request.options.filter { $0.id != "open_url" }
        let legacy = request.choices.filter { choice in
            !request.options.contains { $0.label.caseInsensitiveCompare(choice) == .orderedSame }
        }
        let form = request.semanticKind == .elicitationForm
            ? RequestForm(request.metadata?.details?["schema"]?.object) : nil

        if request.semanticKind == .elicitationForm {
            if let form {
                formFields(form)
            } else {
                Text("Complete this form at the terminal.").font(.caption).foregroundStyle(.secondary)
            }
        }
        if options.contains(where: { $0.requiresText == true }) {
            TextField("Feedback", text: $feedback, axis: .vertical)
                .lineLimit(1...4)
                .textFieldStyle(.roundedBorder)
                .disabled(!canResolve)
        }
        ForEach(options) { option in
            let text = feedback.trimmingCharacters(in: .whitespacesAndNewlines)
            Button(option.label) {
                resolveOption(request.requestId, option.id, option.requiresText == true ? text : nil,
                              option.id == "submit_form" ? form?.submission(formValues) : nil)
            }
            .buttonStyle(.bordered)
            .disabled(!canResolve || (option.requiresText == true && (text.isEmpty || text.utf8.count > 4096))
                      || (option.id == "submit_form" && form?.submission(formValues) == nil))
            .accessibilityIdentifier("conversation.request.option." + option.id)
        }
        ForEach(Array(legacy.enumerated()), id: \.offset) { _, choice in
            Button {
                resolve(request.requestId, choice)
            } label: {
                Text(choice).frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.bordered)
            .disabled(!canResolve)
            .accessibilityIdentifier("conversation.request.choice")
        }
        if options.isEmpty && legacy.isEmpty && request.actionURL == nil && request.semanticKind != .elicitationForm {
            Text("This request can only be answered at the terminal.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private func formFields(_ form: RequestForm) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(form.fields) { field in
                let value = Binding<ConversationJSONValue?>(
                    get: {
                        let current = formValues[field.id] ?? field.defaultValue
                        return current == .null ? nil : current
                    },
                    set: { formValues[field.id] = $0 ?? .null }
                )
                VStack(alignment: .leading, spacing: 4) {
                    if let help = field.help { Text(help).font(.caption).foregroundStyle(.secondary) }
                    if let options = field.options {
                        Picker(field.title, selection: value) {
                            Text(field.required ? "Choose…" : "None").tag(nil as ConversationJSONValue?)
                            ForEach(Array(options.enumerated()), id: \.offset) { _, option in
                                Text(option.formLabel).tag(Optional(option))
                            }
                        }
                    } else if field.type == "boolean" {
                        Picker(field.title, selection: value) {
                            Text(field.required ? "Choose…" : "None").tag(nil as ConversationJSONValue?)
                            Text("Yes").tag(Optional(ConversationJSONValue.boolean(true)))
                            Text("No").tag(Optional(ConversationJSONValue.boolean(false)))
                        }
                    } else {
                        TextField(field.title, text: Binding(
                            get: { value.wrappedValue?.formLabel ?? "" },
                            set: { value.wrappedValue = $0.isEmpty ? nil : .string($0) }
                        ))
                        .textFieldStyle(.roundedBorder)
                        .keyboardType(field.type == "string" ? .default : .numbersAndPunctuation)
                    }
                }
            }
        }
        .disabled(!canResolve)
    }

    private var isLongPrompt: Bool {
        request.prompt.count > 400 || request.prompt.filter(\.isNewline).count >= 8
    }
}

/// What happened to an answer. A refusal is an ordinary outcome, drawn in
/// the same quiet style as the rest of the card rather than as an alert.
struct ConversationAnswerStatus: View {
    let answer: ConversationRequestAnswerPresentation

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if case .submitting = answer.outcome {
                ProgressView().controlSize(.mini)
            } else {
                Image(systemName: symbol)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(answer.title).fontWeight(.medium)
                if let detail = answer.detail {
                    Text(detail).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 0)
        }
        .font(.caption)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("conversation.request.answer")
    }

    private var symbol: String {
        switch answer.outcome {
        case .submitting, .submitted: "checkmark.circle"
        case .refused: "arrow.uturn.backward.circle"
        case .uncertain: "questionmark.circle"
        case .notSent: "wifi.slash"
        }
    }
}

extension ConversationRequestKind {
    var symbol: String {
        self == .permission ? "lock.shield" : "questionmark.bubble"
    }
}

private enum RichRequestKind: String {
    case command, network, permissions, browser, question
    case fileChange = "file_change"
    case sandboxEscalation = "sandbox_escalation"
    case mcpTool = "mcp_tool"
    case appAccess = "app_access"
    case planApproval = "plan_approval"
    case elicitationForm = "elicitation_form"
    case elicitationURL = "elicitation_url"
    case signIn = "sign_in"

    var appearance: (String, String) {
        switch self {
        case .command: ("Run command", "terminal")
        case .fileChange: ("File change requested", "doc.badge.ellipsis")
        case .network: ("Network access requested", "network")
        case .permissions, .sandboxEscalation: ("Additional permissions requested", "lock.shield")
        case .mcpTool: ("Tool approval requested", "wrench.and.screwdriver")
        case .appAccess: ("App access requested", "macwindow")
        case .browser: ("Browser action requested", "safari")
        case .question: ("Question", "questionmark.bubble")
        case .planApproval: ("Plan ready for approval", "list.bullet.clipboard")
        case .elicitationForm: ("Information requested", "list.bullet.rectangle")
        case .elicitationURL: ("Action requested", "link")
        case .signIn: ("Sign-in requested", "person.badge.key")
        }
    }
}

private extension ConversationRequestPresentation {
    var semanticKind: RichRequestKind? { metadata?.requestKind.flatMap(RichRequestKind.init(rawValue:)) }
    var title: String { semanticKind?.appearance.0 ?? kind.title }
    var symbol: String { semanticKind?.appearance.1 ?? kind.symbol }
    var options: [ConversationRequestOption] {
        // Other is a known generic kind. Future kinds use the legacy controls.
        guard semanticKind != nil || metadata?.requestKind == "other" else { return [] }
        var seen = Set<String>()
        return (metadata?.options ?? []).filter { seen.insert($0.id).inserted }
    }

    var actionURL: URL? {
        guard semanticKind == .elicitationURL || semanticKind == .signIn || semanticKind == .browser,
              let raw = metadata?.details?["url"]?.string,
              let url = URL(string: raw),
              url.scheme.map({ ["https", "http"].contains($0.lowercased()) }) == true,
              url.host?.isEmpty == false, url.user == nil, url.password == nil else { return nil }
        return url
    }

    var resolutionCaption: String? {
        guard let outcome = status.outcome else { return nil }
        let source: String? = switch metadata?.resolvedBy {
        case "phone": "on phone"
        case "terminal": "at terminal"
        case "policy": "by policy"
        case "hook": "by hook"
        case "timeout": nil
        default: nil
        }
        let label = metadata?.decision.map { decision in
            switch decision.split(separator: "-", maxSplits: 1).first.map(String.init) ?? decision {
            case "allow_once": "Allowed once"
            case "allow_session": "Allowed for session"
            case "allow_always": "Always allowed"
            case "deny", "deny_with_feedback": "Denied"
            case "deny_always": "Always denied"
            case "approve_plan": "Plan approved"
            case "reject_plan_with_feedback": "Plan rejected"
            case "cancel": "Cancelled"
            case "submit_form", "submitted": "Submitted"
            default: decision.replacingOccurrences(of: "_", with: " ").capitalized
            }
        }
        let text = label ?? (metadata?.resolvedBy == "timeout" ? "Timed out" : outcome)
        return source.map { "\(text) \($0)" } ?? text
    }
}

private struct ConversationRequestDetails: View {
    let request: ConversationRequestPresentation
    var includesLink = true

    private var details: [String: ConversationJSONValue] { request.metadata?.details ?? [:] }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            switch request.semanticKind {
            case .command:
                if let command = details["command"]?.string {
                    Text(command).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                }
                field("Working directory", "cwd")
                field("Sandbox", "sandbox")
                field("Action", "action")
                field("Reason", "reason")
                lines("fileSystemScope")
                lines("networkScope")
            case .fileChange:
                field("Write access under", "grantRoot")
                field("Reason", "reason")
                lines("paths")
                if let diff = details["diff"]?.string { ConversationDiffDisclosure(diff: diff) }
                ForEach(Array((details["changes"]?.array ?? []).enumerated()), id: \.offset) { _, change in
                    if let entry = change.object {
                        if let path = entry["path"]?.string { Text(path).font(.caption.monospaced()) }
                        if let summary = entry["summary"]?.string { Text(summary).font(.caption) }
                        if let diff = entry["diff"]?.string { ConversationDiffDisclosure(diff: diff) }
                    }
                }
            case .network:
                field("Host", "host")
                field("Protocol", "protocol")
                field("Command", "command")
                field("Working directory", "cwd")
                field("Reason", "reason")
            case .permissions, .sandboxEscalation:
                field("Reason", "reason")
                field("Working directory", "cwd")
                field("Scope", "scope")
                lines("fileSystemScope")
                lines("networkScope")
            case .mcpTool:
                field("App", "connectorName")
                field("Server", "server")
                field("Tool", "tool")
                field("Arguments", "argumentsSummary")
            case .appAccess:
                field("App", "appName")
                field("Bundle identifier", "appId")
                field("Server", "server")
                field("Tool", "tool")
                field("Scope", "scope")
            case .browser:
                field("Origin", "origin")
                field("Action", "action")
                field("Access lifetime", "scope")
            case .planApproval:
                field("Plan file", "planPath")
                if let markdown = details["planMarkdown"]?.string {
                    DisclosureGroup("Plan") {
                        ScrollView { ConversationMarkdownView(text: markdown) }
                            .frame(maxHeight: 220)
                    }
                }
            case .elicitationForm, .elicitationURL:
                if details["message"]?.string != request.prompt { field("Message", "message") }
            case .signIn:
                field("Service", "service")
            case .question, nil:
                EmptyView()
            }
            if includesLink, let url = request.actionURL {
                Link("Open \(url.host ?? "link")", destination: url)
            }
        }
        .foregroundStyle(.secondary)
    }

    @ViewBuilder
    private func field(_ label: String, _ key: String) -> some View {
        if let value = details[key]?.string, !value.isEmpty, value != request.prompt {
            LabeledContent(label) { Text(value).textSelection(.enabled) }.font(.caption)
        }
    }

    private func lines(_ key: String) -> some View {
        ForEach(Array((details[key]?.array ?? []).compactMap(\.string).enumerated()), id: \.offset) { _, value in
            Text(value).font(.caption.monospaced()).textSelection(.enabled)
        }
    }
}

struct ConversationDiffDisclosure: View {
    let diff: String

    var body: some View {
        DisclosureGroup("Diff") {
            ScrollView([.horizontal, .vertical]) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(diff.split(separator: "\n", omittingEmptySubsequences: false).enumerated()), id: \.offset) { _, line in
                        Text(String(line))
                            .foregroundStyle(line.hasPrefix("+") ? Color.green : line.hasPrefix("-") ? Color.red : Color.primary)
                    }
                }
                .font(.caption.monospaced())
                .textSelection(.enabled)
            }
            .frame(maxHeight: 200)
        }
        .font(.caption)
    }
}

private struct RequestForm {
    struct Field: Identifiable {
        let id: String
        let title: String
        let help: String?
        let type: String
        let options: [ConversationJSONValue]?
        let required: Bool
        let defaultValue: ConversationJSONValue?
        let minLength: Int64?
        let maxLength: Int64?
        let minimum: Double?
        let maximum: Double?

        func encode(_ value: ConversationJSONValue) -> ConversationJSONValue? {
            if let options, !options.contains(value) { return nil }
            let encoded: ConversationJSONValue
            switch (type, value) {
            case ("string", .string(let text)):
                let length = Int64(text.unicodeScalars.count)
                guard text.utf8.count <= 4096, length >= (minLength ?? 0), length <= (maxLength ?? 4096) else { return nil }
                return value
            case ("boolean", .boolean): return value
            case ("integer", .integer), ("number", .integer), ("number", .number):
                encoded = value
            case ("integer", .string(let text)):
                guard let value = Int64(text) else { return nil }
                encoded = .integer(value)
            case ("number", .string(let text)):
                guard let value = Double(text), value.isFinite else { return nil }
                encoded = .number(value)
            default: return nil
            }
            guard let number = encoded.formNumber,
                  minimum.map({ number >= $0 }) ?? true,
                  maximum.map({ number <= $0 }) ?? true else { return nil }
            return encoded
        }
    }

    let fields: [Field]

    init?(_ schema: [String: ConversationJSONValue]?) {
        guard let schema, schema["type"]?.string == "object",
              Set(schema.keys).isSubset(of: ["$schema", "type", "properties", "required", "additionalProperties", "title", "description"]),
              let properties = schema["properties"]?.object, properties.count <= 32 else { return nil }
        let required = Set((schema["required"]?.array ?? []).compactMap(\.string))
        guard required.isSubset(of: Set(properties.keys)) else { return nil }
        var fields: [Field] = []
        for (id, property) in properties.sorted(by: { $0.key < $1.key }) {
            guard !id.isEmpty, id.utf8.count <= 256, let field = property.object,
                  Set(field.keys).isSubset(of: ["type", "title", "description", "enum", "default", "minLength", "maxLength", "minimum", "maximum"]),
                  let type = field["type"]?.string, ["string", "boolean", "integer", "number"].contains(type) else { return nil }
            let options = field["enum"]?.array
            if let options, options.isEmpty || options.count > 32 || options.contains(where: { $0.formLabel.isEmpty }) { return nil }
            fields.append(Field(id: id, title: field["title"]?.string ?? id, help: field["description"]?.string, type: type,
                                options: options, required: required.contains(id), defaultValue: field["default"],
                                minLength: field["minLength"]?.formInteger, maxLength: field["maxLength"]?.formInteger,
                                minimum: field["minimum"]?.formNumber, maximum: field["maximum"]?.formNumber))
        }
        self.fields = fields
    }

    func submission(_ values: [String: ConversationJSONValue]) -> [String: ConversationJSONValue]? {
        var form: [String: ConversationJSONValue] = [:]
        for field in fields {
            if let value = values[field.id] ?? field.defaultValue, value != .null {
                guard let encoded = field.encode(value) else { return nil }
                form[field.id] = encoded
            } else if field.required {
                return nil
            }
        }
        guard let data = try? JSONEncoder().encode(form), data.count <= 16 * 1024 else { return nil }
        return form
    }
}

private extension ConversationJSONValue {
    var formInteger: Int64? {
        if case .integer(let value) = self { return value }
        return nil
    }

    var formNumber: Double? {
        switch self {
        case .integer(let value): Double(value)
        case .number(let value): value
        default: nil
        }
    }

    var formLabel: String {
        switch self {
        case .string(let value): value
        case .boolean(let value): value ? "Yes" : "No"
        case .integer(let value): String(value)
        case .number(let value): String(value)
        default: ""
        }
    }
}

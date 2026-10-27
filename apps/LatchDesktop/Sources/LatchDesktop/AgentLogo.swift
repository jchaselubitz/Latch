import SwiftUI

/// The Claude and Codex marks, drawn from their published 24×24 SVG paths so
/// they stay crisp at any size without bundling image resources. Arcs in the
/// source artwork are pre-converted to cubic curves, leaving only M, L, C and Z.
enum BrandMark {
    case claude
    case codex

    fileprivate var path: Path {
        switch self {
        case .claude: return Self.claudePath
        case .codex: return Self.codexPath
        }
    }

    /// Each mark in its brand colours: Claude's spark in Anthropic's Claude
    /// orange, which reads on light and dark backgrounds alike, and the Codex
    /// mark in OpenAI's Codex gradient. On dark backgrounds the Codex gradient
    /// is lifted so its deep blue end keeps its contrast.
    func fill(for colorScheme: ColorScheme) -> AnyShapeStyle {
        switch self {
        case .claude:
            return AnyShapeStyle(Self.claudeOrange)
        case .codex:
            let stops = colorScheme == .dark ? Self.codexDarkStops : Self.codexStops
            return AnyShapeStyle(LinearGradient(stops: stops, startPoint: .top, endPoint: .bottom))
        }
    }

    private static let claudeOrange = Color(hex: 0xD97757)

    private static let codexStops: [Gradient.Stop] = [
        .init(color: Color(hex: 0xB1A7FF), location: 0),
        .init(color: Color(hex: 0x7A9DFF), location: 0.5),
        .init(color: Color(hex: 0x3941FF), location: 1),
    ]

    private static let codexDarkStops: [Gradient.Stop] = [
        .init(color: Color(hex: 0xD3CDFF), location: 0),
        .init(color: Color(hex: 0xA6BEFF), location: 0.5),
        .init(color: Color(hex: 0x7077FF), location: 1),
    ]

    private static let claudePath = parse("""
        M4.709 15.955L9.429 13.308L9.509 13.078L9.429 12.95L9.2 12.95L8.41 12.902L5.712 12.829L3.373 12.732
        L1.107 12.61L0.536 12.489L0 11.784L0.055 11.432L0.535 11.111L1.221 11.171L2.741 11.274L5.019 11.432
        L6.671 11.529L9.12 11.784L9.509 11.784L9.564 11.627L9.43 11.529L9.327 11.432L6.969 9.836L4.417 8.148
        L3.081 7.176L2.357 6.685L1.993 6.223L1.835 5.215L2.491 4.493L3.372 4.553L3.597 4.614L4.49 5.3
        L6.398 6.776L8.889 8.609L9.254 8.913L9.399 8.81L9.418 8.737L9.254 8.463L7.899 6.017L6.453 3.527
        L5.809 2.495L5.639 1.876C5.574 1.638 5.539 1.393 5.535 1.147L6.283 0.134L6.696 0L7.692 0.134
        L8.112 0.498L8.732 1.912L9.734 4.141L11.289 7.171L11.745 8.069L11.988 8.901L12.079 9.156
        L12.237 9.156L12.237 9.01L12.365 7.304L12.602 5.209L12.832 2.514L12.912 1.754L13.288 0.844
        L14.035 0.352L14.619 0.632L15.099 1.317L15.032 1.761L14.746 3.612L14.187 6.515L13.823 8.457
        L14.035 8.457L14.278 8.215L15.263 6.909L16.915 4.845L17.645 4.025L18.495 3.121L19.042 2.69
        L20.075 2.69L20.835 3.819L20.495 4.985L19.431 6.332L18.55 7.474L17.286 9.174L16.496 10.534
        L16.569 10.644L16.757 10.624L19.613 10.018L21.156 9.738L22.997 9.423L23.83 9.811L23.921 10.206
        L23.593 11.013L21.624 11.499L19.315 11.961L15.876 12.774L15.834 12.804L15.883 12.865L17.432 13.011
        L18.094 13.047L19.716 13.047L22.736 13.272L23.526 13.794L24 14.432L23.921 14.917L22.706 15.537
        L21.066 15.148L17.237 14.238L15.925 13.909L15.743 13.909L15.743 14.019L16.836 15.087L18.842 16.897
        L21.351 19.227L21.478 19.805L21.156 20.26L20.816 20.211L18.611 18.554L17.76 17.807L15.834 16.187
        L15.706 16.187L15.706 16.357L16.15 17.006L18.495 20.527L18.617 21.607L18.447 21.96L17.839 22.173
        L17.171 22.051L15.797 20.126L14.382 17.959L13.239 16.016L13.099 16.096L12.425 23.35L12.109 23.72
        L11.38 24L10.773 23.539L10.451 22.792L10.773 21.316L11.162 19.392L11.477 17.862L11.763 15.962
        L11.933 15.33L11.921 15.288L11.781 15.306L10.347 17.273L8.167 20.218L6.441 22.063L6.027 22.227
        L5.31 21.857L5.377 21.195L5.778 20.606L8.166 17.57L9.606 15.688L10.536 14.602L10.53 14.444
        L10.475 14.444L4.132 18.56L3.002 18.706L2.515 18.25L2.576 17.504L2.807 17.261L4.715 15.949
        L4.709 15.955Z
        """)

    private static let codexPath = parse("""
        M8.086 0.457C9.049 0.061 10.098 -0.082 11.132 0.042C12.465 0.195 13.653 0.762 14.696 1.742
        C14.725 1.769 14.765 1.78 14.803 1.771C16.211 1.425 17.565 1.547 18.864 2.137L18.927 2.167
        L19.081 2.243C20.438 2.946 21.411 4.013 21.999 5.441C22.277 6.12 22.417 6.829 22.42 7.567
        C22.44 8.117 22.379 8.666 22.24 9.198C22.226 9.253 22.241 9.312 22.28 9.353
        C23.066 10.15 23.613 11.152 23.858 12.244C24.243 14.145 23.848 15.859 22.675 17.384L22.493 17.604
        C21.716 18.494 20.696 19.137 19.559 19.455C19.509 19.47 19.468 19.508 19.451 19.557
        C19.196 20.293 18.94 20.921 18.464 21.549C17.265 23.131 15.502 24.011 13.516 24
        C11.933 23.992 10.53 23.413 9.306 22.264C9.268 22.229 9.215 22.217 9.166 22.232
        C8.648 22.399 8.126 22.423 7.562 22.417C6.661 22.41 5.773 22.197 4.967 21.795
        C4.123 21.376 3.388 20.766 2.821 20.014C2.618 19.745 2.417 19.492 2.27 19.193
        C2.067 18.781 1.902 18.352 1.775 17.91C1.509 16.907 1.503 15.852 1.758 14.846
        C1.766 14.822 1.769 14.797 1.766 14.772C1.761 14.747 1.748 14.725 1.729 14.708
        C1.113 14.084 0.641 13.333 0.349 12.506C0.155 11.997 0.043 11.461 0.016 10.917
        C-0.032 10.201 0.031 9.482 0.204 8.785C0.654 7.301 1.513 6.137 2.781 5.292
        C3.063 5.104 3.331 4.958 3.583 4.854C3.869 4.734 4.156 4.634 4.444 4.55
        C4.486 4.538 4.519 4.505 4.531 4.463C4.749 3.678 5.125 2.945 5.635 2.31
        C6.315 1.464 7.132 0.846 8.086 0.457ZM7.282 8.307C7.049 7.9 6.531 7.759 6.125 7.992
        C5.718 8.224 5.576 8.742 5.809 9.149L7.503 12.114L5.815 14.962C5.6 15.363 5.739 15.862 6.131 16.094
        C6.522 16.326 7.027 16.207 7.275 15.826L9.215 12.554C9.371 12.291 9.373 11.965 9.222 11.7
        L7.282 8.307ZM12.728 14.547C12.279 14.574 11.929 14.945 11.929 15.395
        C11.929 15.844 12.279 16.215 12.728 16.242L17.576 16.242C18.028 16.22 18.384 15.847 18.384 15.394
        C18.384 14.941 18.028 14.568 17.576 14.546L12.728 14.546Z
        """)

    /// Reads a path made only of absolute M, L, C and Z commands.
    private static func parse(_ data: String) -> Path {
        var path = Path()
        var command: Character = "M"
        var numbers: [CGFloat] = []
        var token = ""

        func flushToken() {
            if let value = Double(token) { numbers.append(CGFloat(value)) }
            token = ""
        }
        func emit() {
            switch command {
            case "M" where numbers.count == 2:
                path.move(to: CGPoint(x: numbers[0], y: numbers[1]))
            case "L" where numbers.count == 2:
                path.addLine(to: CGPoint(x: numbers[0], y: numbers[1]))
            case "C" where numbers.count == 6:
                path.addCurve(
                    to: CGPoint(x: numbers[4], y: numbers[5]),
                    control1: CGPoint(x: numbers[0], y: numbers[1]),
                    control2: CGPoint(x: numbers[2], y: numbers[3])
                )
            case "Z":
                path.closeSubpath()
            default:
                assertionFailure("Malformed brand mark path near \(command)")
            }
            numbers.removeAll(keepingCapacity: true)
        }

        var started = false
        for character in data {
            if "MLCZ".contains(character) {
                flushToken()
                if started { emit() }
                started = true
                command = character
            } else if character == " " || character.isNewline {
                flushToken()
            } else {
                token.append(character)
            }
        }
        flushToken()
        emit()
        return path
    }
}

/// Scales a brand mark's 24×24 artwork to fit, centred, in any frame.
private struct BrandMarkShape: Shape {
    let mark: BrandMark

    func path(in rect: CGRect) -> Path {
        let scale = min(rect.width, rect.height) / 24
        let transform = CGAffineTransform(
            translationX: rect.midX - 12 * scale,
            y: rect.midY - 12 * scale
        ).scaledBy(x: scale, y: scale)
        return mark.path.applying(transform)
    }
}

extension SessionAgent {
    var brandMark: BrandMark? {
        switch self {
        case .claude: return .claude
        case .codex: return .codex
        case .shell: return nil
        }
    }
}

/// What a session runs, drawn on a transparent background: the agent's own
/// logo, or a terminal symbol for a plain shell.
struct AgentLogo: View {
    let agent: SessionAgent
    var size: CGFloat = 20
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        Group {
            if let mark = agent.brandMark {
                BrandMarkShape(mark: mark)
                    .fill(mark.fill(for: colorScheme), style: FillStyle(eoFill: true))
            } else {
                Image(systemName: "apple.terminal")
                    .font(.system(size: size * 0.78, weight: .medium))
                    .foregroundStyle(.secondary)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

private extension Color {
    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }
}

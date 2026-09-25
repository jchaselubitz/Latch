import SwiftUI

enum FloatingControlMetrics {
    /// The largest text size the controls follow. Past it the title chip
    /// and the composer field would be squeezed to nothing between them.
    static let largestTypeSize = DynamicTypeSize.accessibility1
    /// Roughly how far a control's side has grown at that size.
    static let largestScale: CGFloat = 1.4
}

/// The outline a floating control is drawn in.
enum FloatingControlShape {
    /// A glyph-only button: back, gear, "+", send, "⋯".
    case circle
    /// A labelled control: the "+ Session" pill, the title and status chip.
    case capsule

    fileprivate var shape: AnyShape {
        switch self {
        case .circle: AnyShape(Circle())
        case .capsule: AnyShape(Capsule())
        }
    }
}

/// One control that floats over scrolling content: the back button, the
/// status chip, the terminal's "⋯", the composer's "+" and send, the
/// "+ Session" pill and the Settings gear.
///
/// On iOS 26 it is Liquid Glass, interactive so it answers a press; on
/// iOS 17-25 it is regular material with a hairline edge, which reads the
/// same over the transcript, the session list and the terminal's black.
///
/// It grows with Dynamic Type, up to the first accessibility size, so the
/// glyph never outgrows its circle and a row of controls still fits a
/// 375pt screen. It never drops below the 44pt hit target.
struct FloatingControl<Label: View>: View {
    var shape: FloatingControlShape = .circle
    /// The resting size at the default text size; never below 44.
    var size: CGFloat = 44
    /// Fills the control, for the one primary action on a surface.
    var tint: Color?
    @ViewBuilder var label: Label

    var body: some View {
        // The cap is applied outside the surface so its scaled size reads
        // the capped text size too.
        FloatingControlSurface(shape: shape, size: max(size, 44), tint: tint, label: label)
            .dynamicTypeSize(...FloatingControlMetrics.largestTypeSize)
    }
}

extension FloatingControl where Label == FloatingControlGlyph {
    /// A circular control showing one SF Symbol.
    init(systemImage: String, size: CGFloat = 44, tint: Color? = nil) {
        self.init(shape: .circle, size: size, tint: tint) {
            FloatingControlGlyph(systemImage: systemImage)
        }
    }
}

/// A floating control's symbol, sized by text style so it scales with the
/// circle around it.
struct FloatingControlGlyph: View {
    let systemImage: String

    var body: some View {
        Image(systemName: systemImage)
            .font(.body.weight(.semibold))
            .imageScale(.large)
    }
}

private struct FloatingControlSurface<Label: View>: View {
    let shape: FloatingControlShape
    let size: CGFloat
    let tint: Color?
    let label: Label

    @ScaledMetric(relativeTo: .body) private var scale: CGFloat = 1

    private var side: CGFloat { (size * scale).rounded() }

    var body: some View {
        switch shape {
        case .circle:
            label
                .frame(width: side, height: side)
                .floatingControlBackground(.circle, tint: tint)
        case .capsule:
            label
                .padding(.horizontal, 16)
                .frame(minWidth: side, minHeight: side)
                .floatingControlBackground(.capsule, tint: tint)
        }
    }
}

extension View {
    /// The floating fill on its own, for a view that lays itself out:
    /// glass on iOS 26, material with a hairline edge before it. A tint
    /// fills the control instead, as for the primary action.
    func floatingControlBackground(_ shape: FloatingControlShape, tint: Color? = nil) -> some View {
        modifier(FloatingControlBackground(outline: shape.shape, tint: tint))
    }
}

private struct FloatingControlBackground: ViewModifier {
    let outline: AnyShape
    let tint: Color?

    func body(content: Content) -> some View {
        Group {
            if #available(iOS 26, *) {
                content.glassEffect(glass, in: outline)
            } else {
                content
                    .background {
                        if let tint {
                            outline.fill(tint)
                        } else {
                            outline.fill(.regularMaterial)
                        }
                    }
                    .overlay {
                        outline.stroke(Color(.separator), lineWidth: 0.5)
                    }
            }
        }
        .contentShape(outline)
    }

    @available(iOS 26, *)
    private var glass: Glass {
        var glass = Glass.regular
        if let tint { glass = glass.tint(tint) }
        return glass.interactive()
    }
}

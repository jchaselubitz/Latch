import SwiftUI

/// A selectable monospaced command or document on a quiet rounded backing.
struct CopyableCodeBlock: View {
    private let text: String
    private let style: Font.TextStyle
    private let padding: CGFloat
    private let cornerRadius: CGFloat
    /// Stretches the backing to the container's width instead of hugging the text.
    private let fillsWidth: Bool

    init(
        _ text: String,
        style: Font.TextStyle = .caption,
        padding: CGFloat = 8,
        cornerRadius: CGFloat = 6,
        fillsWidth: Bool = true
    ) {
        self.text = text
        self.style = style
        self.padding = padding
        self.cornerRadius = cornerRadius
        self.fillsWidth = fillsWidth
    }

    var body: some View {
        Text(text)
            .font(.system(style, design: .monospaced))
            .textSelection(.enabled)
            .padding(padding)
            .frame(maxWidth: fillsWidth ? .infinity : nil, alignment: .leading)
            .background(.quaternary, in: RoundedRectangle(cornerRadius: cornerRadius))
    }
}

import SwiftUI
import AppKit
import UniformTypeIdentifiers

/// Section headers and footnotes are repeated across every settings tab, so they
/// live here to keep one typographic voice instead of ad-hoc `.font(.caption)`
/// calls scattered through the form.
struct SettingsSectionHeader: View {
    private let title: String

    init(_ title: String) { self.title = title }

    var body: some View {
        Text(title)
            .font(.headline)
            .padding(.bottom, 2)
    }
}

struct SettingsFootnote: View {
    private let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }
}

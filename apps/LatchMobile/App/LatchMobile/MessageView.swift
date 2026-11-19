import SwiftUI

/// A centered empty or error state.
struct MessageView: View {
    let icon: String
    let title: String
    let detail: String

    var body: some View {
        ScrollView {
            VStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.largeTitle)
                    .foregroundStyle(.secondary)
                Text(title)
                    .font(.headline)
                Text(detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding(32)
            .frame(maxWidth: .infinity)
            .padding(.top, 60)
        }
    }
}

/// A label over its selectable value, read as one element.
struct StackedLabeledValue: View {
    private let label: String
    private let value: String
    private let monospaced: Bool

    init(_ label: String, _ value: String, monospaced: Bool = false) {
        self.label = label
        self.value = value
        self.monospaced = monospaced
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
            Text(value)
                .font(monospaced ? .callout.monospaced() : .callout)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
        }
        .accessibilityElement(children: .combine)
    }
}

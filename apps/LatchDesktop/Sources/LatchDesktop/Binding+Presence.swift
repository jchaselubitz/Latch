import SwiftUI

extension Binding {
    /// A presentation flag that is true while the optional holds a value and
    /// clears it when the presentation is dismissed.
    func isPresent<Wrapped>() -> Binding<Bool> where Value == Wrapped? {
        Binding<Bool>(
            get: { wrappedValue != nil },
            set: { presented in if !presented { wrappedValue = nil } }
        )
    }
}

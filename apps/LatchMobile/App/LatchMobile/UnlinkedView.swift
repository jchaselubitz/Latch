import SwiftUI

struct UnlinkedView: View {
    /// The Mac this phone paired with, when there is one.
    let pairedMac: String?

    var body: some View {
        MessageView(
            icon: pairedMac == nil ? "laptopcomputer.and.iphone" : "cable.connector",
            title: pairedMac == nil ? "No computer linked" : "Paired, but not linked",
            detail: detail
        )
    }

    private var detail: String {
        guard let pairedMac else {
            return "Tap the gear to open Settings and pair this phone with your Mac."
        }
        return """
        Looking for \(pairedMac) over authenticated Remote Link. Keep Remote Access enabled \
        on the Mac and check that the control plane is reachable.
        """
    }
}

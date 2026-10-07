import LatchMobileKit
import SwiftUI

/// The list's own title row: "Latch" on the left, and opposite it the pill
/// that says whether the rows below are live and which Mac they come from.
struct SessionListHeader: View {
    let status: SessionListLinkStatus?
    let deviceName: String?

    var body: some View {
        HStack(alignment: .center) {
            Text("Latch")
                .font(.largeTitle.weight(.bold))
                .accessibilityAddTraits(.isHeader)
            Spacer(minLength: 12)
            if let status {
                LinkStatusPill(status: status, deviceName: deviceName)
            }
        }
    }
}

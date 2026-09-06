import Foundation

/// Explicitly enabled, local-only ICE traces for field investigations.
public enum NativeTransportDiagnostics {
    public static let preferenceKey = "latch.iceDiagnosticsEnabled"
    public static var logURL: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("ice-debug.log")
    }

    public static func setEnabled(_ enabled: Bool) throws {
        if enabled {
            try FileManager.default.createDirectory(
                at: logURL.deletingLastPathComponent(), withIntermediateDirectories: true
            )
        }
        try configureIceDiagnostics(path: enabled ? logURL.path : nil)
        UserDefaults.standard.set(enabled, forKey: preferenceKey)
    }

    public static func restore() throws {
        if UserDefaults.standard.bool(forKey: preferenceKey) { try setEnabled(true) }
    }
}

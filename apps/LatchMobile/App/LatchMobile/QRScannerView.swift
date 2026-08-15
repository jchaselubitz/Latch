import AVFoundation
import SwiftUI

#if canImport(UIKit)
import UIKit

/// A live camera preview that reports every QR code it reads.
///
/// It deliberately does no validation and keeps no state: the same code is
/// reported many times a second, and deciding what a scan means belongs to
/// `PairingModel`, which already has to make that decision for hand-entered
/// codes too. The capture session is torn down when the view goes away so the
/// camera indicator does not stay lit behind another screen.
struct QRScannerView: UIViewControllerRepresentable {
    /// Called on the main actor for each machine-readable code.
    let onCode: (String) -> Void

    func makeUIViewController(context: Context) -> ScannerViewController {
        let controller = ScannerViewController()
        controller.onCode = onCode
        return controller
    }

    func updateUIViewController(_ controller: ScannerViewController, context: Context) {
        controller.onCode = onCode
    }

    final class ScannerViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
        var onCode: ((String) -> Void)?

        private let session = AVCaptureSession()
        private var preview: AVCaptureVideoPreviewLayer?
        /// Capture configuration blocks the caller, so it stays off the main
        /// queue; the preview layer is attached back on the main actor.
        private let queue = DispatchQueue(label: "dev.cooperativ.latch.scanner")

        override func viewDidLoad() {
            super.viewDidLoad()
            view.backgroundColor = .black
            configure()
        }

        override func viewDidLayoutSubviews() {
            super.viewDidLayoutSubviews()
            preview?.frame = view.bounds
        }

        override func viewWillDisappear(_ animated: Bool) {
            super.viewWillDisappear(animated)
            let session = session
            queue.async { if session.isRunning { session.stopRunning() } }
        }

        private func configure() {
            guard
                let device = AVCaptureDevice.default(for: .video),
                let input = try? AVCaptureDeviceInput(device: device),
                session.canAddInput(input)
            else {
                // No usable camera. `CameraPermission.unavailable` is what the
                // screen shows for this; the preview simply stays black.
                return
            }
            session.addInput(input)

            let output = AVCaptureMetadataOutput()
            guard session.canAddOutput(output) else { return }
            session.addOutput(output)
            output.setMetadataObjectsDelegate(self, queue: .main)
            output.metadataObjectTypes = [.qr]

            let preview = AVCaptureVideoPreviewLayer(session: session)
            preview.videoGravity = .resizeAspectFill
            preview.frame = view.bounds
            view.layer.addSublayer(preview)
            self.preview = preview

            let session = session
            queue.async { if !session.isRunning { session.startRunning() } }
        }

        func metadataOutput(
            _ output: AVCaptureMetadataOutput,
            didOutput objects: [AVMetadataObject],
            from connection: AVCaptureConnection
        ) {
            for object in objects {
                guard
                    let readable = object as? AVMetadataMachineReadableCodeObject,
                    readable.type == .qr,
                    let value = readable.stringValue
                else { continue }
                // The metadata queue is `.main`, so this is already the main
                // actor; saying so keeps the callback synchronous instead of
                // hopping and delivering scans out of order.
                MainActor.assumeIsolated { onCode?(value) }
            }
        }
    }
}
#endif

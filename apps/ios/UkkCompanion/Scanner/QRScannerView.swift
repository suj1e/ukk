import AVFoundation
import SwiftUI
import UIKit

/// Owns one `AVCaptureSession`. Payloads are delivered once; the session stops
/// immediately after the first successful QR read. No payload is logged.
final class QRScannerController: NSObject, ObservableObject, AVCaptureMetadataOutputObjectsDelegate {
    let session = AVCaptureSession()

    private let pipeline = DispatchQueue(label: "dev.ukk.companion.qr.pipeline")
    private var isConfigured = false
    private var hasDelivered = false
    /// Published so the pairing screen can replace the black preview with an
    /// actionable message when capture cannot be configured.
    @Published private(set) var configurationError: String?

    var onPayload: ((String) -> Void)?

    func start() {
        guard CameraAvailability.current().isAvailable else { return }
        if !isConfigured {
            configure()
        }
        guard configurationError == nil else { return }
        pipeline.async { [weak self] in
            guard let self, !self.session.isRunning else { return }
            self.session.startRunning()
        }
    }

    func stop() {
        pipeline.async { [weak self] in
            guard let self, self.session.isRunning else { return }
            self.session.stopRunning()
        }
    }

    private func configure() {
        isConfigured = true
        guard let device = AVCaptureDevice.default(for: .video) else {
            configurationError = "No camera available."
            return
        }
        guard let input = try? AVCaptureDeviceInput(device: device) else {
            configurationError = "The camera could not be opened."
            return
        }
        let output = AVCaptureMetadataOutput()

        session.beginConfiguration()
        guard session.canAddInput(input) else {
            session.commitConfiguration()
            configurationError = "The camera input could not be attached."
            return
        }
        session.sessionPreset = .high
        session.addInput(input)
        guard session.canAddOutput(output) else {
            session.commitConfiguration()
            configurationError = "The QR metadata output could not be attached."
            return
        }
        session.addOutput(output)
        session.commitConfiguration()

        guard output.availableMetadataObjectTypes.contains(.qr) else {
            configurationError = "This camera cannot scan QR codes."
            return
        }
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard !hasDelivered else { return }
        for object in metadataObjects {
            guard let code = object as? AVMetadataMachineReadableCodeObject,
                  code.type == .qr,
                  let value = code.stringValue,
                  !value.isEmpty else {
                continue
            }
            hasDelivered = true
            onPayload?(value)
            stop()
            return
        }
    }
}

/// Thin UIKit bridge for the capture preview. No WebView, no media pipeline.
struct QRScannerView: UIViewRepresentable {
    let controller: QRScannerController

    func makeUIView(context: Context) -> CameraPreviewView {
        let view = CameraPreviewView()
        view.backgroundColor = .black
        view.previewLayer?.session = controller.session
        view.previewLayer?.videoGravity = .resizeAspectFill
        controller.start()
        return view
    }

    func updateUIView(_ uiView: CameraPreviewView, context: Context) {}

    static func dismantleUIView(_ uiView: CameraPreviewView, coordinator: ()) {
        uiView.previewLayer?.session = nil
    }
}

final class CameraPreviewView: UIView {
    override class var layerClass: AnyClass {
        AVCaptureVideoPreviewLayer.self
    }

    var previewLayer: AVCaptureVideoPreviewLayer? {
        layer as? AVCaptureVideoPreviewLayer
    }
}

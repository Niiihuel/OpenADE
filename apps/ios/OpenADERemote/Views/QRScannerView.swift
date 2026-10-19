import SwiftUI
import AVFoundation

struct QRScannerView: View {
    let onScan: (Result<String, Error>) -> Void
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            ZStack(alignment: .bottom) {
                CameraScanner { result in onScan(result); dismiss() }.ignoresSafeArea(edges: .bottom)
                    .overlay {
                        Image(systemName: "viewfinder")
                            .font(.system(size: 220, weight: .ultraLight))
                            .foregroundStyle(.white.opacity(0.8))
                            .accessibilityHidden(true).allowsHitTesting(false)
                    }
                VStack(spacing: 10) {
                    Text("Apuntá al código de tu PC").font(.headline)
                    Text("Está en openADE → Ajustes → Remoto. Después vas a revisar qué PC estás agregando.")
                        .font(.subheadline).foregroundStyle(.white.opacity(0.8))
                }
                .multilineTextAlignment(.center).foregroundStyle(.white).padding(20)
                .background(.black.opacity(0.75), in: RoundedRectangle(cornerRadius: 24))
                .overlay { RoundedRectangle(cornerRadius: 24).strokeBorder(.white.opacity(0.2), lineWidth: 0.5) }
                .padding(20)
            }
            .remoteCanvas()
            .navigationTitle("Conectar por QR").navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancelar") { dismiss() } } }
        }
    }
}

private struct CameraScanner: UIViewControllerRepresentable {
    let onScan: (Result<String, Error>) -> Void
    func makeUIViewController(context: Context) -> ScannerController { ScannerController(onScan: onScan) }
    func updateUIViewController(_ controller: ScannerController, context: Context) {}
    static func dismantleUIViewController(_ controller: ScannerController, coordinator: ()) { controller.stop() }
}

private final class ScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    private let capture = AVCaptureSession()
    private let queue = DispatchQueue(label: "dev.openade.remote.camera")
    private var preview: AVCaptureVideoPreviewLayer?
    private let onScan: (Result<String, Error>) -> Void
    private var finished = false
    private var active = true

    init(onScan: @escaping (Result<String, Error>) -> Void) { self.onScan = onScan; super.init(nibName: nil, bundle: nil) }
    required init?(coder: NSCoder) { return nil }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: configure()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] allowed in
                DispatchQueue.main.async {
                    guard let self, self.active else { return }
                    if allowed { self.configure() } else { self.finish(.failure(PairingError.cameraUnavailable)) }
                }
            }
        default: finish(.failure(PairingError.cameraUnavailable))
        }
    }

    override func viewDidLayoutSubviews() { super.viewDidLayoutSubviews(); preview?.frame = view.bounds }

    private func configure() {
        guard active, let device = AVCaptureDevice.default(for: .video), let input = try? AVCaptureDeviceInput(device: device),
              capture.canAddInput(input) else { finish(.failure(PairingError.cameraUnavailable)); return }
        capture.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard capture.canAddOutput(output) else { finish(.failure(PairingError.cameraUnavailable)); return }
        capture.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        guard output.availableMetadataObjectTypes.contains(.qr) else { finish(.failure(PairingError.cameraUnavailable)); return }
        output.metadataObjectTypes = [.qr]
        let layer = AVCaptureVideoPreviewLayer(session: capture)
        layer.videoGravity = .resizeAspectFill
        layer.frame = view.bounds
        view.layer.addSublayer(layer)
        preview = layer
        let session = capture
        queue.async { session.startRunning() }
    }

    func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput metadataObjects: [AVMetadataObject], from connection: AVCaptureConnection) {
        guard let value = (metadataObjects.first as? AVMetadataMachineReadableCodeObject)?.stringValue else { return }
        finish(.success(value))
    }

    private func finish(_ result: Result<String, Error>) {
        guard !finished, active else { return }
        finished = true; stop(); onScan(result)
    }

    func stop() { active = false; let session = capture; queue.async { if session.isRunning { session.stopRunning() } } }
}

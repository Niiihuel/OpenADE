import SwiftUI
import PhotosUI
import UniformTypeIdentifiers
import ImageIO
import AVFoundation

/// Camera, photos or any file, uploaded to the PC as soon as it is picked;
/// the message then names the upload ids.
struct AttachmentPicker: View {
    @Binding var attachments: [UploadedFile]
    @Binding var uploading: Int
    @Environment(RemoteStore.self) private var store
    @State private var showingPhotos = false
    @State private var showingFiles = false
    @State private var showingCamera = false
    @State private var photos: [PhotosPickerItem] = []
    @State private var error: String?

    private static let limit = 8
    private static let maxBytes = 25 * 1_024 * 1_024
    private var room: Int { max(0, Self.limit - attachments.count - uploading) }

    var body: some View {
        Menu {
            if UIImagePickerController.isSourceTypeAvailable(.camera) {
                Button("Cámara", systemImage: "camera") { openCamera() }
            }
            Button("Fotos", systemImage: "photo.on.rectangle") { showingPhotos = true }
            Button("Archivos", systemImage: "paperclip") { showingFiles = true }
        } label: {
            Image(systemName: "plus")
                .font(.system(size: 20, weight: .regular))
                .frame(width: 40, height: 40)
                .contentShape(Circle())
        }
        .foregroundStyle(RemoteTheme.ink)
        .disabled(room == 0)
        .accessibilityLabel("Adjuntar")
        .photosPicker(isPresented: $showingPhotos, selection: $photos, maxSelectionCount: max(1, room), matching: .images)
        .fileImporter(isPresented: $showingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            do {
                for url in try result.get().prefix(room) { readFile(url) }
            } catch { self.error = error.localizedDescription }
        }
        .fullScreenCover(isPresented: $showingCamera) {
            CameraAttachmentPicker { data in
                showingCamera = false
                if let data { Task { await uploadImage(data, prefix: "foto") } }
            }
            .ignoresSafeArea()
        }
        .task(id: photos) {
            guard !photos.isEmpty else { return }
            let picked = photos
            photos = []
            for item in picked {
                guard let data = try? await item.loadTransferable(type: Data.self) else {
                    error = AttachmentError.unreadableImage.localizedDescription
                    continue
                }
                await uploadImage(data, prefix: "foto")
            }
        }
        .alert("No se pudo adjuntar", isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })) {
            Button("Entendido") { error = nil }
        } message: { Text(error ?? "") }
    }

    private func openCamera() {
        Task {
            if await AVCaptureDevice.requestAccess(for: .video) { showingCamera = true }
            else { error = "Permití el acceso a la cámara en Ajustes para adjuntar una foto." }
        }
    }

    private func readFile(_ url: URL) {
        let granted = url.startAccessingSecurityScopedResource()
        defer { if granted { url.stopAccessingSecurityScopedResource() } }
        do {
            let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
            guard size <= Self.maxBytes else { throw AttachmentError.tooLarge }
            let data = try Data(contentsOf: url)
            let type = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
            Task { await upload(data, filename: url.lastPathComponent, mimeType: type) }
        } catch { self.error = error.localizedDescription }
    }

    /// Photos travel as JPEG up to 2048 px, without their location metadata.
    private func uploadImage(_ original: Data, prefix: String) async {
        guard let source = CGImageSourceCreateWithData(original as CFData, nil),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: 2048
              ] as CFDictionary),
              let data = UIImage(cgImage: image).jpegData(compressionQuality: 0.85) else {
            error = AttachmentError.unreadableImage.localizedDescription
            return
        }
        await upload(data, filename: "\(prefix)-\(UUID().uuidString.prefix(6).lowercased()).jpg", mimeType: "image/jpeg")
    }

    private func upload(_ data: Data, filename: String, mimeType: String) async {
        guard !data.isEmpty, data.count <= Self.maxBytes else { error = AttachmentError.tooLarge.localizedDescription; return }
        guard attachments.count + uploading < Self.limit else { error = AttachmentError.limit.localizedDescription; return }
        uploading += 1
        defer { uploading -= 1 }
        do { attachments.append(try await store.upload(data, filename: filename, mimeType: mimeType)) }
        catch { self.error = RemoteStore.readable(error) }
    }
}

enum AttachmentError: LocalizedError {
    case tooLarge, unreadableImage, limit
    var errorDescription: String? {
        switch self {
        case .tooLarge: "Cada archivo puede ocupar hasta 25 MB."
        case .unreadableImage: "No se pudo leer esta imagen."
        case .limit: "Podés adjuntar hasta 8 archivos por mensaje."
        }
    }
}

private struct CameraAttachmentPicker: UIViewControllerRepresentable {
    let onFinish: (Data?) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(onFinish: onFinish) }
    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.mediaTypes = [UTType.image.identifier]
        picker.delegate = context.coordinator
        return picker
    }
    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}

    final class Coordinator: NSObject, UINavigationControllerDelegate, UIImagePickerControllerDelegate {
        let onFinish: (Data?) -> Void
        init(onFinish: @escaping (Data?) -> Void) { self.onFinish = onFinish }
        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { onFinish(nil) }
        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            onFinish((info[.originalImage] as? UIImage)?.jpegData(compressionQuality: 0.9))
        }
    }
}

import SwiftUI
import PhotosUI
import UniformTypeIdentifiers
import ImageIO
import AVFoundation

struct AttachmentComposerView: View {
    let sessionID: String
    let projectID: String
    let disabled: Bool
    let modeDisabled: Bool
    let allowsImages: Bool
    @Binding var prompt: String
    @Binding var mode: TurnMode
    @Binding var showingOptions: Bool
    @Environment(RemoteStore.self) private var store
    @State private var showingDocument = false
    @State private var showingPhotos = false
    @State private var showingCamera = false
    @State private var showingProject = false
    @State private var showingPending = false
    @State private var nextSource: AttachmentSource?
    @State private var photo: PhotosPickerItem?
    @State private var loading = false
    @State private var error: String?
    @State private var sourceScope = ""
    private var key: String { "attachment:\(sessionID)" }
    private var blocked: Bool { disabled || loading || store.pendingMutations[key] != nil }

    var body: some View {
        ComposerToolButton(title: "Agregar al mensaje", symbol: "plus", selected: showingOptions) {
            showingOptions = true
        }
        .disabled(disabled && modeDisabled)
        .overlay(alignment: .topTrailing) {
            if loading || store.actionIsBusy(key) { ProgressView().controlSize(.mini).allowsHitTesting(false) }
        }
        .onAppear { sourceScope = store.connectionIdentity }
        .sheet(isPresented: $showingOptions, onDismiss: openSelectedSource) {
            NavigationStack {
                ScrollView {
                    VStack(spacing: 2) {
                        if store.supports("attachments.images"), allowsImages {
                            if UIImagePickerController.isSourceTypeAvailable(.camera) {
                                ComposerActionRow(title: "Cámara", symbol: "camera") { select(.camera) }.disabled(blocked)
                            }
                            ComposerActionRow(title: "Fotos", symbol: "photo.on.rectangle") { select(.photos) }.disabled(blocked)
                        }
                        if store.supports("attachments.text") {
                            ComposerActionRow(title: "Archivos", symbol: "paperclip", subtitle: "Texto y código · hasta 256 KB por archivo") { select(.document) }.disabled(blocked)
                        }
                        if store.supports("workspace.files.read") {
                            ComposerActionRow(title: "Archivo del proyecto", symbol: "folder", subtitle: "Agregar una referencia desde tu PC") { select(.project) }.disabled(disabled)
                        }
                        Divider().overlay(RemoteTheme.border).padding(.vertical, 10)
                        ComposerActionRow(title: "Modo de planificación", symbol: "list.bullet.clipboard",
                                          subtitle: "Analizar y proponer sin modificar archivos", selected: mode == .plan) {
                            mode = mode == .plan ? .build : .plan
                            showingOptions = false
                        }.disabled(modeDisabled)
                        if store.pendingMutations[key] != nil {
                            ComposerActionRow(title: "Confirmar adjunto pendiente", symbol: "arrow.clockwise") { select(.pending) }
                        }
                    }
                    .padding(.horizontal, 24).padding(.bottom, 24)
                }
                .navigationTitle("Agregar al mensaje")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Listo") { showingOptions = false } } }
            }
            .presentationDetents([.medium, .large])
            .remoteSheet()
        }
        .photosPicker(isPresented: $showingPhotos, selection: $photo, matching: .images, photoLibrary: .shared())
        .fullScreenCover(isPresented: $showingCamera) {
            CameraAttachmentPicker { result in
                showingCamera = false
                guard sourceScope == store.connectionIdentity else { return }
                switch result {
                case .success(let data):
                    guard let data else { return }
                    Task { await prepareImage(data, prefix: "camera") }
                case .failure(let failure): error = failure.localizedDescription
                }
            }.ignoresSafeArea()
        }
        .fileImporter(isPresented: $showingDocument, allowedContentTypes: [.plainText, .sourceCode, .json, .xml], allowsMultipleSelection: false) { result in
            let scope = sourceScope
            guard scope == store.connectionIdentity else { return }
            do {
                guard let url = try result.get().first else { return }
                let granted = url.startAccessingSecurityScopedResource()
                defer { if granted { url.stopAccessingSecurityScopedResource() } }
                let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
                guard size <= 256 * 1024 else { throw AttachmentError.textTooLarge }
                let upload = try AttachmentUpload(filename: url.lastPathComponent, mediaType: "text/plain", data: Data(contentsOf: url))
                Task { if scope == store.connectionIdentity { await uploadFile(upload) } }
            } catch { self.error = error.localizedDescription }
        }
        .task(id: photo) {
            guard let selection = photo else { return }
            let scope = sourceScope
            guard scope == store.connectionIdentity else { return }
            loading = true
            defer { loading = false; photo = nil }
            do {
                guard let original = try await selection.loadTransferable(type: Data.self) else { throw AttachmentError.unreadableImage }
                guard scope == store.connectionIdentity, !Task.isCancelled else { return }
                await prepareImage(original, prefix: "photo")
            } catch { if !Task.isCancelled { self.error = error.localizedDescription } }
        }
        .sheet(isPresented: $showingPending) { PendingRequestsView().remoteSheet() }
        .sheet(isPresented: $showingProject) {
            NavigationStack {
                FilesWorkspaceView(projectID: projectID) { path in
                    guard sourceScope == store.connectionIdentity else { showingProject = false; return }
                    prompt += "\(prompt.isEmpty ? "" : "\n")Archivo del proyecto: \(path)\n"
                    showingProject = false
                }
                .navigationTitle("Citar archivo")
                .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cerrar") { showingProject = false } } }
                .remoteCanvas()
            }.remoteSheet()
        }
        .alert("No se pudo agregar el adjunto", isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })) {
            Button("Entendido") { error = nil }
        } message: { Text(error ?? "") }
    }

    private func select(_ source: AttachmentSource) {
        nextSource = source
        showingOptions = false
    }

    private func openSelectedSource() {
        guard let source = nextSource, sourceScope == store.connectionIdentity else { nextSource = nil; return }
        nextSource = nil
        switch source {
        case .document: showingDocument = true
        case .photos: showingPhotos = true
        case .project: showingProject = true
        case .pending: showingPending = true
        case .camera:
            Task {
                let scope = sourceScope
                let allowed = await AVCaptureDevice.requestAccess(for: .video)
                guard scope == store.connectionIdentity else { return }
                if allowed { showingCamera = true }
                else { error = "Permití el acceso a la cámara en Ajustes para adjuntar una foto." }
            }
        }
    }

    private func prepareImage(_ original: Data, prefix: String) async {
        guard sourceScope == store.connectionIdentity, !Task.isCancelled else { return }
        do {
            guard original.count <= 32 * 1024 * 1024,
                  let source = CGImageSourceCreateWithData(original as CFData, nil),
                  let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                    kCGImageSourceCreateThumbnailFromImageAlways: true,
                    kCGImageSourceCreateThumbnailWithTransform: true,
                    kCGImageSourceThumbnailMaxPixelSize: 2048
                  ] as CFDictionary),
                  let data = UIImage(cgImage: image).jpegData(compressionQuality: 0.85) else { throw AttachmentError.unreadableImage }
            let upload = try AttachmentUpload(filename: "\(prefix)-\(UUID().uuidString.prefix(8)).jpg", mediaType: "image/jpeg", data: data)
            await uploadFile(upload)
        } catch { self.error = error.localizedDescription }
    }

    private func uploadFile(_ upload: AttachmentUpload) async {
        loading = true
        defer { loading = false }
        do { try await store.addAttachment(upload, sessionID: sessionID, projectID: projectID); error = nil }
        catch { if !(error is CancellationError) { self.error = error.localizedDescription } }
    }
}

struct AttachmentSelectionView: View {
    let sessionID: String
    let disabled: Bool
    @Environment(RemoteStore.self) private var store

    var body: some View {
        if let attachments = store.attachments[sessionID], !attachments.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(attachments) { attachment in
                        HStack(spacing: 8) {
                            Image(systemName: attachment.isImage ? "photo" : "doc.text")
                            Text(attachment.filename).font(.caption).lineLimit(1)
                            Button {
                                store.removeAttachment(id: attachment.id, sessionID: sessionID)
                            } label: {
                                Image(systemName: "xmark").font(.caption.weight(.medium)).frame(width: 34, height: 40)
                            }
                            .buttonStyle(.plain).disabled(disabled)
                            .accessibilityLabel("Quitar \(attachment.filename)")
                        }
                        .padding(.leading, 12)
                        .background(RemoteTheme.raised, in: Capsule())
                        .accessibilityElement(children: .contain)
                    }
                }
            }
        }
    }
}

private enum AttachmentSource { case camera, photos, document, project, pending }

private struct CameraAttachmentPicker: UIViewControllerRepresentable {
    let onFinish: (Result<Data?, Error>) -> Void
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
        let onFinish: (Result<Data?, Error>) -> Void
        init(onFinish: @escaping (Result<Data?, Error>) -> Void) { self.onFinish = onFinish }
        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) { onFinish(.success(nil)) }
        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            guard let image = info[.originalImage] as? UIImage,
                  let data = image.jpegData(compressionQuality: 0.9) else {
                onFinish(.failure(AttachmentError.unreadableImage)); return
            }
            onFinish(.success(data))
        }
    }
}

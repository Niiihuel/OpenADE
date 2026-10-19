import SwiftUI

/// Text, attachments and on-device dictation. While the task runs, an empty
/// draft turns the send button into Stop, and a written one is queued on the
/// PC to go when the current turn ends.
struct PromptComposer: View {
    @Binding var text: String
    @Binding var attachments: [UploadedFile]
    var placeholder = "Pedile algo al agente"
    var running = false
    var queued = 0
    var sending = false
    var stopping = false
    var enabled = true
    var lineLimit: ClosedRange<Int> = 1...6
    var focusOnAppear = false
    let onSend: () -> Void
    var onStop: (() -> Void)? = nil

    @Environment(\.scenePhase) private var scenePhase
    @State private var dictation = ComposerDictation()
    @State private var dictationBase = ""
    @State private var dictationTask: Task<Void, Never>?
    @State private var uploading = 0
    @FocusState private var focused: Bool

    private var hasText: Bool { !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }
    private var stopMode: Bool { running && !hasText && onStop != nil }
    private var canSend: Bool { enabled && hasText && !sending && uploading == 0 }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if !attachments.isEmpty || uploading > 0 {
                AttachmentChips(attachments: $attachments, uploading: uploading)
            }
            TextField(placeholder, text: $text, axis: .vertical)
                .font(.body)
                .lineLimit(lineLimit)
                .focused($focused)
                .padding(.horizontal, 6).padding(.top, 4)
                .disabled(dictation.isRecording || dictation.isStarting)
                .accessibilityLabel("Mensaje para el agente")
            if dictation.isRecording {
                Label("Escuchando… Tocá el micrófono para terminar.", systemImage: "waveform")
                    .font(.caption).foregroundStyle(RemoteTheme.muted).padding(.horizontal, 6)
                    .accessibilityAddTraits(.updatesFrequently)
            }
            HStack(spacing: 2) {
                AttachmentPicker(attachments: $attachments, uploading: $uploading)
                    .disabled(!enabled || sending)
                ComposerToolButton(title: dictation.isRecording ? "Terminar dictado" : "Dictar mensaje",
                                   symbol: dictation.isRecording ? "waveform" : "mic", selected: dictation.isRecording) {
                    toggleDictation()
                }
                .disabled(sending)
                .overlay(alignment: .topTrailing) {
                    if dictation.isStarting { ProgressView().controlSize(.mini).allowsHitTesting(false) }
                }
                Spacer(minLength: 6)
                if running && hasText {
                    Text("Se enviará al terminar").font(.caption).foregroundStyle(RemoteTheme.muted).lineLimit(1)
                        .minimumScaleFactor(0.85)
                } else if queued > 0 {
                    RemoteTag(title: "\(queued) en cola", symbol: "tray.full")
                        .accessibilityLabel("\(queued) mensajes esperan en la PC")
                }
                sendButton
            }
        }
        .padding(10)
        .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 26, style: .continuous))
        .overlay { RoundedRectangle(cornerRadius: 26, style: .continuous).strokeBorder(RemoteTheme.border, lineWidth: 0.75) }
        .onChange(of: dictation.transcript) {
            text = dictationBase + (dictationBase.isEmpty || dictation.transcript.isEmpty ? "" : " ") + dictation.transcript
        }
        .onChange(of: scenePhase) { if scenePhase != .active { stopDictation() } }
        .onDisappear { stopDictation() }
        .task { if focusOnAppear { focused = true } }
        .alert("Dictado", isPresented: Binding(get: { dictation.error != nil }, set: { if !$0 { dictation.error = nil } })) {
            Button("Entendido") { dictation.error = nil }
        } message: { Text(dictation.error ?? "") }
    }

    private var sendButton: some View {
        Button {
            if stopMode { onStop?() } else { stopDictation(); focused = false; onSend() }
        } label: {
            Group {
                if sending || stopping { ProgressView().tint(RemoteTheme.onAccent) }
                else if stopMode { Image(systemName: "stop.fill").font(.system(size: 15, weight: .semibold)) }
                else if running { Image(systemName: "text.append").font(.system(size: 17, weight: .semibold)) }
                else { Image(systemName: "arrow.up").font(.system(size: 18, weight: .semibold)) }
            }
            .frame(width: 40, height: 40)
            .foregroundStyle(active ? RemoteTheme.onAccent : RemoteTheme.muted)
            .background(active ? RemoteTheme.accent : RemoteTheme.raised, in: Circle())
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(!active || sending || stopping)
        .accessibilityLabel(stopMode ? "Detener" : running ? "Enviar al terminar el turno" : "Enviar")
    }

    private var active: Bool { stopMode ? enabled : canSend }

    private func toggleDictation() {
        if dictation.isRecording || dictation.isStarting { stopDictation(); return }
        focused = false
        dictationBase = text
        dictationTask = Task { await dictation.start() }
    }

    private func stopDictation() {
        dictationTask?.cancel()
        dictationTask = nil
        dictation.stop()
    }
}

private struct AttachmentChips: View {
    @Binding var attachments: [UploadedFile]
    let uploading: Int

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(attachments) { file in
                    HStack(spacing: 6) {
                        Image(systemName: Self.symbol(file.filename)).font(.caption)
                        Text(file.filename).font(.caption).lineLimit(1)
                        Button { attachments.removeAll { $0.id == file.id } } label: {
                            Image(systemName: "xmark").font(.caption2.weight(.semibold)).frame(width: 28, height: 32)
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Quitar \(file.filename)")
                    }
                    .padding(.leading, 10)
                    .background(RemoteTheme.raised, in: Capsule())
                }
                if uploading > 0 {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.mini)
                        Text(uploading == 1 ? "Subiendo…" : "Subiendo \(uploading)…").font(.caption)
                    }
                    .padding(.horizontal, 10).frame(height: 32)
                    .background(RemoteTheme.raised, in: Capsule())
                }
            }
        }
        .foregroundStyle(RemoteTheme.ink)
    }

    static func symbol(_ name: String) -> String {
        let ext = name.split(separator: ".").last?.lowercased() ?? ""
        if ["jpg", "jpeg", "png", "heic", "gif", "webp"].contains(ext) { return "photo" }
        if ["pdf"].contains(ext) { return "doc.richtext" }
        return "doc.text"
    }
}

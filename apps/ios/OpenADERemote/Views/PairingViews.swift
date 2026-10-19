import SwiftUI

/// The brand mark on the same light tile as the app icon.
struct BrandMark: View {
    var size: CGFloat = 64
    var body: some View {
        Image("BrandMark")
            .resizable()
            .scaledToFit()
            .padding(size * 0.15)
            .frame(width: size, height: size)
            .background(Color(white: 0.95), in: RoundedRectangle(cornerRadius: size * 0.24, style: .continuous))
            .accessibilityHidden(true)
    }
}

/// First launch: no PC yet.
struct WelcomeView: View {
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 28) {
                    VStack(alignment: .leading, spacing: 14) {
                        BrandMark(size: 76)
                        Text("Tus agentes, desde el bolsillo.")
                            .font(.largeTitle.weight(.semibold))
                            .fixedSize(horizontal: false, vertical: true)
                        Text("Seguí las tareas de tu PC, respondé lo que te pregunten y aprobá sus acciones desde el iPhone.")
                            .font(.body).foregroundStyle(RemoteTheme.muted)
                    }
                    .padding(.top, 24)
                    RemoteCard {
                        VStack(alignment: .leading, spacing: 14) {
                            step(1, "En tu PC, abrí openADE → Ajustes → Remoto y activá el acceso.")
                            step(2, "Escaneá el código QR que aparece, o copiá su enlace y pegalo acá.")
                            step(3, "Compará el código de seguridad y listo.")
                        }
                    }
                    PairingButtons()
                    Label("La conexión es directa con tu PC y va cifrada con su propio certificado. El acceso se guarda en el llavero de este iPhone.",
                          systemImage: "lock")
                        .font(.footnote).foregroundStyle(RemoteTheme.muted)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(24)
            }
            .remoteCanvas()
            .toolbar(.hidden, for: .navigationBar)
        }
    }

    private func step(_ number: Int, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text("\(number)")
                .font(.footnote.weight(.semibold).monospacedDigit())
                .frame(width: 24, height: 24)
                .background(RemoteTheme.raised, in: Circle())
            Text(text).font(.subheadline).fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// "Emparejar otra PC" from Settings, the title menu or a revoked PC.
struct PairingSheet: View {
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 20) {
                Text("En tu PC abrí openADE → Ajustes → Remoto y escaneá el código que aparece.")
                    .font(.subheadline).foregroundStyle(RemoteTheme.muted)
                PairingButtons(onPaired: { dismiss() })
                Spacer()
            }
            .padding(24)
            .navigationTitle("Emparejar una PC")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cerrar") { dismiss() } } }
            .remoteCanvas()
        }
        .presentationDetents([.medium, .large])
    }
}

struct PairingButtons: View {
    var onPaired: (() -> Void)? = nil
    @State private var showingScanner = false
    @State private var scanned: String?
    @State private var payload: PairingPayload?
    @State private var error: String?

    var body: some View {
        VStack(spacing: 12) {
            Button("Escanear el código QR", systemImage: "qrcode.viewfinder") { showingScanner = true }
                .buttonStyle(RemotePrimaryButtonStyle())
            HStack {
                Text("¿Copiaste el enlace?").font(.subheadline).foregroundStyle(RemoteTheme.muted)
                Spacer()
                PasteButton(payloadType: String.self) { values in
                    if let value = values.first { read(value) }
                }
                .buttonBorderShape(.capsule)
                .labelStyle(.titleAndIcon)
            }
            if let error { WorkspaceErrorView(message: error) }
        }
        .sheet(isPresented: $showingScanner, onDismiss: {
            if let scanned { read(scanned); self.scanned = nil }
        }) {
            QRScannerView { result in
                switch result {
                case .success(let value): scanned = value
                case .failure(let failure): error = failure.localizedDescription
                }
            }
        }
        .sheet(item: $payload) { PairingReviewView(payload: $0, onPaired: onPaired).remoteSheet() }
    }

    private func read(_ code: String) {
        do { payload = try PairingPayload.parse(code); error = nil }
        catch { self.error = error.localizedDescription }
    }
}

/// What the person checks before this phone gets a token: which PC, and the
/// same security code its Settings page shows.
struct PairingReviewView: View {
    let payload: PairingPayload
    var onPaired: (() -> Void)? = nil
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var deviceName = UIDevice.current.name
    /// Kept across retries so the PC answers a repeated attempt the same way.
    @State private var requestID = UUID().uuidString
    @State private var error: String?

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    HStack(spacing: 14) {
                        Image(systemName: "desktopcomputer")
                            .font(.system(size: 26, weight: .light))
                            .frame(width: 56, height: 56)
                            .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 16))
                        VStack(alignment: .leading, spacing: 3) {
                            Text(payload.label).font(.title2.weight(.semibold))
                            Text("Quiere emparejarse con este iPhone").font(.subheadline).foregroundStyle(RemoteTheme.muted)
                        }
                    }
                    RemoteCard {
                        Text("Código de seguridad").font(.caption.weight(.semibold)).foregroundStyle(RemoteTheme.muted)
                        Text(payload.shortFingerprint)
                            .font(.system(.title2, design: .monospaced).weight(.semibold))
                            .textSelection(.enabled)
                            .accessibilityLabel("Código de seguridad \(payload.shortFingerprint.map(String.init).joined(separator: " "))")
                        Text("Tiene que coincidir con el que muestra openADE en Ajustes → Remoto. Si no coincide, no sigas.")
                            .font(.footnote).foregroundStyle(RemoteTheme.muted)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    RemoteCard {
                        Text("Direcciones de la PC").font(.caption.weight(.semibold)).foregroundStyle(RemoteTheme.muted)
                        ForEach(payload.routes, id: \.self) { route in
                            Label(RemoteRoute.display(route), systemImage: "network")
                                .font(.callout.monospaced()).lineLimit(1).truncationMode(.middle)
                        }
                    }
                    RemoteCard {
                        Text("Este iPhone").font(.caption.weight(.semibold)).foregroundStyle(RemoteTheme.muted)
                        TextField("Nombre", text: $deviceName).font(.body).disabled(store.pairing)
                    }
                    if let error { WorkspaceErrorView(message: error) }
                    Text("El código vence a las \(Date(timeIntervalSince1970: Double(payload.expiresAt)).spanishTime).")
                        .font(.footnote).foregroundStyle(RemoteTheme.muted)
                }
                .padding(20)
            }
            .safeAreaInset(edge: .bottom) {
                Button(action: pair) {
                    HStack(spacing: 10) {
                        if store.pairing { ProgressView().tint(RemoteTheme.onAccent) }
                        Text(store.pairing ? "Emparejando…" : "Emparejar")
                    }
                }
                .buttonStyle(RemotePrimaryButtonStyle())
                .disabled(store.pairing || deviceName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .padding(.horizontal, 20).padding(.vertical, 12)
                .background(RemoteTheme.surface)
            }
            .navigationTitle("Emparejar PC")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancelar") { dismiss() }.disabled(store.pairing) } }
            .interactiveDismissDisabled(store.pairing)
            .background(RemoteTheme.surface.ignoresSafeArea())
        }
    }

    private func pair() {
        let name = String(deviceName.trimmingCharacters(in: .whitespacesAndNewlines).prefix(64))
        error = nil
        Task {
            do {
                try await store.pair(payload, deviceName: name, requestID: requestID)
                Haptics.success()
                dismiss()
                onPaired?()
            } catch {
                self.error = RemoteStore.readable(error)
                Haptics.warning()
            }
        }
    }
}

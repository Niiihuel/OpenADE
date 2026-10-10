import SwiftUI

struct ConnectView: View {
    var isAdding = false
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var showingConnections = false
    @State private var address = ""
    @State private var token = ""
    @State private var showingScanner = false
    @State private var scannedCode: String?
    @State private var pairingPayload: PairingPayload?
    @FocusState private var focusedField: Field?
    private enum Field { case address, token }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    VStack(alignment: .leading, spacing: 16) {
                        Image(systemName: "laptopcomputer.and.iphone")
                            .font(.system(size: 36, weight: .light))
                            .foregroundStyle(RemoteTheme.ink)
                            .frame(width: 72, height: 72)
                            .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 24))
                            .accessibilityHidden(true)
                        Text("Tu trabajo, desde acá.")
                            .font(.largeTitle.weight(.semibold))
                        Text("Conectá tu PC para conversar con tus agentes, revisar cambios y aprobar sus acciones.")
                            .font(.body).foregroundStyle(RemoteTheme.muted)
                    }
                    .padding(.vertical, 16)

                    RemoteCard {
                        VStack(alignment: .leading, spacing: 16) {
                            Text("Conectá tu computadora").font(.headline)
                            Text("Generá un código de un solo uso desde openADE en tu PC.")
                                .font(.subheadline).foregroundStyle(RemoteTheme.muted)
                            Button("Escanear QR", systemImage: "qrcode.viewfinder") { showingScanner = true }
                                .buttonStyle(RemotePrimaryButtonStyle())
                            HStack {
                                Text("¿Tenés un enlace?").font(.subheadline).foregroundStyle(RemoteTheme.muted)
                                Spacer()
                                PasteButton(payloadType: String.self) { values in
                                    if let value = values.first { readPairing(value) }
                                }
                                .buttonBorderShape(.capsule)
                                .frame(minHeight: 44)
                            }
                            if let pending = store.library.pendingPairing {
                                Button("Reintentar emparejamiento pendiente", systemImage: "arrow.clockwise") { pairingPayload = pending.payload }
                                    .font(.subheadline).frame(minHeight: 44)
                            }
                        }
                    }

                    manualConnectionForm

                    if let notice = store.notice {
                        WorkspaceErrorView(message: notice)
                            .accessibilityLabel("No se pudo conectar. \(notice)")
                    }
                    Label("Tu PC debe estar encendida y conectada a tu red privada. El acceso se guarda en el llavero de este iPhone.", systemImage: "lock")
                        .font(.footnote).foregroundStyle(RemoteTheme.muted)
                        .fixedSize(horizontal: false, vertical: true)
                }.padding(24)
            }
            .remoteCanvas()
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    if isAdding { Button("Cancelar") { dismiss() } }
                    else if !store.computers.isEmpty {
                        Button("Computadoras", systemImage: "desktopcomputer") { showingConnections = true }
                    }
                }
            }
            .sheet(isPresented: $showingConnections) { ConnectionsView().remoteSheet() }
            .sheet(isPresented: $showingScanner, onDismiss: {
                if let scannedCode { readPairing(scannedCode); self.scannedCode = nil }
            }) {
                QRScannerView { result in
                    switch result {
                    case .success(let value): scannedCode = value
                    case .failure(let error): store.notice = error.localizedDescription
                    }
                }.remoteSheet()
            }
            .sheet(item: $pairingPayload) { PairingReviewView(payload: $0).remoteSheet() }
            .navigationTitle(isAdding ? "Agregar computadora" : "openADE")
            .navigationBarTitleDisplayMode(.inline)
            .scrollDismissesKeyboard(.interactively)
        }
    }

    private var manualConnectionForm: some View {
        RemoteCard {
            DisclosureGroup {
                VStack(spacing: 16) {
                    TextField("Nombre de la computadora (opcional)", text: $name)
                        .textContentType(.nickname)
                        .frame(minHeight: 44)
                    Divider()
                    TextField("https://tu-pc.tu-red.ts.net", text: $address)
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .focused($focusedField, equals: .address)
                        .submitLabel(.next)
                        .onSubmit { focusedField = .token }
                        .accessibilityLabel("Dirección HTTPS de la PC")
                        .frame(minHeight: 44)
                    Divider()
                    SecureField("Token de conexión", text: $token)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .focused($focusedField, equals: .token)
                        .submitLabel(.go)
                        .onSubmit { connect() }
                        .privacySensitive()
                        .frame(minHeight: 44)
                    Button(action: connect) {
                        HStack {
                            if store.isConnecting { ProgressView().tint(RemoteTheme.onAccent).padding(.trailing, 6) }
                            Text(store.isConnecting ? "Conectando…" : "Conectar PC")
                        }
                    }
                    .buttonStyle(RemotePrimaryButtonStyle())
                    .disabled(store.isConnecting || address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || token.isEmpty)
                    .accessibilityHint("Verifica la PC y guarda el acceso en el llavero de este iPhone")
                }.padding(.top, 16)
            } label: {
                Label("Usar dirección y token", systemImage: "link")
                    .font(.subheadline.weight(.medium)).frame(minHeight: 44)
            }
        }
    }

    private func readPairing(_ code: String) {
        do { pairingPayload = try PairingPayload.parse(code); store.notice = nil }
        catch { store.notice = error.localizedDescription }
    }

    private func connect() {
        guard !address.isEmpty, !token.isEmpty else { return }
        focusedField = nil
        Task {
            if await store.connect(address: address, token: token, name: name) {
                token = ""
                if isAdding { dismiss() }
            }
        }
    }
}

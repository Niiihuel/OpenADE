import SwiftUI

struct MessageQueueView: View {
    let sessionID: String
    @Environment(RemoteStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            List {
                Section {
                    Text("Los mensajes quedan guardados en este iPhone. Elegí cuándo enviarlos; si el agente está trabajando, se enviarán como una indicación al turno actual.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                .listRowBackground(Color.clear)
                ForEach(store.queuedPrompts.filter { $0.sessionID == sessionID }) { queued in
                    Section {
                        Text(queued.text).textSelection(.enabled)
                        HStack {
                            Text(queued.mode.title).font(.caption).foregroundStyle(.secondary)
                            Spacer()
                            Button("Enviar ahora", systemImage: "arrow.up.circle") { Task { await store.sendQueuedPrompt(queued) } }
                                .frame(minHeight: 44)
                                .disabled(store.phase != .connected || store.pendingTurn(sessionID: sessionID) != nil
                                          || store.actionIsBusy("sendTurn:\(sessionID)") || store.actionIsBusy("steerTurn:\(sessionID)")
                                          || (store.session(id: sessionID)?.isBusy == true
                                              ? store.session(id: sessionID)?.canControlActive == false : store.session(id: sessionID)?.canStartTurn == false))
                        }
                        Button("Eliminar de la cola", role: .destructive) { store.removeQueuedPrompt(id: queued.id) }
                            .frame(minHeight: 44)
                    }
                    .listRowBackground(RemoteTheme.surface)
                }
                if !store.queuedPrompts.contains(where: { $0.sessionID == sessionID }) {
                    RemoteEmptyState(symbol: "tray", title: "Cola vacía", message: "Podés guardar un mensaje mientras el agente está trabajando.")
                        .listRowBackground(Color.clear)
                }
            }
            .scrollContentBackground(.hidden)
            .remoteCanvas()
            .navigationTitle("Mensajes en cola")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Listo") { dismiss() } } }
        }
    }
}

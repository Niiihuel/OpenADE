import SwiftUI

/// What the task waits on, above the composer: a permission or a question.
/// The first answer wins, from the PC or from here.
struct PendingCard: View {
    let taskID: String
    let requests: [PendingRequest]

    var body: some View {
        if let request = requests.first {
            let position = requests.count > 1 ? "1 de \(requests.count)" : nil
            Group {
                if request.isQuestion { QuestionCard(taskID: taskID, request: request, position: position) }
                else { PermissionCard(taskID: taskID, request: request, position: position) }
            }
            .padding(16)
            .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 26, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 26, style: .continuous)
                    .strokeBorder(RemoteTheme.attention.opacity(0.45), lineWidth: 1)
            }
            .shadow(color: .black.opacity(0.12), radius: 16, y: 4)
            .id(request.id)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }
}

private struct CardHeader: View {
    let title: String
    let symbol: String
    let position: String?
    var body: some View {
        HStack {
            Label(title, systemImage: symbol)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(RemoteTheme.attention)
            Spacer()
            if let position { Text(position).font(.caption.monospacedDigit()).foregroundStyle(RemoteTheme.muted) }
        }
        .accessibilityElement(children: .combine)
    }
}

struct PermissionCard: View {
    let taskID: String
    let request: PendingRequest
    let position: String?
    @Environment(RemoteStore.self) private var store
    @State private var showingArguments = false

    private var summary: PermissionSummary { PermissionSummary(request) }
    private var busy: Bool { store.isBusy("pending:\(request.id)") }
    private var disabled: Bool { busy || !store.isOnline }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            CardHeader(title: "Necesita tu permiso", symbol: "hand.raised.fill", position: position)
            Text(request.toolName).font(.footnote.monospaced()).foregroundStyle(RemoteTheme.muted)
            details
            if request.argumentsTruncated {
                // The PC accepts only a refusal for what the phone cannot show.
                Label("Es demasiado grande para mostrarla acá. Revisala en la PC, o rechazala.", systemImage: "exclamationmark.triangle")
                    .font(.caption).foregroundStyle(RemoteTheme.attention)
                Button("Rechazar", role: .destructive) { decide(.deny) }
                    .buttonStyle(RemoteSecondaryButtonStyle(role: .destructive))
                    .disabled(disabled)
            } else {
                VStack(spacing: 8) {
                    HStack(spacing: 8) {
                        Button { decide(.allow) } label: {
                            HStack(spacing: 8) {
                                if busy { ProgressView().tint(RemoteTheme.onAccent) }
                                Text("Permitir una vez")
                            }
                        }
                        .buttonStyle(RemotePrimaryButtonStyle())
                        Button("Rechazar", role: .destructive) { decide(.deny) }
                            .buttonStyle(RemoteSecondaryButtonStyle(role: .destructive))
                    }
                    if let always = request.always {
                        // The exact prefix the PC will keep allowing, shown whole.
                        Button { decide(.allowAlways) } label: {
                            (Text("Permitir siempre ") + Text(always).font(.system(.body, design: .monospaced)))
                                .multilineTextAlignment(.center)
                                .padding(.horizontal, 12).padding(.vertical, 4)
                        }
                        .buttonStyle(RemoteSecondaryButtonStyle())
                        .accessibilityLabel("Permitir siempre los comandos que empiezan con \(always)")
                    } else if ToolKind(toolName: request.toolName) == .run {
                        // The session grant covers every later command of the task, not this one.
                        Button("Permitir todos los comandos en esta tarea") { decide(.allowSession) }
                            .buttonStyle(RemoteSecondaryButtonStyle())
                    } else {
                        Button("Permitir en esta tarea") { decide(.allowSession) }
                            .buttonStyle(RemoteSecondaryButtonStyle())
                            .accessibilityHint("Permite esta herramienta hasta que termine la tarea")
                    }
                }
                .disabled(disabled)
                .padding(.top, 2)
            }
        }
    }

    /// The first lines of a long text, and how many more there are.
    private func clipped(_ text: String, _ limit: Int) -> (text: String, more: Int) {
        let lines = text.components(separatedBy: "\n")
        return (lines.prefix(limit).joined(separator: "\n"), max(0, lines.count - limit))
    }

    private func codeBlock(_ text: String, _ limit: Int) -> some View {
        let shown = clipped(text, limit)
        return VStack(alignment: .leading, spacing: 4) {
            ScrollView(.horizontal, showsIndicators: false) {
                Text(shown.text)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(12)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 14))
            if shown.more > 0 {
                Text(shown.more == 1 ? "Sigue 1 línea más" : "Siguen \(shown.more) líneas más")
                    .font(.caption).foregroundStyle(RemoteTheme.attention)
            }
        }
    }

    /// The summary, every other argument, and always the whole request.
    @ViewBuilder private var details: some View {
        VStack(alignment: .leading, spacing: 8) {
            switch summary.style {
            case .command:
                codeBlock("$ " + (summary.lines.first ?? ""), 12)
                    .accessibilityLabel("Comando: \(summary.lines.first ?? "")")
                if let detail = summary.detail {
                    Text(detail).font(.caption).foregroundStyle(RemoteTheme.muted).lineLimit(2).truncationMode(.middle)
                }
            case .paths:
                ForEach(summary.lines.prefix(8), id: \.self) { path in
                    Label(path, systemImage: "doc.text")
                        .font(.callout.monospaced()).lineLimit(2).truncationMode(.middle)
                }
                if summary.lines.count > 8 {
                    Text("y \(summary.lines.count - 8) archivos más").font(.caption).foregroundStyle(RemoteTheme.attention)
                }
            case .json:
                codeBlock(summary.lines.first ?? "", 16)
            }
            ForEach(summary.extras, id: \.self) { extra in
                Text(extra)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(RemoteTheme.attention)
                    .lineLimit(4)
                    .textSelection(.enabled)
            }
            Button("Ver todos los argumentos", systemImage: "doc.text.magnifyingglass") { showingArguments = true }
                .font(.subheadline)
                .frame(minHeight: 36)
                .tint(RemoteTheme.muted)
        }
        .sheet(isPresented: $showingArguments) {
            ReadOnlyOutputView(title: request.toolName, output: request.arguments.displayText).remoteSheet()
        }
    }

    private func decide(_ decision: PermissionDecision) {
        Task {
            if await store.resolve(request, task: taskID, decision: decision) {
                decision == .deny ? Haptics.warning() : Haptics.success()
            }
        }
    }
}

/// An `ask_user` request, one question at a time. Options come recommended
/// first; the person may also write their own answer.
struct QuestionCard: View {
    let taskID: String
    let request: PendingRequest
    let position: String?
    @Environment(RemoteStore.self) private var store
    @State private var index = 0
    @State private var selected: [String: [String]] = [:]
    @State private var other: [String: String] = [:]

    private var questions: [PendingQuestion] { request.questions }
    private var busy: Bool { store.isBusy("pending:\(request.id)") }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            CardHeader(title: "Te hizo una pregunta", symbol: "questionmark.bubble.fill",
                       position: questions.count > 1 ? "\(index + 1)/\(questions.count)" : position)
            if questions.indices.contains(index) {
                questionView(questions[index])
            } else {
                Text("La pregunta no se pudo leer. Respondela en la PC.").font(.callout).foregroundStyle(RemoteTheme.muted)
            }
            HStack(spacing: 8) {
                Button(isLast ? "Omitir" : "Saltear") { skip() }
                    .buttonStyle(RemoteSecondaryButtonStyle())
                Button { advance() } label: {
                    HStack(spacing: 8) {
                        if busy { ProgressView().tint(RemoteTheme.onAccent) }
                        Text(isLast ? "Responder" : "Siguiente")
                    }
                }
                .buttonStyle(RemotePrimaryButtonStyle())
            }
            .disabled(busy || !store.isOnline)
        }
    }

    private var isLast: Bool { index >= questions.count - 1 }

    private func questionView(_ question: PendingQuestion) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(question.header.uppercased()).font(.caption2.weight(.semibold)).foregroundStyle(RemoteTheme.muted)
            Text(question.question).font(.headline).fixedSize(horizontal: false, vertical: true)
            ForEach(Array(question.options.enumerated()), id: \.element.label) { offset, option in
                optionRow(question, option, recommended: offset == 0)
            }
            TextField(question.options.isEmpty ? "Tu respuesta" : "Otra respuesta (opcional)", text: Binding(
                get: { other[question.header] ?? "" },
                set: { other[question.header] = $0 }
            ), axis: .vertical)
            .lineLimit(1...4)
            .padding(.horizontal, 12).padding(.vertical, 10)
            .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 14))
        }
    }

    private func optionRow(_ question: PendingQuestion, _ option: PendingQuestion.Option, recommended: Bool) -> some View {
        let isSelected = selected[question.header]?.contains(option.label) == true
        let symbol = question.multiSelect ? (isSelected ? "checkmark.square.fill" : "square") : (isSelected ? "largecircle.fill.circle" : "circle")
        return Button { toggle(option.label, in: question) } label: {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: symbol).foregroundStyle(isSelected ? RemoteTheme.ink : RemoteTheme.muted)
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(option.label).font(.body)
                        if recommended { RemoteTag(title: "Recomendada") }
                    }
                    if let description = option.description, !description.isEmpty {
                        Text(description).font(.caption).foregroundStyle(RemoteTheme.muted).fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12).padding(.vertical, 10)
            .background(isSelected ? RemoteTheme.raised : Color.clear, in: RoundedRectangle(cornerRadius: 14))
            .overlay { RoundedRectangle(cornerRadius: 14).strokeBorder(RemoteTheme.border, lineWidth: isSelected ? 0 : 0.75) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isSelected ? [.isSelected] : [])
    }

    private func toggle(_ label: String, in question: PendingQuestion) {
        var current = selected[question.header] ?? []
        if current.contains(label) { current.removeAll { $0 == label } }
        else if question.multiSelect { current.append(label) }
        else { current = [label] }
        selected[question.header] = current
    }

    private func skip() {
        guard questions.indices.contains(index) else { send(); return }
        selected[questions[index].header] = nil
        other[questions[index].header] = nil
        if isLast { send() } else { index += 1 }
    }

    private func advance() {
        if isLast { send() } else { index += 1 }
    }

    private func send() {
        let answer = QuestionAnswer.json(questions: questions, selected: selected, other: other)
        Task { if await store.answer(request, task: taskID, answer: answer) { Haptics.success() } }
    }
}

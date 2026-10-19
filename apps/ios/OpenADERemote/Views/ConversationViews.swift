import SwiftUI

/// One conversation row. Equatable, so a streaming delta re-renders only the
/// row whose text changed.
struct ConversationItemView: View, Equatable {
    let item: ConversationItem

    var body: some View {
        switch item {
        case .user(let entry): UserBubble(entry: entry)
        case .assistant(let entry): AssistantMessage(entry: entry)
        case .notice(let entry): NoticeRow(entry: entry)
        case .tools(let group): ToolGroupRow(group: group)
        case .work(let work): TurnWorkRow(work: work)
        }
    }
}

private struct UserBubble: View {
    let entry: ConversationEntry
    var body: some View {
        HStack {
            Spacer(minLength: 48)
            Text(entry.text)
                .font(.body)
                .textSelection(.enabled)
                .padding(.horizontal, 16).padding(.vertical, 11)
                .background(RemoteTheme.raised, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
                .accessibilityLabel("Vos: \(entry.text)")
        }
    }
}

private struct AssistantMessage: View {
    let entry: ConversationEntry
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if entry.textTruncated {
                Text("Vista recortada. El texto completo sigue en la PC.").font(.caption).foregroundStyle(RemoteTheme.muted)
            }
            if entry.text.isEmpty && entry.isRunning {
                TypingDots()
            } else {
                MarkdownContentView(text: entry.text)
            }
        }
    }
}

private struct NoticeRow: View {
    let entry: ConversationEntry
    var body: some View {
        Label(entry.text, systemImage: entry.isError ? "exclamationmark.triangle" : "info.circle")
            .font(.footnote)
            .foregroundStyle(entry.isError ? Color.red : RemoteTheme.muted)
            .padding(.horizontal, 14).padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background((entry.isError ? Color.red.opacity(0.08) : RemoteTheme.surface), in: RoundedRectangle(cornerRadius: 16))
    }
}

/// Consecutive tool calls in one compact row: what they did by kind, a
/// spinner while one runs, a red mark when one failed.
struct ToolGroupRow: View {
    let group: ToolGroup
    @State private var expanded: Bool

    init(group: ToolGroup) {
        self.group = group
        #if DEBUG
        _expanded = State(initialValue: DebugUIFixtures.mode == "tools")
        #else
        _expanded = State(initialValue: false)
        #endif
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button { withAnimation(.snappy(duration: 0.2)) { expanded.toggle() } } label: {
                HStack(spacing: 10) {
                    Group {
                        if group.isRunning { ProgressView().controlSize(.small) }
                        else if group.failures > 0 { Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.red) }
                        else { Image(systemName: group.calls.count == 1 ? group.calls[0].kind.symbol : "square.stack.3d.up") }
                    }
                    .frame(width: 20)
                    Text(group.isRunning ? runningTitle : group.summary)
                        .font(.subheadline)
                        .lineLimit(2)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.semibold))
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                }
                .foregroundStyle(RemoteTheme.muted)
                .frame(minHeight: 40)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(group.summary)\(group.isRunning ? ", en curso" : "")\(group.failures > 0 ? ", con errores" : "")")
            .accessibilityHint(expanded ? "Oculta cada acción" : "Muestra cada acción")
            if expanded {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(group.calls) { ToolCallRow(call: $0) }
                }
                .padding(.leading, 30)
                .padding(.bottom, 6)
            }
        }
    }

    private var runningTitle: String {
        guard let live = group.calls.last(where: \.isRunning) else { return group.summary }
        let done = group.calls.filter { !$0.isRunning && $0.kind != .followUp }
        return done.isEmpty ? live.label : "\(ToolKind.summary(done)) · \(live.label)"
    }
}

private struct ToolCallRow: View {
    let call: ToolCallItem
    @State private var expanded = false
    @State private var showingOutput = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button { if !call.output.isEmpty || call.arguments != nil { expanded.toggle() } } label: {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Image(systemName: call.kind.symbol).font(.caption).frame(width: 16)
                    Text(call.label)
                        .font(call.kind == .run ? .system(.footnote, design: .monospaced) : .footnote)
                        .lineLimit(expanded ? nil : 1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    if call.isRunning { ProgressView().controlSize(.mini) }
                    else if call.isError { Image(systemName: "xmark.circle.fill").font(.caption).foregroundStyle(.red) }
                }
                .foregroundStyle(call.isError ? Color.red : RemoteTheme.ink.opacity(0.85))
                .frame(minHeight: 32)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(call.label)\(call.isRunning ? ", en curso" : call.isError ? ", falló" : "")")
            if expanded {
                VStack(alignment: .leading, spacing: 8) {
                    if let arguments = call.arguments, arguments != .object([:]) {
                        OutputBlock(text: String(arguments.displayText.prefix(2_000)))
                    }
                    if !call.output.isEmpty {
                        OutputBlock(text: String(call.output.suffix(4_000)))
                        if call.output.count > 4_000 {
                            Button("Ver la salida completa", systemImage: "doc.text.magnifyingglass") { showingOutput = true }
                                .font(.caption).frame(minHeight: 36)
                        }
                    } else if call.isRunning {
                        Text("Esperando salida…").font(.caption).foregroundStyle(RemoteTheme.muted)
                    }
                }
                .padding(.leading, 24)
            }
        }
        .sheet(isPresented: $showingOutput) {
            ReadOnlyOutputView(title: call.label, output: call.output).remoteSheet()
        }
    }
}

private struct OutputBlock: View {
    let text: String
    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(text)
                .lineLimit(60)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(RemoteTheme.ink.opacity(0.85))
                .textSelection(.enabled)
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(RemoteTheme.surface, in: RoundedRectangle(cornerRadius: 12))
        .overlay { RoundedRectangle(cornerRadius: 12).strokeBorder(RemoteTheme.border.opacity(0.6), lineWidth: 0.5) }
    }
}

/// A finished turn's steps behind one row: "Trabajó 2 min · Leyó 4 archivos".
struct TurnWorkRow: View {
    let work: TurnWork
    @State private var expanded: Bool

    init(work: TurnWork) {
        self.work = work
        #if DEBUG
        _expanded = State(initialValue: DebugUIFixtures.mode == "tools")
        #else
        _expanded = State(initialValue: false)
        #endif
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Button { withAnimation(.snappy(duration: 0.22)) { expanded.toggle() } } label: {
                HStack(spacing: 8) {
                    Text(work.title).font(.subheadline.weight(.medium)).foregroundStyle(RemoteTheme.ink.opacity(0.8))
                    if let summary = work.summary {
                        Text(summary).font(.subheadline).foregroundStyle(RemoteTheme.muted).lineLimit(2)
                    }
                    if work.failures > 0 { Image(systemName: "exclamationmark.circle.fill").font(.caption).foregroundStyle(.red) }
                    Spacer(minLength: 4)
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.semibold)).foregroundStyle(RemoteTheme.muted)
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                }
                .frame(minHeight: 36)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel([work.title, work.summary].compactMap { $0 }.joined(separator: ", "))
            .accessibilityHint(expanded ? "Oculta los pasos" : "Muestra los pasos del turno")
            if expanded {
                VStack(alignment: .leading, spacing: 14) {
                    ForEach(work.items) { ConversationItemView(item: $0) }
                }
                .padding(.leading, 14)
                .overlay(alignment: .leading) { Rectangle().fill(RemoteTheme.border).frame(width: 1.5) }
            }
        }
    }
}

/// Three dots while the agent writes its first words.
struct TypingDots: View {
    @State private var phase = 0.0
    var body: some View {
        HStack(spacing: 5) {
            ForEach(0..<3) { index in
                Circle().fill(RemoteTheme.muted).frame(width: 7, height: 7)
                    .opacity(0.35 + 0.65 * max(0, sin(phase - Double(index) * 0.8)))
            }
        }
        .frame(height: 22)
        .onAppear { withAnimation(.linear(duration: 1.2).repeatForever(autoreverses: false)) { phase = .pi * 2 } }
        .accessibilityLabel("Escribiendo")
    }
}

/// "Trabajando · 1:12" below the conversation while a turn runs.
struct WorkingIndicator: View {
    let since: Date?
    let waiting: Bool
    var body: some View {
        HStack(spacing: 8) {
            if waiting {
                Image(systemName: "hand.raised.fill").foregroundStyle(RemoteTheme.attention)
                Text("Esperando tu respuesta").foregroundStyle(RemoteTheme.attention)
            } else {
                ProgressView().controlSize(.small)
                Text("Trabajando")
                if let since { Text(since, style: .timer).monospacedDigit() }
            }
        }
        .font(.footnote.weight(.medium))
        .foregroundStyle(RemoteTheme.muted)
        .accessibilityElement(children: .combine)
    }
}

struct ReadOnlyOutputView: View {
    let title: String
    let output: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ScrollView([.horizontal, .vertical]) {
                Text(output.isEmpty ? "Sin salida de texto." : output)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .padding()
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Listo") { dismiss() } } }
            .remoteCanvas()
        }
    }
}

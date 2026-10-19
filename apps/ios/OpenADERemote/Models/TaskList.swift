import Foundation

struct TaskSection: Identifiable, Equatable {
    let id: String
    let title: String
    var tasks: [RemoteTask]
}

/// The list as the PC's sidebar arranges it: pinned tasks first, then one
/// section per project in the order its tasks appear. The PC's order is kept.
enum TaskListLayout {
    static func sections(tasks: [RemoteTask], projects: [RemoteProject], query: String = "") -> [TaskSection] {
        let names = Dictionary(projects.map { ($0.id, $0.name) }, uniquingKeysWith: { first, _ in first })
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let visible = needle.isEmpty ? tasks : tasks.filter { task in
            task.title.localizedStandardContains(needle) || task.agent.localizedStandardContains(needle)
                || (names[task.project] ?? "").localizedStandardContains(needle)
        }
        var sections: [TaskSection] = []
        let pinned = visible.filter(\.pinned)
        if !pinned.isEmpty { sections.append(TaskSection(id: "pinned", title: "Fijadas", tasks: pinned)) }
        var order: [String] = []
        var grouped: [String: [RemoteTask]] = [:]
        for task in visible where !task.pinned {
            if grouped[task.project] == nil { order.append(task.project) }
            grouped[task.project, default: []].append(task)
        }
        for project in order {
            sections.append(TaskSection(id: "project-\(project)", title: names[project] ?? "Otro proyecto",
                                        tasks: grouped[project] ?? []))
        }
        return sections
    }
}

use super::*;
use serde_json::json;

/// A frame from a compact description of accessibility nodes:
/// `(id, parent, role, name, backend)` plus per-node extras set afterwards.
fn frame(nodes: &[(&str, Option<&str>, &str, &str, Option<i64>)]) -> Frame {
    let mut ax: Vec<AxNode> = nodes
        .iter()
        .map(|(id, parent, role, name, backend)| AxNode {
            node_id: (*id).to_owned(),
            parent_id: parent.map(str::to_owned),
            role: Some(AxValue { value: json!(role) }),
            name: Some(AxValue { value: json!(name) }),
            backend: *backend,
            ..AxNode::default()
        })
        .collect();
    let mut children: HashMap<&str, Vec<String>> = HashMap::new();
    for (id, parent, ..) in nodes {
        if let Some(parent) = parent {
            children.entry(parent).or_default().push((*id).to_owned());
        }
    }
    for node in &mut ax {
        node.child_ids = children.remove(node.node_id.as_str()).unwrap_or_default();
    }
    Frame {
        ax,
        ..Frame::default()
    }
}

fn node<'a>(frame: &'a mut Frame, id: &str) -> &'a mut AxNode {
    frame.ax.iter_mut().find(|node| node.node_id == id).unwrap()
}

fn property(frame: &mut Frame, id: &str, name: &str, value: Value) {
    node(frame, id).properties.push(AxProperty {
        name: name.to_owned(),
        value: AxValue { value },
    });
}

fn ignore(frame: &mut Frame, id: &str, reason: &str) {
    let node = node(frame, id);
    node.ignored = true;
    node.ignored_reasons.push(AxProperty {
        name: reason.to_owned(),
        value: AxValue::default(),
    });
}

fn element<'a>(frame: &'a mut Frame, backend: i64, tag: &str) -> &'a mut DomNode {
    frame.dom.entry(backend).or_insert_with(|| DomNode {
        tag: tag.to_owned(),
        visible: true,
        ..DomNode::default()
    })
}

/// Renders the whole page. Frames built by hand all belong to one document.
fn text(frames: &[Frame], refs: &mut RefTable) -> String {
    if refs.documents.is_empty() {
        refs.document(&DocumentKey::default());
    }
    render(frames, None, &SnapshotOptions::default(), refs).unwrap()
}

/// The frames of a recorded page, registered in `refs` as a snapshot does,
/// and the text Chrome gave for them.
fn recorded(page: &str, refs: &mut RefTable) -> (Vec<Frame>, String) {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recorded");
    let inputs = std::fs::read_to_string(directory.join(format!("{page}.json"))).unwrap();
    let expected = std::fs::read_to_string(directory.join(format!("{page}.yaml"))).unwrap();
    let recording: Recorded = serde_json::from_str(&inputs).unwrap();
    (recording.into_frames(refs), expected)
}

#[test]
fn recorded_pages_render_to_the_text_chrome_gave() {
    for page in ["tools", "form", "lists", "table", "frames", "custom"] {
        let mut refs = RefTable::default();
        let (frames, expected) = recorded(page, &mut refs);
        let rendered = text(&frames, &mut refs);
        assert_eq!(rendered, expected.trim_end_matches('\n'), "{page}");
        // The same page again: every element keeps its ref.
        let frames = recorded(page, &mut refs).0;
        assert_eq!(text(&frames, &mut refs), rendered, "{page} is stable");
    }
}

#[test]
fn the_recorded_shapes_show_what_each_part_of_the_format_is_for() {
    let page = |name: &str| {
        let mut refs = RefTable::default();
        let frames = recorded(name, &mut refs).0;
        text(&frames, &mut refs)
    };
    let tools = page("tools");
    assert!(tools.starts_with("- generic [active] [ref=e1]:\n  - heading \"Herramientas\" [level=1] [ref=e2]\n"));
    assert_eq!(tools.matches("- button \"Borrar\" [ref=e").count(), 2, "two elements, one name, two refs");
    assert!(tools.contains("  - combobox \"Color\" [ref=e8]:\n    - option \"Rojo\" [selected]\n    - option \"Verde\"\n"));
    assert!(tools.contains("  - generic [ref=e9]:\n    - checkbox \"Acepto\" [ref=e10]\n    - text: Acepto\n"));
    assert!(tools.contains("  - iframe \"Marco\" [ref=e17]:\n    - button \"Dentro\" [ref=f1e1]"), "{tools}");
    assert!(!tools.contains("Texto oculto"));

    let form = page("form");
    assert!(form.contains("textbox \"Nombre\" [ref=e5]:\n        - /placeholder: Nombre y apellido\n"), "{form}");
    assert!(form.contains(": •••••••\n") && !form.contains("secreto"), "a password is never in the tree");
    assert!(form.contains("- spinbutton \"Edad\" [ref=e13]: \"36\"\n"), "a number is quoted as text: {form}");
    assert!(form.contains("- radio \"Equipo\" [disabled] [ref="));
    assert!(form.contains("- button \"Negrita\" [pressed] [ref="));
    assert!(form.contains("- link \"Ayuda\" [ref=e40] [cursor=pointer]:\n      - /url: /ayuda?tema=alta\n"), "{form}");
    assert!(form.ends_with("- text: Sin destino"));

    let custom = page("custom");
    assert!(custom.contains("- generic [ref=e3] [cursor=pointer]: Tarjeta uno\n"));
    assert!(custom.contains("- generic: Tarjeta inerte\n"), "no pointer events, no ref");
    assert!(custom.contains("- checkbox \"Tres estados\" [checked=mixed] [ref="));
    assert!(custom.contains("- tab \"General\" [selected] [ref="));
    assert!(custom.contains("- img \"Icono\" [ref=") && custom.contains("] [cursor=pointer]\n  - paragraph"));
    assert!(custom.contains("- button \"Muy anidado\" [ref="), "wrappers that add nothing are gone");
    assert!(!custom.contains("Invisible") && !custom.contains("aria-hidden"));

    let frames = page("frames");
    assert_eq!(frames.matches("[ref=f").count(), 8, "{frames}");
    assert!(frames.contains("  - iframe \"Anidado\" [ref=e5]:\n    - generic [ref=f2e1]:\n      - heading \"Marco intermedio\" [level=2] [ref=f2e2]\n      - iframe \"Interior\" [ref=f2e3]:\n        - button \"Dentro\" [ref=f"));
    assert!(!frames.contains("Oculto"), "a frame that is not displayed is not descended into");

    let table = page("table");
    assert!(table.contains("  - table [ref=e3]:\n    - caption [ref=e4]: Pedidos de octubre\n    - rowgroup [ref=e5]:\n      - row [ref=e6]:\n        - columnheader \"N.º\" [ref=e7]\n"));
    assert!(table.contains("        - cell [ref=e16]:\n          - button \"Ver\" [ref=e17]\n          - button \"Anular\" [ref=e18]\n"));
    assert!(table.contains("- gridcell \"2\" [selected] [ref="));
    assert!(table.ends_with("  - text: Maquetado\n  - link \"Enlace en tabla de maquetado\" [ref=e42] [cursor=pointer]:\n    - /url: /x"));
}

#[test]
fn options_narrow_a_recorded_page_and_leave_its_refs_alone() {
    let mut refs = RefTable::default();
    let frames = recorded("form", &mut refs).0;
    let whole = text(&frames, &mut refs);
    let frames = recorded("form", &mut refs).0;
    let interactive = SnapshotOptions {
        interactive_only: true,
        ..SnapshotOptions::default()
    };
    let narrowed = render(&frames, None, &interactive, &mut refs).unwrap();
    assert!(narrowed.len() * 4 < whole.len() * 3, "{} of {}", narrowed.len(), whole.len());
    assert!(narrowed.starts_with("- form \"Cliente\" [ref=e3]:\n  - textbox \"Nombre\" [ref=e5]:\n    - /placeholder: Nombre y apellido\n  - textbox \"Correo\" [ref=e7]: ada@example.test\n"), "{narrowed}");
    assert!(!narrowed.contains("generic") && !narrowed.contains("- text:") && !narrowed.contains("heading"));
    assert!(narrowed.contains("  - group \"Plan\" [ref=e16]:\n    - radio \"Gratis\" [checked] [ref=e18]\n"), "{narrowed}");
    for line in narrowed.lines().filter(|line| line.contains("[ref=")) {
        assert!(whole.contains(line.trim_start()), "{line} is the line of the whole page");
    }
    // What the narrowed snapshot left out still resolves afterwards.
    let heading = whole.lines().find(|line| line.contains("heading")).unwrap();
    let reference = heading[heading.find("[ref=").unwrap() + 5..heading.len() - 1].to_owned();
    assert!(refs.resolve(&reference).is_some());

    let frames = recorded("form", &mut refs).0;
    let shallow = SnapshotOptions {
        max_depth: Some(1),
        ..SnapshotOptions::default()
    };
    assert_eq!(
        render(&frames, None, &shallow, &mut refs).unwrap(),
        "- main [ref=e1]:\n  - heading \"Alta de cliente\" [level=1] [ref=e2]\n  - form \"Cliente\" [ref=e3]"
    );
    // A subtree, by the backend node of its root.
    let frames = recorded("form", &mut refs).0;
    let fieldset = frames[0].ax.iter().find(|node| node.name() == "Plan" && node.role() == "group").unwrap().backend;
    let subtree = render(&frames, fieldset, &SnapshotOptions::default(), &mut refs).unwrap();
    assert!(subtree.starts_with("- group \"Plan\" [ref=e16]:\n  - generic [ref=e17]:\n    - radio \"Gratis\" [checked] [ref=e18]\n    - text: Gratis\n"), "{subtree}");
    assert_eq!(subtree.lines().count(), 10);
    assert_eq!(render(&frames, Some(-5), &SnapshotOptions::default(), &mut refs), None, "a node that is not in the tree");
}

#[test]
fn a_ref_follows_its_element_and_goes_stale_with_it() {
    let page = |extra: bool, second: &str| {
        let mut nodes = vec![
            ("1", None, "RootWebArea", "", Some(1)),
            ("2", Some("1"), "button", "Guardar", Some(10)),
            ("3", Some("1"), "button", second, Some(11)),
        ];
        if extra {
            nodes.push(("4", Some("1"), "link", "Efímero", Some(12)));
        }
        frame(&nodes)
    };
    let key = |loader: &str| DocumentKey {
        session: "S".into(),
        frame: "F".into(),
        loader: loader.into(),
    };
    let mut refs = RefTable::default();
    let snapshot = |frame: Frame, refs: &mut RefTable, loader: &str, options: &SnapshotOptions| {
        let frame = Frame {
            document: refs.document(&key(loader)),
            ..frame
        };
        render(&[frame], None, options, refs).unwrap()
    };
    let whole = SnapshotOptions::default();
    assert_eq!(
        snapshot(page(true, "Borrar"), &mut refs, "L1", &whole),
        "- button \"Guardar\" [ref=e1]\n- button \"Borrar\" [ref=e2]\n- link \"Efímero\" [ref=e3]"
    );
    assert_eq!(refs.resolve("e2"), Some((&key("L1"), 11)));
    // The link left the page; the second button was renamed.
    assert_eq!(
        snapshot(page(false, "Eliminar"), &mut refs, "L1", &whole),
        "- button \"Guardar\" [ref=e1]\n- button \"Eliminar\" [ref=e4]"
    );
    assert_eq!(refs.resolve("e1"), Some((&key("L1"), 10)), "an unchanged element keeps its ref");
    assert_eq!(refs.resolve("e2"), None, "a renamed element is a new one");
    assert_eq!(refs.resolve("e3"), None, "what the latest snapshot does not list is stale");
    assert_eq!(refs.resolve("e4"), Some((&key("L1"), 11)));
    // A narrowed snapshot adds refs and takes none away.
    let narrowed = SnapshotOptions {
        max_depth: Some(0),
        interactive_only: true,
        ..SnapshotOptions::default()
    };
    snapshot(frame(&[("1", None, "RootWebArea", "", Some(1)), ("2", Some("1"), "button", "Guardar", Some(10))]), &mut refs, "L1", &narrowed);
    assert!(refs.resolve("e1").is_some() && refs.resolve("e4").is_some());

    // A new document in the same frame: nothing of the old one resolves, and
    // its refs can never be mistaken for the new ones.
    assert_eq!(snapshot(page(false, "Eliminar"), &mut refs, "L2", &whole), "- button \"Guardar\" [ref=f1e1]\n- button \"Eliminar\" [ref=f1e2]");
    assert_eq!(refs.resolve("e1"), None);
    assert_eq!(refs.resolve("e4"), None);
    assert_eq!(refs.resolve("f1e2"), Some((&key("L2"), 11)));
    // Going back to a document seen before starts over as well: it is a third load.
    assert!(snapshot(page(false, "Eliminar"), &mut refs, "L3", &whole).contains("[ref=f2e1]"));

    for malformed in ["", "e", "f1", "e99999", "f9e1", "f1e", "x1", "e-1", "e1e", "ée1"] {
        assert_eq!(refs.resolve(malformed), None, "{malformed:?}");
    }
    refs.retain(|document| document.loader != "L3");
    assert_eq!(refs.resolve("f2e1"), None);
}

#[test]
fn names_that_only_repeat_what_is_listed_are_left_out() {
    let mut page = frame(&[
        ("1", None, "RootWebArea", "", Some(1)),
        // A name that is its content: said once.
        ("2", Some("1"), "button", "Guardar", Some(2)),
        ("3", Some("2"), "StaticText", "Guardar", Some(3)),
        // A name computed from children that are listed: the children say it.
        ("4", Some("1"), "link", "Guía rápida", Some(4)),
        ("5", Some("4"), "StaticText", "Guía ", Some(5)),
        ("6", Some("4"), "emphasis", "", Some(6)),
        ("7", Some("6"), "StaticText", "rápida", Some(7)),
        // A name from elsewhere (a label): both are kept.
        ("8", Some("1"), "button", "Cerrar", Some(8)),
        ("9", Some("8"), "StaticText", "×", Some(9)),
        // A role ARIA does not name from content shows its content as content.
        ("10", Some("1"), "term", "CDP", Some(10)),
        ("11", Some("10"), "StaticText", "CDP", Some(11)),
        // A caption that names its table is listed; the table's name would repeat it.
        ("12", Some("1"), "table", "Pedidos", Some(12)),
        ("13", Some("12"), "caption", "", Some(13)),
        ("14", Some("13"), "StaticText", "Pedidos", Some(14)),
        ("15", Some("12"), "row", "", Some(15)),
        // A legend that names its group is the one left out.
        ("16", Some("1"), "group", "Plan", Some(16)),
        ("17", Some("16"), "Legend", "", Some(17)),
        ("18", Some("17"), "StaticText", "Plan", Some(18)),
        ("19", Some("16"), "radio", "Pro", Some(19)),
        // An image without a name says nothing; with one it does.
        ("20", Some("1"), "image", "", Some(20)),
        ("21", Some("1"), "image", "Logo", Some(21)),
    ]);
    property(&mut page, "4", "url", json!("http://fixture/docs/a"));
    assert_eq!(
        text(&[page], &mut RefTable::default()),
        "- button \"Guardar\" [ref=e1]\n\
         - link [ref=e2]:\n  - /url: http://fixture/docs/a\n  - text: Guía\n  - emphasis [ref=e3]: rápida\n\
         - button \"Cerrar\" [ref=e4]: ×\n\
         - term [ref=e5]: CDP\n\
         - table [ref=e6]:\n  - caption [ref=e7]: Pedidos\n  - row [ref=e8]\n\
         - group \"Plan\" [ref=e9]:\n  - radio \"Pro\" [ref=e10]\n\
         - img \"Logo\" [ref=e11]"
    );
}

#[test]
fn states_values_and_internal_roles_are_printed_as_playwright_prints_them() {
    let mut page = frame(&[
        ("1", None, "RootWebArea", "", Some(1)),
        ("2", Some("1"), "checkbox", "Uno", Some(2)),
        ("3", Some("1"), "checkbox", "Dos", Some(3)),
        ("4", Some("1"), "button", "Tres", Some(4)),
        ("5", Some("1"), "heading", "Cuatro", Some(5)),
        ("6", Some("1"), "textbox", "Cinco", Some(6)),
        ("7", Some("6"), "generic", "", Some(7)),
        ("8", Some("7"), "StaticText", "editor interno", Some(8)),
        ("9", Some("1"), "DisclosureTriangle", "Seis", Some(9)),
        ("10", Some("1"), "LabelText", "", Some(10)),
        ("11", Some("10"), "StaticText", "Siete", Some(11)),
        ("12", Some("10"), "LineBreak", "\n", Some(12)),
        ("13", Some("10"), "StaticText", "ocho", Some(13)),
        ("14", Some("1"), "listitem", "", Some(14)),
        ("15", Some("14"), "ListMarker", "• ", Some(15)),
        ("16", Some("14"), "StaticText", "Nueve", Some(16)),
        ("17", Some("16"), "InlineTextBox", "Nueve", None),
        ("18", Some("1"), "slider", "Diez", Some(18)),
        ("19", Some("1"), "textbox", "Once", Some(19)),
        ("20", Some("19"), "paragraph", "", Some(20)),
        ("21", Some("20"), "StaticText", "texto rico", Some(21)),
        ("22", Some("1"), "treeitem", "Doce", Some(22)),
    ]);
    for (id, name, value) in [
        ("2", "checked", json!("true")),
        ("3", "checked", json!("mixed")),
        ("3", "disabled", json!(true)),
        ("3", "invalid", json!("true")),
        ("4", "pressed", json!("mixed")),
        ("4", "expanded", json!(true)),
        ("4", "focused", json!(true)),
        ("4", "invalid", json!("false")),
        ("5", "level", json!(3)),
        ("6", "editable", json!("plaintext")),
        ("9", "expanded", json!(false)),
        ("19", "editable", json!("richtext")),
        ("22", "selected", json!(true)),
        ("22", "level", json!(2)),
    ] {
        property(&mut page, id, name, value);
    }
    node(&mut page, "6").value = Some(AxValue { value: json!("línea uno\nlínea dos") });
    node(&mut page, "18").value = Some(AxValue { value: json!(30) });
    assert_eq!(
        text(&[page], &mut RefTable::default()),
        "- checkbox \"Uno\" [checked] [ref=e1]\n\
         - checkbox \"Dos\" [checked=mixed] [disabled] [invalid] [ref=e2]\n\
         - button \"Tres\" [expanded] [active] [pressed=mixed] [ref=e3]\n\
         - heading \"Cuatro\" [level=3] [ref=e4]\n\
         - textbox \"Cinco\" [ref=e5]: línea uno línea dos\n\
         - button \"Seis\" [ref=e6]\n\
         - generic [ref=e7]: Siete ocho\n\
         - listitem [ref=e8]: Nueve\n\
         - slider \"Diez\" [ref=e9]: \"30\"\n\
         - textbox \"Once\" [ref=e10]:\n  - paragraph [ref=e11]: texto rico\n\
         - treeitem \"Doce\" [selected] [ref=e12]"
    );
    assert_eq!(aria_role("image"), "img");
    assert_eq!(aria_role("Iframe"), "iframe");
    assert_eq!(aria_role("Canvas"), "generic");
    assert_eq!(aria_role("navigation"), "navigation");
}

#[test]
fn what_accessibility_ignores_is_hidden_transparent_or_a_plain_container() {
    let mut page = frame(&[
        ("1", None, "RootWebArea", "", Some(1)),
        ("2", Some("1"), "none", "", Some(2)),
        ("3", Some("2"), "button", "Dentro de un contenedor", Some(3)),
        ("4", Some("2"), "button", "Segundo", Some(4)),
        ("5", Some("1"), "none", "", Some(5)),
        ("6", Some("5"), "button", "Oculto para lectores", Some(6)),
        ("7", Some("1"), "none", "", Some(7)),
        ("8", Some("7"), "checkbox", "Acepto", Some(8)),
        ("9", Some("7"), "none", "", Some(9)),
        ("10", Some("1"), "none", "", Some(10)),
        ("11", Some("10"), "StaticText", "Presentación", Some(11)),
        ("12", Some("1"), "none", "", Some(12)),
        ("13", Some("12"), "StaticText", "• ", None),
        ("14", Some("1"), "none", "", Some(14)),
        ("15", Some("14"), "button", "Único", Some(15)),
        ("16", Some("1"), "none", "", Some(16)),
        ("17", Some("16"), "row", "", Some(17)),
        ("18", Some("1"), "none", "", Some(18)),
        ("19", Some("18"), "generic", "", Some(19)),
        ("20", Some("19"), "StaticText", "celda de maquetado", Some(20)),
    ]);
    ignore(&mut page, "2", "uninteresting");
    ignore(&mut page, "5", "ariaHiddenSubtree");
    ignore(&mut page, "7", "labelFor");
    ignore(&mut page, "9", "presentationalRole");
    ignore(&mut page, "10", "presentationalRole");
    ignore(&mut page, "12", "presentationalRole");
    ignore(&mut page, "14", "uninteresting");
    ignore(&mut page, "16", "uninteresting");
    ignore(&mut page, "18", "uninteresting");
    for (backend, tag) in [(2, "DIV"), (3, "BUTTON"), (4, "BUTTON"), (5, "DIV"), (6, "BUTTON"), (7, "LABEL"), (8, "INPUT"), (10, "DIV"), (14, "DIV"), (15, "BUTTON"), (16, "TBODY"), (17, "TR"), (18, "TBODY"), (19, "TD")] {
        element(&mut page, backend, tag);
    }
    // The label's own text: ignored by accessibility, read on the page.
    *element(&mut page, 9, "#text") = DomNode {
        tag: "#text".into(),
        text: Some(" Acepto".into()),
        ..DomNode::default()
    };
    element(&mut page, 12, "::marker").marker = true;
    assert_eq!(
        text(&[page], &mut RefTable::default()),
        "- generic [ref=e1]:\n  - button \"Dentro de un contenedor\" [ref=e2]\n  - button \"Segundo\" [ref=e3]\n\
         - generic [ref=e4]:\n  - checkbox \"Acepto\" [ref=e5]\n  - text: Acepto\n\
         - text: Presentación\n\
         - button \"Único\" [ref=e6]\n\
         - rowgroup [ref=e7]:\n  - row [ref=e8]\n\
         - text: celda de maquetado"
    );
}

#[test]
fn the_dom_decides_refs_cursors_and_which_wrappers_are_only_their_words() {
    let build = || {
        let mut page = frame(&[
            ("1", None, "RootWebArea", "", Some(1)),
            ("2", Some("1"), "generic", "", Some(2)),
            ("3", Some("2"), "StaticText", "Tarjeta", Some(3)),
            ("4", Some("2"), "button", "Dentro", Some(4)),
            ("5", Some("1"), "generic", "", Some(5)),
            ("6", Some("5"), "StaticText", "palabras", Some(6)),
            ("7", Some("1"), "generic", "", Some(7)),
            ("8", Some("7"), "StaticText", "con clic", Some(8)),
            ("9", Some("1"), "button", "Sin caja", Some(9)),
            ("10", Some("1"), "button", "Inerte", Some(10)),
            ("11", Some("1"), "link", "Ayuda", Some(11)),
            ("12", Some("1"), "image", "", Some(12)),
            ("13", Some("1"), "image", "", Some(13)),
            ("14", Some("1"), "generic", "", Some(14)),
        ]);
        property(&mut page, "11", "url", json!("http://fixture/ayuda"));
        page
    };
    // Without the DOM every element is assumed reachable and nothing more is known.
    assert_eq!(
        text(&[build()], &mut RefTable::default()),
        "- generic [ref=e1]:\n  - text: Tarjeta\n  - button \"Dentro\" [ref=e2]\n\
         - generic [ref=e3]: palabras\n\
         - generic [ref=e4]: con clic\n\
         - button \"Sin caja\" [ref=e5]\n\
         - button \"Inerte\" [ref=e6]\n\
         - link \"Ayuda\" [ref=e7]:\n  - /url: http://fixture/ayuda"
    );
    let mut page = build();
    for backend in [2, 4, 5, 7, 10, 11, 12, 13, 14] {
        element(&mut page, backend, "DIV");
    }
    element(&mut page, 2, "DIV").pointer_cursor = true;
    element(&mut page, 4, "BUTTON").pointer_cursor = true;
    *element(&mut page, 5, "SPAN") = DomNode {
        tag: "SPAN".into(),
        visible: true,
        inline: true,
        only_text: true,
        ..DomNode::default()
    };
    *element(&mut page, 7, "SPAN") = DomNode {
        clickable: true,
        ..page.dom[&5].clone()
    };
    element(&mut page, 9, "BUTTON").visible = false;
    element(&mut page, 10, "BUTTON").no_pointer_events = true;
    element(&mut page, 11, "A").href = Some("/ayuda".into());
    element(&mut page, 11, "A").pointer_cursor = true;
    element(&mut page, 13, "svg").pointer_cursor = true;
    element(&mut page, 14, "DIV").pointer_cursor = true;
    assert_eq!(
        text(&[page], &mut RefTable::default()),
        "- generic [ref=e1] [cursor=pointer]:\n  - text: Tarjeta\n  - button \"Dentro\" [ref=e2]\n\
         - text: palabras\n\
         - generic [ref=e3]: con clic\n\
         - button \"Sin caja\"\n\
         - button \"Inerte\"\n\
         - link \"Ayuda\" [ref=e4] [cursor=pointer]:\n  - /url: /ayuda\n\
         - img [ref=e5] [cursor=pointer]\n\
         - generic [ref=e6] [cursor=pointer]"
    );
}

#[test]
fn frames_nest_under_their_iframe_with_refs_of_their_own() {
    let mut main = frame(&[
        ("1", None, "RootWebArea", "", Some(1)),
        ("2", Some("1"), "Iframe", "Pagos", Some(2)),
        ("3", Some("1"), "Iframe", "Sin leer", Some(3)),
        ("4", Some("1"), "Iframe", "Repetido", Some(4)),
    ]);
    main.children = HashMap::from([(2, 1), (4, 1), (3, 9)]);
    let mut child = frame(&[
        ("1", None, "RootWebArea", "", Some(1)),
        ("2", Some("1"), "button", "Pagar", Some(2)),
        ("3", Some("1"), "Iframe", "", Some(3)),
    ]);
    // A frame that claims to contain its own parent is not followed twice.
    child.children = HashMap::from([(3, 0)]);
    let mut refs = RefTable::default();
    let key = |frame: &str| DocumentKey {
        frame: frame.into(),
        ..DocumentKey::default()
    };
    main.document = refs.document(&key("main"));
    child.document = refs.document(&key("child"));
    assert_eq!(
        text(&[main, child], &mut refs),
        "- iframe \"Pagos\" [ref=e1]:\n  - button \"Pagar\" [ref=f1e1]\n  - iframe [ref=f1e2]\n\
         - iframe \"Sin leer\" [ref=e2]\n\
         - iframe \"Repetido\" [ref=e3]"
    );
    assert_eq!(refs.resolve("f1e1"), Some((&key("child"), 2)));
    assert_eq!(refs.resolve("e1"), Some((&key("main"), 2)), "the same node id in two documents is two elements");
}

#[test]
fn a_hostile_tree_neither_overflows_the_stack_nor_loops() {
    // Far deeper than any page, and each level a real element.
    let depth = 20_000;
    let ids: Vec<String> = (0..depth).map(|level| level.to_string()).collect();
    let mut nodes = vec![("root", None, "RootWebArea", "", Some(0))];
    for level in 0..depth {
        let parent = if level == 0 { "root" } else { ids[level - 1].as_str() };
        nodes.push((ids[level].as_str(), Some(parent), "list", "", Some(level as i64 + 1)));
    }
    let deep = text(&[frame(&nodes)], &mut RefTable::default());
    assert!(deep.lines().count() <= MAX_TREE_DEPTH + 1, "{}", deep.lines().count());
    assert!(deep.starts_with("- list [ref=e1]:\n  - list [ref=e2]"));

    // Children that name an ancestor, themselves, or a node that does not exist.
    let mut cyclic = frame(&[
        ("1", None, "RootWebArea", "", Some(1)),
        ("2", Some("1"), "list", "", Some(2)),
        ("3", Some("2"), "button", "Uno", Some(3)),
    ]);
    node(&mut cyclic, "3").child_ids = vec!["1".into(), "2".into(), "3".into(), "fantasma".into()];
    node(&mut cyclic, "2").child_ids.push("3".into());
    assert_eq!(text(&[cyclic], &mut RefTable::default()), "- list [ref=e1]:\n  - button \"Uno\" [ref=e2]");
    // No root at all, and no frames at all.
    assert_eq!(render(&[Frame::default()], None, &SnapshotOptions::default(), &mut RefTable::default()), None);
    assert_eq!(render(&[], None, &SnapshotOptions::default(), &mut RefTable::default()), None);
}

#[test]
fn a_dom_snapshot_with_broken_indexes_is_read_without_panicking() {
    let broken: DomSnapshot = serde_json::from_value(json!({
        "strings": ["F1", "DIV", "#text", "hola", "href", "/x", "pointer", "visible", "inline", "none", "marker", "role", "button"],
        "documents": [{
            "frameId": 0,
            "nodes": {
                "parentIndex": [-1, 0, 1, 99, 0],
                "nodeType": [1, 1, 3, 1],
                "nodeName": [1, 1, 2, 500, -7],
                "nodeValue": [-1, -1, 3],
                "backendNodeId": [10, 11, 12, 13, 14],
                "attributes": [[], [4, 5, 11, 12, 4], [77, 78]],
                "contentDocumentIndex": {"index": [1, 42], "value": [0, 9]},
                "pseudoType": {"index": [3, -1], "value": [10, 10]},
                "isClickable": {"index": [1, 1000, -3]}
            },
            "layout": {
                "nodeIndex": [1, 2, 70, 3],
                "styles": [[8, 7, 6, 9], [], [8]],
                "bounds": [[0, 0, 10, 10], [0, 0, 5], [1, 1, 1, 1], [0, 0, 0, 0]]
            }
        }, {"frameId": 400}]
    }))
    .unwrap();
    let frames = broken.into_frames();
    let document = &frames["F1"];
    assert_eq!(document.len(), 5);
    let link = &document[&11];
    assert_eq!((link.tag.as_str(), link.href.as_deref()), ("DIV", Some("/x")));
    assert!(link.visible && link.inline && link.pointer_cursor && link.no_pointer_events && link.clickable && link.explicit_role && link.only_text);
    assert_eq!(link.content_frame.as_deref(), Some("F1"));
    assert_eq!(document[&12].text.as_deref(), Some("hola"));
    assert!(!document[&12].visible, "bounds that are not four numbers are no box");
    assert!(document[&13].marker && document[&13].tag.is_empty() && !document[&13].visible);
    assert!(frames.contains_key(""), "a document whose frame id is out of range still parses");
    assert!(DomSnapshot::default().into_frames().is_empty());
}

#[test]
fn elements_are_found_by_the_role_and_name_the_snapshot_prints() {
    let frames = recorded("tools", &mut RefTable::default()).0;
    let page = &frames[0];
    assert_eq!(matching(page, "button", "Borrar").len(), 2);
    assert_eq!(matching(page, "button", "Confirmar").len(), 1);
    assert_eq!(matching(page, "combobox", "Color").len(), 1);
    assert_eq!(matching(page, "checkbox", "Acepto").len(), 1, "the name is compared without its stray spaces");
    assert_eq!(matching(page, "iframe", "Marco").len(), 1);
    assert_eq!(matching(page, "button", "borrar").len(), 0, "the name is exact");
    assert_eq!(matching(page, "link", "Borrar").len(), 0);
    let frames = recorded("lists", &mut RefTable::default()).0;
    assert_eq!(matching(&frames[0], "img", "Logo").len(), 1, "by the role as printed, not Chrome's own word for it");
    assert_eq!(matching(&frames[0], "image", "Logo").len(), 0);
}

#[test]
fn text_is_quoted_exactly_when_yaml_would_read_it_as_something_else() {
    for plain in ["Hola mundo", "a:b", "http://fixture/x", "x - y", "50%", "é", "a#b", "1.2.3", "0x", "e5", "truest", "~", "tab\taquí"] {
        assert_eq!(yaml_value(plain), plain, "{plain:?}");
    }
    for (text, quoted) in [
        ("", "\"\""),
        (" x", "\" x\""),
        ("x ", "\"x \""),
        ("- x", "\"- x\""),
        ("a: b", "\"a: b\""),
        ("a:", "\"a:\""),
        ("a #b", "\"a #b\""),
        ("uno\ndos", "\"uno\\ndos\""),
        ("{x}", "\"{x}\""),
        ("`x`", "\"`x`\""),
        ("[x", "\"[x\""),
        ("\"x\"", "\"\\\"x\\\"\""),
        ("#x", "\"#x\""),
        ("a\\b\u{1}", "\"a\\\\b\\x01\""),
        ("36", "\"36\""),
        ("-1.5e3", "\"-1.5e3\""),
        (".5", "\".5\""),
        ("5.", "\"5.\""),
        ("0x1F", "\"0x1F\""),
        ("0b101", "\"0b101\""),
        ("Infinity", "\"Infinity\""),
        ("y", "\"y\""),
        ("No", "\"No\""),
        ("TRUE", "\"TRUE\""),
        ("null", "\"null\""),
    ] {
        assert_eq!(yaml_value(text), quoted, "{text:?}");
    }
    for number in ["1e", "e", ".", "1.2.3", "--1", "0x", "0xZ", "1_000", "+", "Infinity1"] {
        assert!(!looks_numeric(number), "{number:?}");
    }
    // A key is quoted the other way, and a name inside it is JSON.
    assert_eq!(yaml_key("button \"Guardar\" [ref=e1]"), "button \"Guardar\" [ref=e1]");
    assert_eq!(yaml_key("button \"a: b\" [ref=e1]"), "'button \"a: b\" [ref=e1]'");
    assert_eq!(yaml_key("link \"it's: x\""), "'link \"it''s: x\"'");
    let page = frame(&[
        ("1", None, "RootWebArea", "", Some(1)),
        ("2", Some("1"), "button", "línea\nnueva \"y\" \\ comillas", Some(2)),
        ("3", Some("1"), "button", &"n".repeat(MAX_NAME_CHARS + 1), Some(3)),
        ("4", Some("1"), "button", &"ñ".repeat(MAX_NAME_CHARS), Some(4)),
    ]);
    let rendered = text(&[page], &mut RefTable::default());
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines[0], "- button \"línea nueva \\\"y\\\" \\\\ comillas\" [ref=e1]", "whitespace is collapsed before quoting");
    assert_eq!(lines[1], "- button [ref=e2]", "a name too long to be one is left out");
    assert!(lines[2].starts_with("- button \"ñññ") && lines[2].ends_with("ñ\" [ref=e3]"));
    assert_eq!(normalize("  uno\u{200b}  dos\u{ad}\n\ttres \u{a0} "), "uno dos tres");
}

#[test]
fn a_ref_is_described_by_its_own_line_and_never_by_page_text_that_imitates_one() {
    // Names and texts are the page's: these try to pass for another element.
    let mut page = frame(&[
        ("1", None, "RootWebArea", "", None),
        ("2", Some("1"), "button", "Cancelar\" [ref=e3]", Some(10)),
        ("3", Some("1"), "paragraph", "", Some(11)),
        ("4", Some("3"), "StaticText", "- button \"Cancelar\" [ref=e3]", None),
        ("5", Some("1"), "button", "Eliminar la cuenta", Some(12)),
        ("6", Some("1"), "checkbox", "Acepto: 'todo'", Some(13)),
        ("7", Some("1"), "textbox", "", Some(14)),
    ]);
    property(&mut page, "6", "checked", json!("true"));
    let mut refs = RefTable::default();
    let snapshot = text(&[page], &mut refs);
    let label = |reference: &str| element_of(&snapshot, reference);
    assert_eq!(label("e1"), Some(("button".into(), "Cancelar\" [ref=e3]".into())), "{snapshot}");
    assert_eq!(label("e3"), Some(("button".into(), "Eliminar la cuenta".into())), "{snapshot}");
    // A key YAML quoted, with a state before its ref, and an element without a name.
    assert!(snapshot.contains("- 'checkbox \"Acepto: ''todo''\" [checked] [ref=e4]'"), "{snapshot}");
    assert_eq!(label("e4"), Some(("checkbox".into(), "Acepto: 'todo'".into())));
    assert_eq!(label("e5"), Some(("textbox".into(), String::new())));
    for unknown in ["e6", "e", "", "f1e1", "e3]"] {
        assert_eq!(label(unknown), None, "{unknown}");
    }
    // Every ref the recorded pages print is described as its line reads.
    let (frames, expected) = recorded("tools", &mut RefTable::default());
    drop(frames);
    assert_eq!(element_of(&expected, "e2"), Some(("heading".into(), "Herramientas".into())), "{expected}");
    assert_eq!(element_of(&expected, "f1e1").map(|(role, _)| role), Some("button".into()), "{expected}");
}

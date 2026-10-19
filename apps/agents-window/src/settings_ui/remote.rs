//! Settings → Remoto: the phone's access to this PC.

use super::*;
use crate::view::RemoteView;
/// Side of the pairing code on screen.
const CODE_SIDE: f32 = 260.0;

pub(super) fn page(state: &ViewState, compact: bool) -> Vec<Element> {
    let remote = &state.remote;
    let mut groups = Vec::new();
    if let Some(error) = &remote.error {
        groups.push(settings_parts::feedback_banner(error, FeedbackTone::Error, None, compact));
    }
    let status = match remote.listening {
        Some(port) => format!("Escuchando en el puerto {port}. Huella {}", remote.fingerprint),
        None if remote.enabled => "No está escuchando.".to_owned(),
        None => "Apagado: ningún teléfono puede conectarse.".to_owned(),
    };
    groups.push(settings_parts::settings_group(
        "Acceso desde el teléfono",
        vec![
            settings_parts::switch_row(
                "remote_enabled",
                "Permitir que un iPhone use este PC",
                status,
                remote.enabled,
                true,
                compact,
            ),
            settings_parts::settings_row(
                "Puerto",
                "Donde el teléfono se conecta. Se guarda con Enter.",
                field("remote_port", &remote.port, "7787"),
                compact,
            ),
            settings_parts::switch_row(
                "remote_keep_awake",
                "Mantener el PC despierto",
                "Mientras el remoto esté activo el PC no se suspende solo, para que el teléfono lo encuentre. Gasta más batería.",
                remote.keep_awake,
                remote.enabled,
                compact,
            ),
            settings_parts::switch_row(
                "remote_upnp",
                "Abrir el puerto en el router (UPnP)",
                remote.upnp.clone().unwrap_or_else(|| {
                    "Para llegar desde afuera por IPv4. Pone el servidor en internet, protegido por TLS y por la clave de cada teléfono.".into()
                }),
                remote.upnp.is_some(),
                remote.enabled,
                compact,
            ),
            settings_parts::settings_row(
                "Dirección pública",
                "Opcional. Si abriste el puerto en tu router, el nombre o la IP pública con la que se llega desde afuera.",
                field("remote_public", &remote.public_address, "casa.ejemplo.com"),
                compact,
            ),
        ],
    ));
    if let Some(port) = remote.listening {
        groups.push(settings_parts::settings_group(
            "Firewall",
            vec![settings_parts::value_row(
                "Puerto abierto en este PC",
                format!(
                    "El sistema tiene que dejar entrar conexiones al puerto {port}, por IPv4 y por IPv6. Desde este mismo PC responde aunque esté cerrado, así que probalo desde el teléfono. En NixOS: networking.firewall.allowedTCPPorts = [ {port} ];"
                ),
                format!("TCP {port}"),
                compact,
            )],
        ));
        groups.push(pairing(remote, compact));
        groups.push(devices(remote, compact));
        groups.push(settings_parts::settings_group(
            "Avisos",
            vec![settings_parts::value_row(
                "Avisos en el iPhone",
                if remote.push {
                    "Cuando una tarea espera tu permiso, te pregunta algo, termina o falla, mientras no estás frente al PC."
                } else {
                    "Faltan la clave de Apple Push en la carpeta remote/push de los datos de openADE."
                },
                if remote.push { "Activos" } else { "Sin configurar" },
                compact,
            )],
        ));
    }
    groups
}

fn pairing(remote: &RemoteView, compact: bool) -> Element {
    let routes = if remote.routes.is_empty() {
        "No se encontró una dirección de red: conectá el PC a una red.".to_owned()
    } else {
        remote.routes.iter().map(|route| route.trim_start_matches("https://")).collect::<Vec<_>>().join(" · ")
    };
    let Some((image, _)) = &remote.qr else {
        return settings_parts::settings_group(
            "Emparejar un iPhone",
            vec![settings_parts::settings_row(
                "Código de emparejamiento",
                format!("Sirve una vez y por diez minutos. El teléfono se conectará por: {routes}"),
                outline("remote_pair", "Mostrar código").icon(Icon::Phone).render(),
                compact,
            )],
        );
    };
    let code = Element::raster_image(image.clone()).size(Some(CODE_SIDE), Some(CODE_SIDE)).radius(8.0);
    let explanation = Element::column(vec![
        Text::new("Escanealo con la app openADE del iPhone", theme::component_palette()).render(),
        Text::new(
            format!("Vence en diez minutos y sirve una sola vez. Huella del PC: {}. Direcciones: {routes}", remote.fingerprint),
            theme::component_palette(),
        )
        .muted()
        .render(),
        outline("remote_pair_close", "Ocultar").render(),
    ])
    .gap(8.0);
    let body = if compact {
        Element::column(vec![code, explanation]).gap(16.0)
    } else {
        Element::row(vec![code, explanation.flex(1.0)]).gap(20.0)
    };
    settings_parts::settings_group("Emparejar un iPhone", vec![body.padding(12.0)])
}

fn devices(remote: &RemoteView, compact: bool) -> Element {
    if remote.devices.is_empty() {
        return settings_parts::settings_group(
            "Teléfonos con acceso",
            vec![Text::new("Todavía no hay ninguno.", theme::component_palette()).muted().render()],
        );
    }
    let rows = remote
        .devices
        .iter()
        .map(|device| {
            settings_parts::settings_row(
                &device.name,
                &device.seen,
                outline(format!("remote_revoke:{}", device.id), "Quitar acceso").render(),
                compact,
            )
        })
        .collect();
    settings_parts::settings_group("Teléfonos con acceso", rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::RemoteDeviceView;
    use openide_ui::{DrawCommand, Frame, Size, UiTree};

    fn listening() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.remote = RemoteView {
            enabled: true,
            port: "7787".into(),
            listening: Some(7787),
            fingerprint: "AB12-CD34-EF56-7890".into(),
            routes: vec!["https://192.168.1.40:7787".into(), "https://[2800:810::1]:7787".into()],
            push: true,
            devices: vec![
                RemoteDeviceView { id: "a".into(), name: "iPhone de Nihuel".into(), seen: "Conectado ahora".into(), connected: true },
                RemoteDeviceView { id: "b".into(), name: "iPad".into(), seen: "Visto hace 3 d".into(), connected: false },
            ],
            qr: Some((crate::app::remote_qr_fixture(), u64::MAX)),
            ..RemoteView::default()
        };
        state
    }

    fn texts(state: &ViewState, width: f32) -> (Frame, String) {
        let frame = UiTree::new(Element::column(page(state, width < 500.0)).gap(16.0)).layout(Size::new(width, 1400.0));
        let text = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        (frame, text)
    }

    #[test]
    fn the_page_shows_the_code_the_addresses_and_who_has_access() {
        let (_, text) = texts(&listening(), 720.0);
        for expected in ["Escuchando en el puerto 7787", "AB12-CD34-EF56-7890", "192.168.1.40:7787", "iPhone de Nihuel", "Conectado ahora", "Quitar acceso", "Activos"] {
            assert!(text.contains(expected), "{expected} missing from:\n{text}");
        }
        let mut off = crate::view::tests::empty_state();
        off.remote.port = "7787".into();
        let (_, text) = texts(&off, 720.0);
        assert!(text.contains("Apagado"), "{text}");
        assert!(!text.contains("Teléfonos con acceso"));
    }

    /// Renders the page with a code on screen as PNG files under
    /// `OPENADE_VISUAL_DIR`, to look at.
    #[test]
    #[ignore = "visual review: OPENADE_VISUAL_DIR=<dir> cargo test export_remote_visual_review -- --ignored"]
    fn export_remote_visual_review() {
        let out = std::path::PathBuf::from(std::env::var_os("OPENADE_VISUAL_DIR").expect("OPENADE_VISUAL_DIR"));
        std::fs::create_dir_all(&out).unwrap();
        let state = listening();
        for (name, width) in [("remote-wide", 760.0), ("remote-narrow", 380.0)] {
            let height = 1100.0;
            let (frame, _) = texts(&state, width);
            let path = out.join(format!("{name}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height).unwrap();
            let svg = std::fs::read_to_string(&path).unwrap();
            let parsed = resvg::usvg::Tree::from_str(&svg, &crate::settings_ui::visual_qa::native_svg_options()).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(&parsed, resvg::tiny_skia::Transform::identity(), &mut pixmap.as_mut());
            pixmap.save_png(path.with_extension("png")).unwrap();
        }
    }
}

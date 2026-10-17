use super::*;
use crate::external_agent_settings::{ExternalPermissionMode, ExternalTool};

fn tool_row(
    state: &ViewState,
    tool: ExternalTool,
    title: &str,
    description: &str,
    compact: bool,
) -> Element {
    settings_parts::switch_row(
        format!("external_tool:{}", tool.name()),
        title,
        description,
        state.external_agents.allows(tool),
        state.external_agents.enabled() && tool != ExternalTool::Subagents,
        compact,
    )
}

pub(super) fn page(state: &ViewState, compact: bool) -> Vec<Element> {
    let settings = state.external_agents;
    let mode = settings.permission_mode();
    let mode_option = |option: ExternalPermissionMode| {
        DropdownOption::new(option.action_id(), option.label()).selected(option == mode)
    };
    let mut content = vec![
        settings_parts::feedback_banner(
            "Funciona con Claude Code y Codex iniciados desde el menú de perfiles de la terminal. Los subagentes de openADE todavía no se ofrecen a una CLI externa.",
            FeedbackTone::Info,
            None,
            compact,
        ),
        settings_parts::settings_group(
            "Puente de herramientas",
            vec![
                settings_parts::switch_row(
                    "external_agents_enabled",
                    "Ofrecer herramientas de openADE",
                    "Claude Code, Codex y otras CLIs iniciadas desde openADE reciben las herramientas activadas abajo. Desactivado, ninguna llamada externa llega a openADE.",
                    settings.enabled(),
                    true,
                    compact,
                ),
                page_note(
                    "Solo CLIs iniciadas por openADE",
                    "Cada sesión recibe un acceso propio que deja de valer al cerrarla. Una CLI abierta por fuera de openADE no puede conectarse.",
                    Icon::Terminal,
                ),
            ],
        ),
        settings_parts::settings_group(
            "CLIs detectadas",
            state
                .agent_clis
                .iter()
                .map(|cli| {
                    settings_parts::settings_row(
                        &cli.label,
                        match &cli.resolved {
                            Some(path) => format!("Encontrada en {path}. Aparece en el menú de perfiles de la terminal."),
                            None => "No se encontró en el PATH de tu shell. Escribí la ruta absoluta del ejecutable y presioná Enter.".to_owned(),
                        },
                        field(
                            &format!("agent_cli_path:{}", cli.id),
                            &cli.manual,
                            "Ruta manual (opcional)",
                        )
                        .size(Some(260.0), None),
                        compact,
                    )
                })
                .collect(),
        ),
    ];
    content.extend(acp_groups(state, compact));
    content.extend([
        own_profiles(state, compact),
        settings_parts::settings_group(
            "Herramientas ofrecidas",
            vec![
                tool_row(
                    state,
                    ExternalTool::GitReview,
                    "Revisión de cambios",
                    "Lee los cambios sin confirmar del proyecto como los muestra la revisión. Solo lectura; no pide aprobación.",
                    compact,
                ),
                tool_row(
                    state,
                    ExternalTool::Browser,
                    "Navegador",
                    "Cada CLI maneja un navegador propio, que se abre en una pestaña del panel lateral con su primera llamada y se cierra con su terminal. No comparte páginas ni sesiones iniciadas con el navegador de una conversación.",
                    compact,
                ),
                tool_row(
                    state,
                    ExternalTool::Conversations,
                    "Otras conversaciones",
                    "Lee conversaciones de este proyecto, nunca de otros. Su contenido puede incluir datos sensibles o instrucciones que la CLI tome como propias.",
                    compact,
                ),
                tool_row(
                    state,
                    ExternalTool::Subagents,
                    "Subagentes",
                    "Todavía no disponible. Creará subagentes de openADE y esperará su resultado.",
                    compact,
                ),
            ],
        ),
        settings_parts::settings_group(
            "Permisos",
            vec![
                settings_parts::settings_row(
                    "Aprobaciones",
                    match mode {
                        ExternalPermissionMode::MirrorCli => "Seguir a la CLI: si la iniciaste en modo aceptar todo, las herramientas de openADE tampoco preguntan. En cualquier otro modo, openADE pide aprobación.",
                        ExternalPermissionMode::AlwaysAsk => "Preguntar siempre: openADE pide aprobación aunque la CLI esté en modo aceptar todo.",
                    },
                    settings_choice_selector(
                        state,
                        "external_permission",
                        mode.label(),
                        vec![
                            mode_option(ExternalPermissionMode::MirrorCli),
                            mode_option(ExternalPermissionMode::AlwaysAsk),
                        ],
                    ),
                    compact,
                ),
                settings_parts::settings_row(
                    "Reglas de ejecución",
                    "Ninguna herramienta ofrecida hoy ejecuta comandos. Cuando alguna lo haga, las reglas que los deniegan valdrán también para las llamadas externas.",
                    outline("settings_page:commands", "Ver reglas").icon(Icon::ArrowRight).render(),
                    compact,
                ),
            ],
        ),
    ]);
    content
}

/// The agents of the ACP registry: those ready to start a task, then the
/// rest with what each needs to run here.
fn acp_groups(state: &ViewState, compact: bool) -> Vec<Element> {
    use crate::acp_run::{AcpEntry, AcpSource};
    if state.acp_catalog.is_empty() {
        return vec![match &state.acp_catalog_error {
            Some(error) => settings_parts::feedback_banner(
                format!("No se pudo leer el registro de agentes ACP: {error}"),
                FeedbackTone::Error,
                None,
                compact,
            ),
            None => settings_parts::loading_state(
                "Buscando agentes ACP",
                "Leyendo el registro de agentes y lo que está instalado en este equipo.",
            ),
        }];
    }
    let row = |entry: &AcpEntry| {
        let about = match entry.description.trim().trim_end_matches('.') {
            "" => String::new(),
            about => format!("{about}. "),
        };
        match &entry.source {
            AcpSource::Own(path) => settings_parts::value_row(
                &entry.name,
                format!("{about}Usa tu instalación en {}.", path.display()),
                "Instalado",
                compact,
            ),
            AcpSource::Downloaded => settings_parts::settings_row(
                &entry.name,
                format!("{about}Versión {} descargada por openADE.", entry.version),
                outline(format!("acp_remove:{}", entry.id), "Quitar").icon(Icon::Trash).render(),
                compact,
            ),
            AcpSource::Package { runner, package, on } => settings_parts::switch_row(
                format!("acp_package:{}", entry.id),
                &entry.name,
                format!("{about}Se inicia con {runner} {package}, que lo descarga la primera vez."),
                *on,
                true,
                compact,
            ),
            AcpSource::Download { target, .. } => {
                let installing = state.acp_installing.as_deref() == Some(entry.id.as_str());
                let check = if target.sha256.is_some() {
                    " y comprueba su huella publicada"
                } else {
                    "; el registro no publica su huella"
                };
                settings_parts::settings_row(
                    &entry.name,
                    format!("{about}Descarga la versión {} de su fabricante{check}.", entry.version),
                    outline(format!("acp_install:{}", entry.id), if installing { "Instalando…" } else { "Instalar" })
                        .icon(Icon::Add)
                        .enabled(state.acp_installing.is_none())
                        .render(),
                    compact,
                )
            }
            AcpSource::Missing(reason) => settings_parts::value_row(&entry.name, about.trim_end(), *reason, compact),
        }
    };
    let (ready, rest): (Vec<&AcpEntry>, Vec<&AcpEntry>) =
        state.acp_catalog.iter().partition(|entry| entry.offer().is_some());
    let mut ready_rows = vec![page_note(
        "Conversan en el chat de openADE",
        "Los agentes listos aparecen en Inicio y en Nueva tarea con. Cada uno usa su propia cuenta y la primera vez te pide iniciar sesión.",
        Icon::InfoCircle,
    )];
    ready_rows.extend(ready.into_iter().map(row));
    vec![
        settings_parts::settings_group("Agentes ACP listos", ready_rows),
        settings_parts::settings_group("Registro ACP", rest.into_iter().map(row).collect()),
    ]
}

/// The CLI profiles of the user's file. The built-in ones have no row here:
/// they are not the user's to change.
fn own_profiles(state: &ViewState, compact: bool) -> Element {
    let mut rows = state
        .cli_profiles
        .iter()
        .map(|profile| {
            settings_parts::settings_row(
                profile.label.clone(),
                format!("{} · {}", profile.id, profile.executable.display()),
                ButtonGroup::new(vec![
                    outline(format!("cli_profile_edit:{}", profile.id), "Editar").render(),
                    outline(format!("cli_profile_delete:{}", profile.id), "Quitar").render(),
                ])
                .sizing(crate::ui_sizing::ENTITY_ACTIONS)
                .stacked(compact)
                .render(),
                compact,
            )
        })
        .collect::<Vec<_>>();
    rows.push(settings_parts::settings_row(
        "Otra CLI",
        "Un ejecutable que recibe el pedido por la entrada estándar o como argumento y responde por la salida. Se elige como motor de una automatización; no aparece en el menú de perfiles de la terminal.",
        outline("cli_profile_new", "Agregar perfil").icon(Icon::Add).render(),
        compact,
    ));
    settings_parts::settings_group("Perfiles propios", rows)
}

/// The form of one of the user's own CLI profiles, above the full window.
pub(super) fn editor_modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    use openide_cli::PromptTransport;
    let Some(draft) = state
        .cli_profile_draft
        .as_ref()
        .filter(|_| state.shows(super::SettingsPage::ExternalAgents))
    else {
        return base;
    };
    let width = Modal::fit_width(viewport_width, 560.0);
    let body_width = crate::ui_sizing::form_width(width).max(0.0);
    let editing = draft.editing.is_some();
    let mut fields = Vec::new();
    if state.settings_notice_error {
        if let Some(error) = &state.settings_notice {
            fields.push(settings_parts::feedback_banner(error, FeedbackTone::Error, None, width < 420.0));
        }
    }
    let input = |id: &str, value: &str, placeholder: &str| {
        super::mono_field(id, value, placeholder).width(body_width).height(40.0)
    };
    let area = |id: &str, value: &str, placeholder: &str, height: f32| {
        TextArea::new(id, value, placeholder, control_palette())
            .width(body_width)
            .height(height)
            .render()
    };
    fields.extend(
        [
            super::form_field(
                "ID del perfil",
                input("cli_profile_draft_id", &draft.id, "ej. mi-cli").enabled(!editing).render(),
            )
            .hint("Minúsculas, números, guiones y guiones bajos. No se puede cambiar después.")
            .rows(),
            super::form_field(
                "Nombre",
                input("cli_profile_draft_label", &draft.label, "Mi CLI").render(),
            )
            .rows(),
            super::form_field(
                "Ejecutable",
                input("cli_profile_draft_executable", &draft.executable, "ej. mi-cli").render(),
            )
            .hint("Nombre del ejecutable o ruta absoluta. Se inicia sin shell.")
            .rows(),
            super::form_field(
                "Argumentos (JSON)",
                area("cli_profile_draft_args", &draft.args, "[\"--print\"]", 96.0),
            )
            .hint("Lista JSON de textos; cada elemento es un argumento literal. Si el pedido va como argumento, exactamente uno debe contener {prompt}.")
            .rows(),
            super::form_field(
                "Variables de entorno",
                area("cli_profile_draft_env", &draft.env, "LOG_LEVEL=debug", 76.0),
            )
            .hint("Una por línea, como NOMBRE=valor. El valor se guarda sin cifrar en cli-profiles.json, en la carpeta de datos.")
            .rows(),
            super::form_field(
                "Cómo recibe el pedido",
                ButtonGroup::new(
                    [
                        (PromptTransport::Stdin, "stdin", "Entrada estándar"),
                        (PromptTransport::Argument, "argument", "Argumento"),
                    ]
                    .into_iter()
                    .map(|(transport, id, label)| {
                        outline(format!("cli_profile_transport:{id}"), label)
                            .active(draft.prompt_transport == transport)
                            .render()
                    })
                    .collect(),
                )
                .stacked(width < 420.0)
                .render(),
            )
            .rows(),
        ]
        .into_iter()
        .flatten(),
    );
    let cancel = outline("cli_profile_cancel", "Cancelar").render();
    let save = theme::primary(
        "cli_profile_save",
        if editing { "Guardar cambios" } else { "Agregar perfil" },
    )
    .size(ControlSize::Compact)
    .render();
    Modal::new(
        base,
        true,
        if editing { "Editar perfil" } else { "Nuevo perfil" },
        "Define una CLI que openADE inicia sin terminal en cada ejecución.",
        "cli_profile_cancel",
        theme::component_palette(),
    )
    .width(width)
    .height(600.0)
    .body(Element::column(fields).gap(7.0).size(Some(body_width), None))
    .footer(super::form_footer(cancel, save, width))
    .initial_focus(if editing { "cli_profile_draft_label" } else { "cli_profile_draft_id" })
    .return_focus(
        draft
            .editing
            .as_ref()
            .map_or_else(|| "cli_profile_new".to_owned(), |id| format!("cli_profile_edit:{id}")),
    )
    .render()
}

/// Which model `/review` runs on.
pub(super) fn review_model_group(state: &ViewState, compact: bool) -> Element {
    use crate::review_model::ReviewModelMode;
    let review = &state.review_model;
    let explanation = match review.mode {
        ReviewModelMode::Same => "La revisión usa el modelo que tengas seleccionado en el composer.".to_owned(),
        ReviewModelMode::Automatic if review.automatic.is_empty() => "Ahora usaría el modelo seleccionado: el catálogo no muestra otro del mismo proveedor que soporte herramientas y cueste la mitad o menos.".to_owned(),
        ReviewModelMode::Automatic => format!("Ahora usaría {}. Elige, dentro del mismo proveedor, el modelo más capaz que cueste la mitad o menos que el seleccionado y soporte herramientas.", review.automatic),
        ReviewModelMode::Manual if review.manual.is_empty() => "Todavía no guardaste un modelo. Hasta que lo hagas, /review no arranca en este modo.".to_owned(),
        ReviewModelMode::Manual => format!("La revisión usa {}.", review.manual),
    };
    let options = ReviewModelMode::ALL
        .into_iter()
        .map(|mode| DropdownOption::new(mode.action_id(), mode.label()).selected(mode == review.mode))
        .collect();
    // The manual choice is picked here, from the providers that are
    // connected now and the models of each that can run a turn.
    let choices = connected_model_choices(state);
    let picked = choices.iter().find(|provider| provider.id == review.manual_provider);
    let provider_label = picked.map_or_else(
        || if review.manual_provider.is_empty() { "Elegir proveedor".to_owned() } else { review.manual_provider.clone() },
        |provider| provider.name.clone(),
    );
    let provider_options = choices
        .iter()
        .map(|provider| {
            DropdownOption::new(format!("review_provider:{}", provider.id), provider.name.as_str())
                .selected(provider.id == review.manual_provider)
        })
        .collect();
    let provider_note = if choices.is_empty() {
        "No hay proveedores conectados con modelos utilizables. Conectá uno en Ajustes → Proveedores y modelos.".to_owned()
    } else if !review.manual_provider.is_empty() && picked.is_none() {
        format!("{} ya no está conectado. La revisión no arranca en modo manual hasta que elijas otro.", review.manual_provider)
    } else {
        "Proveedores conectados ahora.".to_owned()
    };
    let model_label = picked
        .and_then(|provider| provider.models.iter().find(|(id, _)| *id == review.manual_model))
        .map_or_else(
            || if review.manual_model.is_empty() { "Elegir modelo".to_owned() } else { review.manual_model.clone() },
            |(_, name)| name.clone(),
        );
    let model_options = picked
        .map(|provider| {
            provider
                .models
                .iter()
                .map(|(id, name)| {
                    DropdownOption::new(format!("review_pick:{id}"), name.as_str())
                        .selected(*id == review.manual_model)
                })
                .collect()
        })
        .unwrap_or_default();
    let model_note = match picked {
        None => "Elegí primero un proveedor.".to_owned(),
        Some(provider) if provider.omitted > 0 => format!(
            "Se listan {} de {} modelos utilizables. Para otro, seleccionalo en el composer y usá Guardar el seleccionado.",
            provider.models.len(),
            provider.models.len() + provider.omitted
        ),
        Some(_) => "Modelos de ese proveedor que pueden correr un turno.".to_owned(),
    };
    settings_parts::settings_group(
        "Modelo para revisiones",
        vec![
            settings_parts::settings_row(
                "Modelo que usa /review",
                explanation,
                settings_choice_selector(state, "review_model", review.mode.label(), options),
                compact,
            ),
            settings_parts::settings_row(
                "Proveedor del modelo manual",
                provider_note,
                settings_choice_selector(state, "review_provider", &provider_label, provider_options),
                compact,
            ),
            settings_parts::settings_row(
                "Modelo manual",
                model_note,
                settings_choice_selector(state, "review_pick", &model_label, model_options),
                compact,
            ),
            settings_parts::settings_row(
                "Tomar el del composer",
                "Guarda como modelo manual el que tengas seleccionado ahora en el composer.",
                outline("review_model_capture", "Guardar el seleccionado")
                    .icon(Icon::Check)
                    .render(),
                compact,
            ),
            page_note(
                "No cambia tu modelo de trabajo",
                "El modelo de revisión se usa solo en el turno de la revisión. Tu selección en el composer queda como estaba. Para reanudar una revisión interrumpida hay que seleccionar ese mismo modelo.",
                Icon::InfoCircle,
            ),
        ],
    )
}

/// Claude Code plugins that ship skills, each with its own switch.
pub(super) fn plugin_skills_group(state: &ViewState, compact: bool) -> Option<Element> {
    if state.plugin_skills.is_empty() {
        return None;
    }
    let mut rows = vec![page_note(
        "Skills de plugins de Claude Code",
        "Al activar un plugin, el agente nativo recibe el nombre y la descripción de cada skill y puede leer sus archivos cuando la tarea lo pida. No se ejecuta nada del plugin. Si el plugin se actualiza, hay que volver a activarlo.",
        Icon::InfoCircle,
    )];
    for (index, plugin) in state.plugin_skills.iter().enumerate() {
        rows.push(settings_parts::switch_row(
            format!("plugin_skills:{index}"),
            &plugin.id,
            format!("{} skills", plugin.skill_count),
            plugin.enabled,
            true,
            compact,
        ));
    }
    Some(settings_parts::settings_group("Plugins de Claude Code", rows))
}

/// Claude Code hooks found on this machine, each with its own switch.
pub(super) fn compat_hooks_group(state: &ViewState, compact: bool) -> Vec<Element> {
    let mut content = Vec::new();
    if let Some(error) = &state.compat_hooks_error {
        content.push(settings_parts::feedback_banner(error, FeedbackTone::Error, None, compact));
    }
    let mut rows = vec![page_note(
        "Nada se ejecuta sin que lo actives",
        "Cada entrada es un comando definido para Claude Code. Al activarla, openADE lo ejecuta con sh y los permisos de tu cuenta en las llamadas del agente nativo. Si el comando cambia en su archivo de origen, la entrada se desactiva sola.",
        Icon::InfoCircle,
    )];
    if state.compat_hooks.is_empty() {
        rows.push(page_note(
            "Sin hooks de Claude Code",
            "No se encontraron hooks de comandos en la configuración de Claude Code de este equipo.",
            SettingsPage::Hooks.icon(),
        ));
    }
    for (index, hook) in state.compat_hooks.iter().enumerate() {
        let scope = if hook.matcher.is_empty() || hook.matcher == "*" {
            "todas las llamadas".to_owned()
        } else {
            hook.matcher.clone()
        };
        let command: String = hook.command.chars().take(160).collect();
        let description = if hook.runs_today {
            command
        } else {
            // Every event is run today; kept for one that some day is not.
            format!("openADE todavía no ejecuta este evento. {command}")
        };
        let source = if hook.origin.is_empty() {
            "Tu configuración".to_owned()
        } else {
            format!("Plugin {}", hook.origin)
        };
        rows.push(settings_parts::switch_row(
            format!("compat_hook:{index}"),
            format!("{source} · {} · {scope}", hook.event),
            description,
            hook.enabled,
            hook.runs_today,
            compact,
        ));
    }
    content.push(settings_parts::settings_group("Hooks de Claude Code", rows));
    content
}

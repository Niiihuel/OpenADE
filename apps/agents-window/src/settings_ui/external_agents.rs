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
    vec![
        settings_parts::feedback_banner(
            "Funciona con Claude Code y Codex iniciados desde el menú de perfiles de la terminal. Los subagentes todavía no se ofrecen: su interruptor se guarda para cuando estén disponibles.",
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
    ]
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

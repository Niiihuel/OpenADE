//! Automations page. The host owns navigation and persistence; this module
//! renders stable action IDs and validates the editor before it reaches the
//! durable [`crate::automations::AutomationStore`].

use chrono::{Local, TimeZone};
use openide_ui::{
    Button, ButtonVariant, Color, Component, ControlSize, Element, FontFamily, Icon, ScrollArea,
    SearchField, Switch,
};

use crate::{
    automations::{
        parse_local_once, Automation, AutomationDraft, AutomationError, AutomationRun,
        AutomationRunStatus, AutomationSchedule,
    },
    native_provider::NativeProviderSettings,
    theme,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AutomationScreen {
    #[default]
    List,
    Edit,
    History,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AutomationFilter {
    #[default]
    All,
    Active,
    Paused,
    Completed,
    Failed,
}

impl AutomationFilter {
    pub const fn action_id(self) -> &'static str {
        match self {
            Self::All => "automation_filter:all",
            Self::Active => "automation_filter:active",
            Self::Paused => "automation_filter:paused",
            Self::Completed => "automation_filter:completed",
            Self::Failed => "automation_filter:failed",
        }
    }

    pub fn from_action_id(value: &str) -> Option<Self> {
        match value {
            "automation_filter:all" => Some(Self::All),
            "automation_filter:active" => Some(Self::Active),
            "automation_filter:paused" => Some(Self::Paused),
            "automation_filter:completed" => Some(Self::Completed),
            "automation_filter:failed" => Some(Self::Failed),
            _ => None,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::All => "Todas",
            Self::Active => "Activas",
            Self::Paused => "Pausadas",
            Self::Completed => "Completadas",
            Self::Failed => "Con errores",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AutomationFrequency {
    Once,
    EveryMinutes,
    #[default]
    Daily,
    Weekly,
}

impl AutomationFrequency {
    pub const fn action_id(self) -> &'static str {
        match self {
            Self::Once => "automation_frequency:once",
            Self::EveryMinutes => "automation_frequency:interval",
            Self::Daily => "automation_frequency:daily",
            Self::Weekly => "automation_frequency:weekly",
        }
    }

    pub fn from_action_id(value: &str) -> Option<Self> {
        match value {
            "automation_frequency:once" => Some(Self::Once),
            "automation_frequency:interval" => Some(Self::EveryMinutes),
            "automation_frequency:daily" => Some(Self::Daily),
            "automation_frequency:weekly" => Some(Self::Weekly),
            _ => None,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Once => "Una vez",
            Self::EveryMinutes => "Intervalo",
            Self::Daily => "Diaria",
            Self::Weekly => "Semanal",
        }
    }
}

/// Editable local-time fields. The host updates these on `InputChanged`.
#[derive(Clone, Debug)]
pub struct AutomationForm {
    pub title: String,
    pub prompt: String,
    pub backend: String,
    pub native_provider: Option<NativeProviderSettings>,
    pub frequency: AutomationFrequency,
    pub once_at: String,
    pub interval_minutes: String,
    pub time: String,
    /// Monday = 0, as in [`AutomationSchedule::Weekly`].
    pub weekday: u8,
}

impl AutomationForm {
    pub fn new(backend: impl Into<String>) -> Self {
        Self {
            title: String::new(),
            prompt: String::new(),
            backend: backend.into(),
            native_provider: None,
            frequency: AutomationFrequency::Daily,
            once_at: String::new(),
            interval_minutes: "60".to_owned(),
            time: "09:00".to_owned(),
            weekday: 0,
        }
    }

    pub fn from_automation(automation: &Automation) -> Self {
        let mut form = Self::new(&automation.backend);
        form.title.clone_from(&automation.title);
        form.prompt.clone_from(&automation.prompt);
        form.native_provider.clone_from(&automation.native_provider);
        match &automation.schedule {
            AutomationSchedule::Once { at_ms } => {
                form.frequency = AutomationFrequency::Once;
                form.once_at = Local
                    .timestamp_millis_opt(*at_ms)
                    .single()
                    .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
            }
            AutomationSchedule::EveryMinutes { interval, .. } => {
                form.frequency = AutomationFrequency::EveryMinutes;
                form.interval_minutes = interval.to_string();
            }
            AutomationSchedule::Daily { hour, minute } => {
                form.frequency = AutomationFrequency::Daily;
                form.time = format!("{hour:02}:{minute:02}");
            }
            AutomationSchedule::Weekly {
                weekday,
                hour,
                minute,
            } => {
                form.frequency = AutomationFrequency::Weekly;
                form.weekday = *weekday;
                form.time = format!("{hour:02}:{minute:02}");
            }
        }
        form
    }

    pub fn to_draft(&self, now_ms: i64) -> Result<AutomationDraft, AutomationError> {
        let schedule = match self.frequency {
            AutomationFrequency::Once => AutomationSchedule::Once {
                at_ms: parse_local_once(&self.once_at)?,
            },
            AutomationFrequency::EveryMinutes => AutomationSchedule::EveryMinutes {
                interval: self.interval_minutes.trim().parse().map_err(|_| {
                    AutomationError::Invalid(
                        "El intervalo debe ser un número de minutos".to_owned(),
                    )
                })?,
                anchor_ms: now_ms,
            },
            AutomationFrequency::Daily => {
                let (hour, minute) = parse_time(&self.time)?;
                AutomationSchedule::Daily { hour, minute }
            }
            AutomationFrequency::Weekly => {
                let (hour, minute) = parse_time(&self.time)?;
                AutomationSchedule::Weekly {
                    weekday: self.weekday,
                    hour,
                    minute,
                }
            }
        };
        let draft = AutomationDraft {
            title: self.title.clone(),
            prompt: self.prompt.clone(),
            backend: self.backend.clone(),
            native_provider: if self.backend == "native" {
                self.native_provider.clone()
            } else {
                None
            },
            schedule,
        };
        draft.validate()?;
        if draft
            .schedule
            .next_after(now_ms.saturating_sub(1))
            .is_none()
        {
            return Err(AutomationError::Invalid(
                "Elige una fecha y hora futuras".to_owned(),
            ));
        }
        Ok(draft)
    }
}

fn parse_time(value: &str) -> Result<(u8, u8), AutomationError> {
    let mut parts = value.trim().split(':');
    let (Some(hour), Some(minute), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(AutomationError::Invalid("Usa una hora HH:MM".to_owned()));
    };
    let hour: u8 = hour
        .parse()
        .map_err(|_| AutomationError::Invalid("Usa una hora HH:MM".to_owned()))?;
    let minute: u8 = minute
        .parse()
        .map_err(|_| AutomationError::Invalid("Usa una hora HH:MM".to_owned()))?;
    if hour > 23 || minute > 59 {
        return Err(AutomationError::Invalid(
            "Usa una hora válida HH:MM".to_owned(),
        ));
    }
    Ok((hour, minute))
}

/// One source of truth for the page. `items` and `runs` are snapshots from
/// the active workspace's AutomationStore, never demo data.
#[derive(Clone, Debug)]
pub struct AutomationViewState {
    pub workspace: String,
    pub items: Vec<Automation>,
    pub runs: Vec<AutomationRun>,
    pub backends: Vec<(String, String)>,
    pub now_ms: i64,
    pub screen: AutomationScreen,
    pub selected_id: Option<String>,
    pub form: AutomationForm,
    pub filter: AutomationFilter,
    pub query: String,
    pub busy_id: Option<String>,
    pub pending_delete_id: Option<String>,
    pub notice: Option<String>,
    pub error: Option<String>,
}

impl AutomationViewState {
    pub fn new(workspace: impl Into<String>, backends: Vec<(String, String)>, now_ms: i64) -> Self {
        let backend = backends
            .first()
            .map(|item| item.0.clone())
            .unwrap_or_default();
        Self {
            workspace: workspace.into(),
            items: Vec::new(),
            runs: Vec::new(),
            backends,
            now_ms,
            screen: AutomationScreen::List,
            selected_id: None,
            form: AutomationForm::new(backend),
            filter: AutomationFilter::All,
            query: String::new(),
            busy_id: None,
            pending_delete_id: None,
            notice: None,
            error: None,
        }
    }

    pub fn selected(&self) -> Option<&Automation> {
        self.selected_id
            .as_ref()
            .and_then(|id| self.items.iter().find(|item| &item.id == id))
    }

    pub fn visible_items(&self) -> Vec<&Automation> {
        let query = self.query.trim().to_lowercase();
        let mut items: Vec<_> = self
            .items
            .iter()
            .filter(|item| {
                (query.is_empty()
                    || item.title.to_lowercase().contains(&query)
                    || item.prompt.to_lowercase().contains(&query))
                    && (self.filter == AutomationFilter::All
                        || self.filter == status_for(item, &self.runs).filter())
            })
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        items
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutomationStatus {
    Active,
    Paused,
    Completed,
    Failed,
}

impl AutomationStatus {
    const fn filter(self) -> AutomationFilter {
        match self {
            Self::Active => AutomationFilter::Active,
            Self::Paused => AutomationFilter::Paused,
            Self::Completed => AutomationFilter::Completed,
            Self::Failed => AutomationFilter::Failed,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Active => "Activa",
            Self::Paused => "Pausada",
            Self::Completed => "Completada",
            Self::Failed => "Con error",
        }
    }
}

fn status_for(item: &Automation, runs: &[AutomationRun]) -> AutomationStatus {
    let last = runs
        .iter()
        .filter(|run| run.automation_id == item.id)
        .max_by_key(|run| run.created_at);
    if matches!(
        last.map(|run| run.status),
        Some(AutomationRunStatus::Failed | AutomationRunStatus::Interrupted)
    ) {
        AutomationStatus::Failed
    } else if item.enabled {
        AutomationStatus::Active
    } else if item.next_run_at.is_none() {
        AutomationStatus::Completed
    } else {
        AutomationStatus::Paused
    }
}

#[derive(Clone, Copy)]
pub struct AutomationTemplate {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub prompt: &'static str,
    pub frequency: AutomationFrequency,
    pub time: &'static str,
    pub weekday: u8,
    pub icon: Icon,
}

pub const TEMPLATES: [AutomationTemplate; 4] = [
    AutomationTemplate {
        id: "morning_brief",
        title: "Resumen de desarrollo",
        description: "Commits, cambios de módulos y estado de CI desde el día anterior.",
        prompt: "Resume los commits, cambios de módulos y estado de CI desde el día hábil anterior. Señala riesgos y próximos pasos concretos.",
        frequency: AutomationFrequency::Weekly,
        time: "09:00",
        weekday: 0,
        icon: Icon::Clock,
    },
    AutomationTemplate {
        id: "risk_scan",
        title: "Inspección de riesgos",
        description: "Busca regresiones y riesgos importantes en los cambios recientes.",
        prompt: "Revisa los cambios de código de las últimas 24 horas. Identifica riesgos de fallos, pérdida de datos o seguridad con evidencia y prioridad.",
        frequency: AutomationFrequency::Daily,
        time: "10:00",
        weekday: 0,
        icon: Icon::Search,
    },
    AutomationTemplate {
        id: "release_brief",
        title: "Resumen de versión",
        description: "Agrupa PRs y commits por mejoras, correcciones y experiencia.",
        prompt: "Agrupa los PRs y commits integrados esta semana por mejoras, correcciones y experiencia de usuario. Redacta un resumen de publicación.",
        frequency: AutomationFrequency::Weekly,
        time: "16:00",
        weekday: 4,
        icon: Icon::Code,
    },
    AutomationTemplate {
        id: "docs_sync",
        title: "Documentación al día",
        description: "Contrasta código, configuración y documentación reciente.",
        prompt: "Compara el código y la configuración de los últimos siete días con la documentación. Enumera discrepancias comprobables y mejoras sugeridas.",
        frequency: AutomationFrequency::Weekly,
        time: "15:00",
        weekday: 2,
        icon: Icon::Folder,
    },
];

pub fn template_form(id: &str, backend: impl Into<String>) -> Option<AutomationForm> {
    let template = TEMPLATES.iter().find(|template| template.id == id)?;
    let mut form = AutomationForm::new(backend);
    form.title = template.title.to_owned();
    form.prompt = template.prompt.to_owned();
    form.frequency = template.frequency;
    form.time = template.time.to_owned();
    form.weekday = template.weekday;
    Some(form)
}

fn text(value: impl Into<String>, size: f32, color: Color) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(size)
        .foreground(color)
}

fn muted(value: impl Into<String>) -> Element {
    text(value, 13.0, theme::FOREGROUND_SUBTLE)
}

fn divider() -> Element {
    Element::spacer()
        .size(None, Some(1.0))
        .background(theme::BORDER)
}

fn centered(element: Element) -> Element {
    Element::row(vec![
        Element::spacer().flex(1.0),
        element,
        Element::spacer().flex(1.0),
    ])
}

fn action(
    id: impl Into<String>,
    title: impl Into<String>,
    icon: Option<Icon>,
    variant: ButtonVariant,
) -> Element {
    let palette = if variant == ButtonVariant::Solid {
        theme::action_palette(theme::BRAND, theme::PRIMARY_FOREGROUND)
    } else {
        theme::component_palette()
    };
    let mut button = Button::new(id, title, palette)
        .variant(variant)
        .size(ControlSize::Spacious);
    if let Some(icon) = icon {
        button = button.icon(icon);
    }
    button.render().font_size(13.0)
}

fn field(id: &str, value: &str, placeholder: &str, height: f32) -> Element {
    Element::text_input(id, value, placeholder)
        .background(theme::CARD)
        .hover_background(theme::hover_for(theme::CARD))
        .border(theme::BORDER, 1.0)
        .focus_border(theme::FOREGROUND_SUBTLE, 1.0)
        .foreground(theme::FOREGROUND)
        .font_family(FontFamily::Sans)
        .font_size(14.0)
        .padding(11.0)
        .radius(9.0)
        .size(None, Some(height))
}

fn section_label(value: &str) -> Element {
    text(value, 14.0, theme::FOREGROUND_SUBTLE)
}

fn form_label(value: &str) -> Element {
    text(value, 13.0, theme::FOREGROUND).min_size(0.0, 22.0)
}

fn automation_card(
    item: &Automation,
    runs: &[AutomationRun],
    width: f32,
    busy: bool,
    pending_delete: bool,
) -> Element {
    let status = status_for(item, runs);
    let next = if status == AutomationStatus::Active {
        item.next_run_at
            .and_then(|at| Local.timestamp_millis_opt(at).single())
            .map(|at| format!(" · Próxima {}", at.format("%d/%m %H:%M")))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let summary = format!("{}{}", item.schedule.summary(), next);
    let status_color = if status == AutomationStatus::Failed {
        theme::DESTRUCTIVE
    } else if status == AutomationStatus::Active {
        theme::FOREGROUND
    } else {
        theme::FOREGROUND_SUBTLE
    };
    let status_badge = Element::panel(text(status.title(), 11.0, status_color))
        .padding(6.0)
        .background(theme::SELECTED)
        .radius(6.0);
    let mut rows = vec![
        Element::row(vec![
            Element::truncated_text(&item.title)
                .foreground(theme::FOREGROUND)
                .font_family(FontFamily::Sans)
                .font_size(15.0)
                .flex(1.0),
            status_badge,
        ])
        .gap(8.0),
        Element::truncated_text(&item.prompt)
            .foreground(theme::FOREGROUND_SUBTLE)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .min_size(0.0, 24.0),
        Element::row(vec![
            Element::icon(Icon::Clock).foreground(theme::FOREGROUND_SUBTLE),
            Element::truncated_text(summary)
                .foreground(theme::FOREGROUND_SUBTLE)
                .font_family(FontFamily::Sans)
                .font_size(12.0)
                .flex(1.0),
            muted(format!("{} ejec.", item.run_count)).font_size(11.0),
        ])
        .gap(6.0),
    ];
    if pending_delete {
        rows.push(text("¿Eliminar esta tarea?", 12.0, theme::FOREGROUND));
        rows.push(
            Element::row(vec![
                Element::spacer().flex(1.0),
                action(
                    "automation_delete_cancel",
                    "Cancelar",
                    None,
                    ButtonVariant::Ghost,
                ),
                action(
                    format!("automation_delete_confirm:{}", item.id),
                    "Eliminar",
                    Some(Icon::Trash),
                    ButtonVariant::Outline,
                ),
            ])
            .gap(4.0),
        );
    } else {
        let edit = action(
            format!("automation_edit:{}", item.id),
            "Editar",
            Some(Icon::Edit),
            ButtonVariant::Ghost,
        );
        let history = action(
            format!("automation_history:{}", item.id),
            "Historial",
            Some(Icon::History),
            ButtonVariant::Ghost,
        );
        let run = if busy {
            Button::new(
                format!("automation_run:{}", item.id),
                "Iniciando…",
                theme::component_palette(),
            )
            .icon(Icon::ArrowUp)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Spacious)
            .disabled()
            .render()
        } else {
            action(
                format!("automation_run:{}", item.id),
                "Ejecutar",
                Some(Icon::ArrowUp),
                ButtonVariant::Ghost,
            )
        };
        let enabled = Switch::new(
            format!("automation_toggle:{}", item.id),
            item.enabled,
            theme::component_palette(),
        )
        .render();
        if width < 440.0 {
            rows.push(Element::row(vec![edit, history, Element::spacer().flex(1.0)]).gap(5.0));
            rows.push(Element::row(vec![run, Element::spacer().flex(1.0), enabled]).gap(5.0));
        } else {
            rows.push(
                Element::row(vec![
                    edit,
                    history,
                    Element::spacer().flex(1.0),
                    run,
                    enabled,
                ])
                .gap(6.0),
            );
        }
        // Destructive action is distinct from the card's primary actions.
        rows.push(
            action(
                format!("automation_delete_request:{}", item.id),
                "Eliminar tarea",
                Some(Icon::Trash),
                ButtonVariant::Ghost,
            )
            .foreground(theme::FOREGROUND_SUBTLE)
            .size(None, Some(25.0)),
        );
    }
    Element::panel(Element::column(rows).gap(8.0))
        .padding(14.0)
        .background(theme::BACKGROUND)
        .border(theme::BORDER, 1.0)
        .radius(12.0)
        .size(Some(width), None)
        .min_size(0.0, 160.0)
}

fn template_card(template: &AutomationTemplate, width: f32) -> Element {
    let schedule = match template.frequency {
        AutomationFrequency::Daily => format!("Todos los días a las {}", template.time),
        AutomationFrequency::Weekly => {
            let day = [
                "lunes",
                "martes",
                "miércoles",
                "jueves",
                "viernes",
                "sábado",
                "domingo",
            ][usize::from(template.weekday)];
            format!("Cada {day} a las {}", template.time)
        }
        _ => String::new(),
    };
    Element::clickable(
        format!("automation_template:{}", template.id),
        Element::column(vec![
            Element::row(vec![
                Element::icon(template.icon).foreground(theme::FOREGROUND),
                text(template.title, 14.0, theme::FOREGROUND),
            ])
            .gap(8.0),
            muted(template.description),
            Element::spacer().flex(1.0),
            text(schedule, 12.0, theme::FOREGROUND_SUBTLE),
        ])
        .gap(8.0),
    )
    .background(theme::BACKGROUND)
    .hover_background(theme::HOVER)
    .pressed_background(theme::SELECTED)
    .transition_ms(140)
    .border(theme::BORDER, 1.0)
    .radius(12.0)
    .padding(14.0)
    .size(Some(width), Some(130.0))
}

fn cards_grid(items: Vec<Element>, content_width: f32, gap: f32) -> Element {
    if content_width < 690.0 {
        Element::column(items).gap(gap)
    } else {
        let mut rows = Vec::new();
        let mut items = items.into_iter();
        while let Some(first) = items.next() {
            let second = items.next().unwrap_or_else(Element::spacer);
            rows.push(Element::row(vec![first, second]).gap(gap));
        }
        Element::column(rows).gap(gap)
    }
}

fn list_page(state: &AutomationViewState, content_width: f32) -> Vec<Element> {
    let compact = content_width < 690.0;
    let card_width = if compact {
        content_width
    } else {
        (content_width - 14.0) / 2.0
    };
    let heading = text("Automatizaciones", 30.0, theme::FOREGROUND);
    let heading = if state.items.is_empty() {
        heading
    } else if content_width < 440.0 {
        Element::column(vec![
            heading,
            action(
                "automation_new",
                "Nueva tarea",
                Some(Icon::Add),
                ButtonVariant::Solid,
            ),
        ])
        .gap(12.0)
    } else {
        Element::row(vec![
            heading,
            Element::spacer().flex(1.0),
            action(
                "automation_new",
                "Nueva tarea",
                Some(Icon::Add),
                ButtonVariant::Solid,
            ),
        ])
        .gap(12.0)
    };
    let mut body = vec![
        heading,
        muted("Programa tareas para trabajar en tu proyecto mientras la aplicación está abierta."),
        Element::spacer().size(None, Some(8.0)),
    ];
    let visible = state.visible_items();
    if state.items.is_empty() {
        body.push(
            Element::panel(
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    centered(text(
                        "Todavía no hay tareas programadas",
                        16.0,
                        theme::FOREGROUND_SUBTLE,
                    )),
                    centered(muted(
                        "Crea una tarea recurrente o empieza desde una plantilla.",
                    )),
                    centered(action(
                        "automation_new",
                        "Crear tarea programada",
                        Some(Icon::Add),
                        ButtonVariant::Solid,
                    )),
                    Element::spacer().flex(1.0),
                ])
                .gap(12.0),
            )
            .padding(18.0)
            .background(theme::BACKGROUND)
            .border(theme::BORDER, 1.0)
            .radius(16.0)
            .size(Some(content_width), Some(226.0)),
        );
    } else {
        body.push(
            SearchField::new(
                "automation_query",
                &state.query,
                "Buscar tareas programadas…",
                theme::component_palette(),
            )
            .size(ControlSize::Spacious)
            .width(content_width)
            .render(),
        );
        let filters = [
            AutomationFilter::All,
            AutomationFilter::Active,
            AutomationFilter::Paused,
            AutomationFilter::Completed,
            AutomationFilter::Failed,
        ];
        let filter_buttons: Vec<_> = filters
            .into_iter()
            .map(|filter| {
                let active = state.filter == filter;
                Button::new(
                    filter.action_id(),
                    filter.title(),
                    theme::component_palette(),
                )
                .variant(if active {
                    ButtonVariant::Raised
                } else {
                    ButtonVariant::Ghost
                })
                .size(ControlSize::Compact)
                .active(active)
                .render()
                .font_size(12.0)
            })
            .collect();
        if content_width < 490.0 {
            let mut filter_buttons = filter_buttons.into_iter();
            body.push(
                Element::column(vec![
                    Element::row(vec![
                        filter_buttons.next().unwrap(),
                        filter_buttons.next().unwrap(),
                        filter_buttons.next().unwrap(),
                    ])
                    .gap(5.0),
                    Element::row(vec![
                        filter_buttons.next().unwrap(),
                        filter_buttons.next().unwrap(),
                    ])
                    .gap(5.0),
                ])
                .gap(4.0),
            );
        } else {
            body.push(Element::row(filter_buttons).gap(5.0));
        }
        if visible.is_empty() {
            body.push(
                Element::panel(muted(
                    "No hay tareas que coincidan con el filtro o la búsqueda.",
                ))
                .padding(24.0)
                .background(theme::BACKGROUND)
                .border(theme::BORDER, 1.0)
                .radius(12.0)
                .size(Some(content_width), Some(92.0)),
            );
        } else {
            let cards = visible
                .into_iter()
                .map(|item| {
                    automation_card(
                        item,
                        &state.runs,
                        card_width,
                        state.busy_id.as_deref() == Some(&item.id),
                        state.pending_delete_id.as_deref() == Some(&item.id),
                    )
                })
                .collect();
            body.push(cards_grid(cards, content_width, 14.0));
        }
    }
    body.extend([
        Element::spacer().size(None, Some(10.0)),
        divider(),
        section_label("Plantillas de tareas programadas"),
        cards_grid(
            TEMPLATES
                .iter()
                .map(|template| template_card(template, card_width))
                .collect(),
            content_width,
            14.0,
        ),
    ]);
    body
}

fn frequency_control(form: &AutomationForm, available_width: f32) -> Element {
    let frequencies = [
        AutomationFrequency::Once,
        AutomationFrequency::EveryMinutes,
        AutomationFrequency::Daily,
        AutomationFrequency::Weekly,
    ];
    let columns = if available_width < 520.0 { 2 } else { 4 };
    let button_width = (available_width - 8.0 - 6.0 * (columns - 1) as f32) / columns as f32;
    let buttons: Vec<_> = frequencies
        .into_iter()
        .map(|frequency| {
            Button::new(
                frequency.action_id(),
                frequency.title(),
                theme::component_palette(),
            )
            .variant(if form.frequency == frequency {
                ButtonVariant::Raised
            } else {
                ButtonVariant::Ghost
            })
            .size(ControlSize::Spacious)
            .width(button_width)
            .active(form.frequency == frequency)
            .render()
        })
        .collect();
    let content = if columns == 2 {
        let mut buttons = buttons.into_iter();
        Element::column(vec![
            Element::row(vec![buttons.next().unwrap(), buttons.next().unwrap()]).gap(6.0),
            Element::row(vec![buttons.next().unwrap(), buttons.next().unwrap()]).gap(6.0),
        ])
        .gap(5.0)
    } else {
        Element::row(buttons).gap(6.0)
    };
    content.padding(4.0).background(theme::CARD).radius(11.0)
}

fn schedule_editor(form: &AutomationForm, content_width: f32) -> Vec<Element> {
    let mut rows = vec![
        form_label("Frecuencia"),
        frequency_control(form, content_width - 36.0),
    ];
    match form.frequency {
        AutomationFrequency::Once => rows.extend([
            form_label("Fecha y hora local"),
            field(
                "automation_once_at",
                &form.once_at,
                "AAAA-MM-DD HH:MM",
                42.0,
            ),
        ]),
        AutomationFrequency::EveryMinutes => rows.extend([
            form_label("Cada cuántos minutos"),
            field("automation_interval", &form.interval_minutes, "60", 42.0),
        ]),
        AutomationFrequency::Daily => rows.extend([
            form_label("Hora local"),
            field("automation_time", &form.time, "09:00", 42.0),
        ]),
        AutomationFrequency::Weekly => {
            let days = ["L", "M", "X", "J", "V", "S", "D"];
            rows.push(form_label("Día de la semana"));
            rows.push(
                Element::row(
                    days.into_iter()
                        .enumerate()
                        .map(|(index, day)| {
                            Button::new(
                                format!("automation_weekday:{index}"),
                                day,
                                theme::component_palette(),
                            )
                            .variant(if usize::from(form.weekday) == index {
                                ButtonVariant::Raised
                            } else {
                                ButtonVariant::Ghost
                            })
                            .size(ControlSize::Compact)
                            .active(usize::from(form.weekday) == index)
                            .render()
                        })
                        .collect(),
                )
                .gap(6.0),
            );
            rows.push(form_label("Hora local"));
            rows.push(field("automation_time", &form.time, "09:00", 42.0));
        }
    }
    rows
}

fn editor_page(state: &AutomationViewState, content_width: f32) -> Vec<Element> {
    let existing = state.selected_id.is_some();
    let mut schedule = schedule_editor(&state.form, content_width);
    let backend_options = if state.backends.is_empty() {
        vec![muted("No hay agentes disponibles para programar tareas.")]
    } else {
        state
            .backends
            .iter()
            .map(|(id, title)| {
                Button::new(
                    format!("automation_backend:{id}"),
                    title,
                    theme::component_palette(),
                )
                .variant(if state.form.backend == *id {
                    ButtonVariant::Raised
                } else {
                    ButtonVariant::Ghost
                })
                .size(ControlSize::Regular)
                .active(state.form.backend == *id)
                .render()
            })
            .collect()
    };
    let mut backend_rows = vec![
        form_label("Motor del agente"),
        Element::row(backend_options).gap(8.0),
    ];
    if state.form.backend == "native" {
        let selected = state
            .form
            .native_provider
            .as_ref()
            .map(|settings| format!("{} · {}", settings.provider_id, settings.model_id))
            .unwrap_or_else(|| "Configuración nativa actual".to_owned());
        backend_rows.push(muted(format!("Modelo de esta tarea: {selected}")));
        backend_rows.push(action(
            "automation_native_current",
            "Usar modelo actual de Settings",
            None,
            ButtonVariant::Ghost,
        ));
    } else {
        backend_rows.push(muted("Se usa la CLI detectada en PATH para este equipo."));
    }
    let mut body = vec![
        action("automation_back", "Volver", Some(Icon::ArrowLeft), ButtonVariant::Ghost),
        text(
            if existing { "Editar tarea" } else { "Nueva tarea programada" },
            29.0,
            theme::FOREGROUND,
        ),
        muted("El agente iniciará una sesión nueva en este proyecto cuando llegue el horario."),
        Element::spacer().size(None, Some(8.0)),
        Element::panel(
            Element::column(vec![
                form_label("Título"),
                field("automation_title", &state.form.title, "Ej. Resumen diario", 44.0),
                form_label("Instrucciones"),
                Element::text_area(
                    "automation_prompt",
                    &state.form.prompt,
                    "Describe el resultado esperado y los límites de esta tarea…",
                )
                .background(theme::CARD)
                .border(theme::BORDER, 1.0)
                .focus_border(theme::FOREGROUND_SUBTLE, 1.0)
                .foreground(theme::FOREGROUND)
                .font_family(FontFamily::Sans)
                .font_size(14.0)
                .padding(12.0)
                .radius(10.0)
                .size(None, Some(144.0)),
                muted("Las ejecuciones sin supervisión pueden esperar una aprobación antes de modificar archivos."),
            ])
            .gap(10.0),
        )
        .padding(18.0)
        .background(theme::BACKGROUND)
        .border(theme::BORDER, 1.0)
        .radius(13.0),
        Element::panel(Element::column(std::mem::take(&mut schedule)).gap(10.0))
            .padding(18.0)
            .background(theme::BACKGROUND)
            .border(theme::BORDER, 1.0)
            .radius(13.0),
        Element::panel(Element::column(backend_rows).gap(10.0))
            .padding(18.0)
            .background(theme::BACKGROUND)
            .border(theme::BORDER, 1.0)
            .radius(13.0),
        Element::row(vec![
            Element::spacer().flex(1.0),
            action("automation_cancel", "Cancelar", None, ButtonVariant::Ghost),
            action(
                "automation_save",
                if existing { "Guardar cambios" } else { "Crear tarea" },
                Some(Icon::Check),
                ButtonVariant::Solid,
            ),
        ])
        .gap(10.0),
    ];
    if let Some(error) = &state.error {
        body.insert(
            4,
            Element::panel(text(error, 13.0, theme::DESTRUCTIVE))
                .background(theme::CARD)
                .border(theme::DESTRUCTIVE, 1.0)
                .padding(12.0)
                .radius(9.0),
        );
    }
    body
}

fn history_page(state: &AutomationViewState) -> Vec<Element> {
    let Some(item) = state.selected() else {
        return vec![
            action(
                "automation_back",
                "Volver",
                Some(Icon::ArrowLeft),
                ButtonVariant::Ghost,
            ),
            muted("Esta tarea ya no existe."),
        ];
    };
    let mut runs: Vec<_> = state
        .runs
        .iter()
        .filter(|run| run.automation_id == item.id)
        .collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
    let mut body = vec![
        action(
            "automation_back",
            "Volver",
            Some(Icon::ArrowLeft),
            ButtonVariant::Ghost,
        ),
        text(
            format!("Historial · {}", item.title),
            27.0,
            theme::FOREGROUND,
        ),
        muted("Cada ejecución queda asociada a una sesión; los fallos conservan su diagnóstico."),
        Element::spacer().size(None, Some(8.0)),
    ];
    if runs.is_empty() {
        body.push(
            Element::panel(muted("Aún no hay ejecuciones de esta tarea."))
                .padding(28.0)
                .background(theme::BACKGROUND)
                .border(theme::BORDER, 1.0)
                .radius(12.0),
        );
    } else {
        for run in runs {
            let (status, color) = match run.status {
                AutomationRunStatus::Pending => ("Iniciando", theme::FOREGROUND_SUBTLE),
                AutomationRunStatus::Dispatched => ("Sesión creada", theme::FOREGROUND),
                AutomationRunStatus::Failed => ("Falló", theme::DESTRUCTIVE),
                AutomationRunStatus::Skipped => ("Omitida", theme::FOREGROUND_SUBTLE),
                AutomationRunStatus::Interrupted => ("Interrumpida", theme::DESTRUCTIVE),
            };
            let when = Local
                .timestamp_millis_opt(run.scheduled_at)
                .single()
                .map(|time| time.format("%d/%m/%Y %H:%M").to_string())
                .unwrap_or_else(|| "Fecha desconocida".to_owned());
            let mut details = vec![
                Element::row(vec![
                    text(status, 14.0, color),
                    Element::spacer().flex(1.0),
                    muted(when),
                ]),
                muted(if run.manual {
                    "Inicio manual"
                } else {
                    "Programada"
                }),
            ];
            if let Some(error) = &run.error {
                details.push(text(error, 12.0, theme::DESTRUCTIVE));
            }
            if let Some(session_id) = &run.session_id {
                details.push(action(
                    format!("automation_open_session:{session_id}"),
                    "Abrir sesión",
                    Some(Icon::ArrowRight),
                    ButtonVariant::Ghost,
                ));
            }
            body.push(
                Element::panel(Element::column(details).gap(7.0))
                    .padding(14.0)
                    .background(theme::BACKGROUND)
                    .border(theme::BORDER, 1.0)
                    .radius(11.0),
            );
        }
    }
    body
}

/// Responsive page body for the center dock. `width` is its available width
/// after the shared workspace border, not the entire window width.
pub fn build_automations(state: &AutomationViewState, width: f32) -> Element {
    let gutter = if width < 640.0 { 22.0 } else { 44.0 };
    let content_width = (width - 2.0 * gutter).clamp(240.0, 980.0);
    let mut body = match state.screen {
        AutomationScreen::List => list_page(state, content_width),
        AutomationScreen::Edit => editor_page(state, content_width),
        AutomationScreen::History => history_page(state),
    };
    if let Some(notice) = &state.notice {
        body.insert(
            0,
            Element::panel(text(notice, 12.0, theme::FOREGROUND))
                .padding(10.0)
                .background(theme::CARD)
                .border(theme::BORDER, 1.0)
                .radius(8.0),
        );
    }
    if state.screen == AutomationScreen::List {
        if let Some(error) = &state.error {
            body.insert(
                0,
                Element::panel(text(error, 12.0, theme::DESTRUCTIVE))
                    .padding(10.0)
                    .background(theme::CARD)
                    .border(theme::DESTRUCTIVE, 1.0)
                    .radius(8.0),
            );
        }
    }
    let content = Element::column(body)
        .gap(17.0)
        .size(Some(content_width), None);
    let centered = Element::row(vec![
        Element::spacer().flex(1.0),
        content,
        Element::spacer().flex(1.0),
    ])
    .padding(gutter);
    ScrollArea::new("automation_page", centered)
        .tint(theme::FOREGROUND_SUBTLE)
        .render()
        .flex(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, Point, Size, UiEvent, UiTree};

    fn state() -> AutomationViewState {
        AutomationViewState::new(
            "C:/workspace",
            vec![("codex".into(), "Codex".into())],
            1_000_000,
        )
    }

    fn sample() -> Automation {
        Automation {
            id: "one".into(),
            title: "Revisión diaria".into(),
            prompt: "Revisa errores".into(),
            backend: "codex".into(),
            native_provider: None,
            schedule: AutomationSchedule::Daily { hour: 9, minute: 0 },
            enabled: true,
            next_run_at: Some(86_400_000),
            last_run_at: None,
            run_count: 0,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn form_parses_schedule_and_rejects_invalid_local_input() {
        let mut form = AutomationForm::new("codex");
        form.title = "Resumen".into();
        form.prompt = "Resume cambios".into();
        form.frequency = AutomationFrequency::Weekly;
        form.weekday = 4;
        form.time = "16:30".into();
        assert_eq!(
            form.to_draft(1_000_000).unwrap().schedule,
            AutomationSchedule::Weekly {
                weekday: 4,
                hour: 16,
                minute: 30,
            }
        );
        form.time = "34:82".into();
        assert!(form.to_draft(1_000_000).is_err());
        form.frequency = AutomationFrequency::EveryMinutes;
        form.interval_minutes = "0".into();
        assert!(form.to_draft(1_000_000).is_err());
    }

    #[test]
    fn native_form_keeps_its_provider_and_model_when_saved() {
        let mut form = AutomationForm::new("native");
        form.title = "Resumen".into();
        form.prompt = "Resume cambios".into();
        form.native_provider = Some(NativeProviderSettings {
            provider_id: "deepseek".into(),
            model_id: "deepseek-flash".into(),
            custom_endpoint: "https://api.openai.com/v1".into(),
        });
        let draft = form.to_draft(1_000_000).unwrap();
        assert_eq!(draft.native_provider, form.native_provider);
    }

    #[test]
    fn template_prefills_real_editable_draft() {
        let form = template_form("risk_scan", "codex").unwrap();
        assert_eq!(form.frequency, AutomationFrequency::Daily);
        assert_eq!(form.time, "10:00");
        assert!(form.to_draft(1_000_000).is_ok());
        assert!(template_form("missing", "codex").is_none());
    }

    #[test]
    fn list_empty_and_editor_expose_expected_actions() {
        let mut state = state();
        let empty = UiTree::new(build_automations(&state, 960.0)).layout(Size::new(960.0, 780.0));
        assert!(empty.regions.iter().any(|r| r.id == "automation_new"));
        assert!(empty
            .regions
            .iter()
            .any(|r| r.id == "automation_template:risk_scan"));
        state.screen = AutomationScreen::Edit;
        let mut tree = UiTree::new(build_automations(&state, 960.0));
        let editor = tree.layout(Size::new(960.0, 780.0));
        for id in [
            "automation_title",
            "automation_prompt",
            "automation_frequency:weekly",
        ] {
            assert!(editor.regions.iter().any(|r| r.id == id), "missing {id}");
        }
        tree.handle_event(UiEvent::Scroll {
            position: Point::new(930.0, 500.0),
            delta_y: -2_000.0,
        });
        let scrolled = tree.layout(Size::new(960.0, 780.0));
        assert!(scrolled.regions.iter().any(|r| r.id == "automation_save"));
    }

    #[test]
    fn filter_and_history_keep_runs_scoped_to_selected_automation() {
        let mut state = state();
        state.items.push(sample());
        state.filter = AutomationFilter::Active;
        assert_eq!(state.visible_items().len(), 1);
        state.filter = AutomationFilter::Paused;
        assert!(state.visible_items().is_empty());
        state.filter = AutomationFilter::All;
        state.query = "errores".into();
        assert_eq!(state.visible_items().len(), 1);
        state.query = "nada".into();
        assert!(state.visible_items().is_empty());
        state.query.clear();
        state.selected_id = Some("one".into());
        state.screen = AutomationScreen::History;
        let history = UiTree::new(build_automations(&state, 760.0)).layout(Size::new(760.0, 720.0));
        assert!(history.regions.iter().any(|r| r.id == "automation_back"));
    }

    #[test]
    fn compact_page_keeps_controls_inside_the_center_dock() {
        let mut state = state();
        state.items.push(sample());
        let viewport = Size::new(320.0, 720.0);
        let frame = UiTree::new(build_automations(&state, viewport.width)).layout(viewport);
        for region in &frame.regions {
            assert!(region.rect.x >= -0.5, "{} starts outside", region.id);
            assert!(
                region.rect.x + region.rect.width <= viewport.width + 0.5,
                "{} ends outside at {}",
                region.id,
                region.rect.x + region.rect.width
            );
        }
        state.screen = AutomationScreen::Edit;
        let editor = UiTree::new(build_automations(&state, viewport.width)).layout(viewport);
        for region in &editor.regions {
            assert!(
                region.rect.x + region.rect.width <= viewport.width + 0.5,
                "{} ends outside editor",
                region.id
            );
        }
    }

    #[test]
    fn run_now_is_not_clickable_while_its_automation_is_busy() {
        let mut state = state();
        state.items.push(sample());
        let viewport = Size::new(960.0, 780.0);
        let ready = UiTree::new(build_automations(&state, viewport.width)).layout(viewport);
        assert!(ready.regions.iter().any(|r| r.id == "automation_run:one"));

        state.busy_id = Some("one".into());
        let busy = UiTree::new(build_automations(&state, viewport.width)).layout(viewport);
        assert!(!busy.regions.iter().any(|r| r.id == "automation_run:one"));
        assert!(busy.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { text, .. } if text == "Iniciando…"
        )));
    }
}

//! Automations page. The host owns navigation and persistence; this module
//! renders stable action IDs and validates the editor before it reaches the
//! durable [`crate::automations::AutomationStore`].

use chrono::{Datelike, Local, TimeZone};
use openide_ui::{
    Alert, AlertTone, Button, ButtonGroup, ButtonVariant, Card, CardGrid, Component, ControlSize,
    DateTimePicker, Element, Empty, FontFamily, FormField, FormFieldSizing, Icon, IconButton,
    Modal, PageColumn, PageHeader, SearchField, Separator, Step, Stepper, Switch, Tab, Tabs, Text, TextArea,
    TextField, TextRole, TimePicker, Tooltip, TooltipPlacement, TooltipSpec,
};

use crate::{
    automations::{
        parse_local_once, Automation, AutomationDraft, AutomationError, AutomationRun,
        AutomationRunStatus, AutomationSchedule,
    },
    native_automation_result::NativeAutomationOutcome,
    native_provider::NativeProviderSettings,
    theme,
    ui_sizing::{AUTOMATION_FIELD, AUTOMATION_SCHEDULE_FIELD},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AutomationScreen {
    #[default]
    List,
    Edit,
    History,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AutomationEditorStep {
    #[default]
    Content,
    Schedule,
    Agent,
}

impl AutomationEditorStep {
    pub const fn index(self) -> usize {
        match self {
            Self::Content => 0,
            Self::Schedule => 1,
            Self::Agent => 2,
        }
    }

    pub const fn previous(self) -> Self {
        match self {
            Self::Content | Self::Schedule => Self::Content,
            Self::Agent => Self::Schedule,
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Content => Self::Schedule,
            Self::Schedule | Self::Agent => Self::Agent,
        }
    }

    pub fn from_action_id(id: &str) -> Option<Self> {
        match id {
            "automation_step:content" => Some(Self::Content),
            "automation_step:schedule" => Some(Self::Schedule),
            "automation_step:agent" => Some(Self::Agent),
            _ => None,
        }
    }
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
            Self::Completed => "Sin próximas",
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

    pub fn once_parts(&self) -> (&str, &str) {
        match self.once_at.split_once(' ') {
            Some((date, time)) => (date, time),
            None if self.once_at.is_empty() => ("", "09:00"),
            None => (&self.once_at, "09:00"),
        }
    }

    pub fn set_once_date(&mut self, date: &str) {
        let time = self.once_parts().1.to_owned();
        self.once_at = format!("{date} {time}");
    }

    pub fn set_once_time(&mut self, time: &str) {
        let date = self.once_parts().0.to_owned();
        self.once_at = format!("{date} {time}");
    }

    fn parsed_schedule(&self, now_ms: i64) -> Result<AutomationSchedule, AutomationError> {
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
        schedule.validate()?;
        if schedule.next_after(now_ms.saturating_sub(1)).is_none() {
            return Err(AutomationError::Invalid(
                "Elige una fecha y hora futuras".to_owned(),
            ));
        }
        Ok(schedule)
    }

    pub fn validate_step(
        &self,
        step: AutomationEditorStep,
        now_ms: i64,
    ) -> Result<(), AutomationError> {
        match step {
            AutomationEditorStep::Content => {
                if self.title.trim().is_empty() || self.title.trim().chars().count() > 100 {
                    return Err(AutomationError::Invalid(
                        "El título debe tener entre 1 y 100 caracteres".to_owned(),
                    ));
                }
                if self.prompt.trim().is_empty() || self.prompt.trim().chars().count() > 10_000 {
                    return Err(AutomationError::Invalid(
                        "La instrucción debe tener entre 1 y 10 000 caracteres".to_owned(),
                    ));
                }
                Ok(())
            }
            AutomationEditorStep::Schedule => self.parsed_schedule(now_ms).map(|_| ()),
            AutomationEditorStep::Agent => self.to_draft(now_ms).map(|_| ()),
        }
    }

    pub fn to_draft(&self, now_ms: i64) -> Result<AutomationDraft, AutomationError> {
        let schedule = self.parsed_schedule(now_ms)?;
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
    pub editor_step: AutomationEditorStep,
    pub calendar_year: i32,
    pub calendar_month: u32,
    pub filter: AutomationFilter,
    pub query: String,
    pub busy_id: Option<String>,
    pub pending_delete_id: Option<String>,
    pub notice: Option<String>,
    pub error: Option<String>,
    /// Fresh verification errors are separate from confirmed results and
    /// historical dispatch diagnostics. Supplied only for this workspace.
    pub native_result_errors: Vec<String>,
}

impl AutomationViewState {
    pub fn new(workspace: impl Into<String>, backends: Vec<(String, String)>, now_ms: i64) -> Self {
        let today = Local::now().date_naive();
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
            editor_step: AutomationEditorStep::Content,
            calendar_year: today.year(),
            calendar_month: today.month(),
            filter: AutomationFilter::All,
            query: String::new(),
            busy_id: None,
            pending_delete_id: None,
            notice: None,
            error: None,
            native_result_errors: Vec::new(),
        }
    }

    pub fn open_editor(&mut self, form: AutomationForm, selected_id: Option<String>) {
        if let Some((year, month)) = form.once_at.get(..7).and_then(|value| {
            let (year, month) = value.split_once('-')?;
            Some((year.parse::<i32>().ok()?, month.parse::<u32>().ok()?))
        }) {
            if (1..=9999).contains(&year) && (1..=12).contains(&month) {
                self.calendar_year = year;
                self.calendar_month = month;
            }
        }
        self.form = form;
        self.selected_id = selected_id;
        self.editor_step = AutomationEditorStep::Content;
        self.screen = AutomationScreen::Edit;
        self.error = None;
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
            Self::Completed => "Sin próximas",
            Self::Failed => "Con error",
        }
    }
}

fn status_for(item: &Automation, runs: &[AutomationRun]) -> AutomationStatus {
    let last = runs
        .iter()
        .filter(|run| run.automation_id == item.id)
        .max_by_key(|run| run.created_at);
    let has_error = last.is_some_and(|run| {
        run.native_result.as_ref().map_or_else(
            || {
                matches!(
                    run.status,
                    AutomationRunStatus::Failed | AutomationRunStatus::Interrupted
                )
            },
            |result| {
                result.recovery_required()
                    || matches!(
                        result.outcome(),
                        NativeAutomationOutcome::Failed | NativeAutomationOutcome::Interrupted
                    )
            },
        )
    });
    if has_error {
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

fn section_label(value: &str) -> Element {
    Text::new(value, theme::component_palette())
        .role(TextRole::Label)
        .render()
}

fn form_field(label: &str, control: Element, sizing: FormFieldSizing) -> FormField {
    FormField::new(label, control, theme::component_palette()).sizing(sizing)
}

fn automation_card(item: &Automation, runs: &[AutomationRun], width: f32, busy: bool) -> Element {
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
    let status_badge = Element::panel(
        Text::new(status.title(), theme::component_palette())
            .role(TextRole::Micro)
            .ink(status_color)
            .render(),
    )
    .padding(5.0)
    .background(theme::SELECTED)
    .radius(6.0);
    let toggle_id = format!("automation_toggle:{}", item.id);
    let toggle = Tooltip::new(
        Switch::new(toggle_id.clone(), item.enabled, theme::component_palette()).render(),
        toggle_id,
        if item.enabled {
            "Pausar tarea"
        } else {
            "Activar tarea"
        },
        theme::component_palette(),
    )
    .render();
    let header = vec![
        Element::truncated_text(&item.title)
            .foreground(theme::FOREGROUND)
            .font_family(FontFamily::Sans)
            .font_size(14.0)
            .flex(1.0),
        status_badge,
    ];
    let mut rows = vec![
        Element::row(header).gap(8.0),
        Element::truncated_text(&item.prompt)
            .foreground(theme::FOREGROUND_SUBTLE)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .min_size(0.0, 18.0),
        Element::row(vec![
            Element::icon(Icon::Clock).foreground(theme::FOREGROUND_SUBTLE),
            Element::truncated_text(summary)
                .foreground(theme::FOREGROUND_SUBTLE)
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .flex(1.0),
            Text::new(
                format!("{} ejec.", item.run_count),
                theme::component_palette(),
            )
            .role(TextRole::Micro)
            .muted()
            .render(),
        ])
        .gap(5.0),
    ];
    let edit = IconButton::new(
        format!("automation_edit:{}", item.id),
        Icon::Edit,
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Regular)
    .tooltip_with(TooltipSpec::new("Editar tarea").placement(TooltipPlacement::Above))
    .render();
    let history = IconButton::new(
        format!("automation_history:{}", item.id),
        Icon::History,
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Regular)
    .tooltip_with(TooltipSpec::new("Ver historial").placement(TooltipPlacement::Above))
    .render();
    let run = IconButton::new(
        format!("automation_run:{}", item.id),
        Icon::ArrowUp,
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Regular)
    .enabled(!busy)
    .tooltip_with(
        TooltipSpec::new(if busy {
            "Iniciando…"
        } else {
            "Ejecutar ahora"
        })
        .placement(TooltipPlacement::Above),
    )
    .render();
    let delete = IconButton::new(
        format!("automation_delete_request:{}", item.id),
        Icon::Trash,
        theme::component_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Regular)
    .tooltip_with(TooltipSpec::new("Eliminar tarea").placement(TooltipPlacement::Above))
    .render();
    let mut actions = vec![edit, history, run];
    if busy {
        actions.push(
            Text::new("Iniciando…", theme::component_palette())
                .role(TextRole::Micro)
                .muted()
                .render(),
        );
    }
    actions.extend([Element::spacer().flex(1.0), toggle]);
    actions.push(delete);
    rows.push(Element::row(actions).gap(4.0));
    Card::new(Element::column(rows).gap(5.0), theme::canvas_card_palette())
        .width(width)
        .render()
        // Radius 10 is this card's own; see docs/limn-migration-ledger.md.
        .radius(10.0)
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
    theme::interactive_surface(Element::clickable(
        format!("automation_template:{}", template.id),
        Element::column(vec![
            Element::row(vec![
                Element::icon(template.icon).foreground(theme::FOREGROUND),
                Element::truncated_text(template.title)
                    .font_family(FontFamily::Sans)
                    .font_size(13.0)
                    .foreground(theme::FOREGROUND)
                    .flex(1.0),
            ])
            .gap(6.0),
            Element::truncated_text(template.description)
                .font_family(FontFamily::Sans)
                .font_size(11.0)
                .foreground(theme::FOREGROUND_SUBTLE),
            Text::new(schedule, theme::component_palette())
                .role(TextRole::Micro)
                .muted()
                .render(),
        ])
        .gap(5.0),
    ))
    .background(theme::BACKGROUND)
    .border(theme::BORDER, 1.0)
    .padding(10.0)
    .size(Some(width), Some(94.0))
}

fn list_heading(empty: bool, content_width: f32) -> Element {
    let heading = PageHeader::new("Automatizaciones", theme::component_palette())
        .description("Tareas programadas para este proyecto")
        .sizing(crate::ui_sizing::AUTOMATIONS_HEADER);
    if empty {
        return heading.render();
    }
    heading
        .trailing(
            Button::new("automation_new", "Nueva tarea", theme::component_palette())
                .icon(Icon::Add)
                .size(ControlSize::Regular)
                .render(),
        )
        .trailing_top(true)
        .stacked(content_width < 440.0)
        .render()
}

#[cfg(test)]
fn legacy_list_heading(empty: bool, content_width: f32) -> Element {
    let heading = Element::column(vec![
        theme::text_px("Automatizaciones", 22.0, theme::FOREGROUND),
        Text::new(
            "Tareas programadas para este proyecto",
            theme::component_palette(),
        )
        .muted()
        .render(),
    ])
    .gap(2.0);
    let new = || {
        Button::new("automation_new", "Nueva tarea", theme::component_palette())
            .icon(Icon::Add)
            .size(ControlSize::Regular)
            .render()
    };
    if empty {
        heading
    } else if content_width < 440.0 {
        Element::column(vec![heading, new()]).gap(8.0)
    } else {
        // The action stays on the title's line; limn's PageHeader centers it.
        Element::row(vec![heading, Element::spacer().flex(1.0), new()]).gap(8.0)
    }
}

fn list_page(state: &AutomationViewState, content_width: f32) -> Vec<Element> {
    let grid = || CardGrid::new(content_width, 690.0);
    let card_width = grid().item_width();
    let mut body = vec![list_heading(state.items.is_empty(), content_width)];
    let visible = state.visible_items();
    if state.items.is_empty() {
        body.push(
            Empty::new(
                "Todavía no hay tareas programadas",
                "Crea una tarea recurrente o empieza desde una plantilla.",
                theme::component_palette(),
            )
            .icon(Icon::Clock)
            .action(
                "automation_new",
                "Crear tarea programada",
                ButtonVariant::Default,
            )
            .compact()
            .width(content_width)
            .height(if content_width < 420.0 { 204.0 } else { 188.0 })
            .render(),
        );
    } else {
        body.push(
            SearchField::new(
                "automation_query",
                &state.query,
                "Buscar tareas programadas…",
                theme::component_palette(),
            )
            .size(ControlSize::Regular)
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
        let tab_widths = [62.0, 68.0, 72.0, 96.0, 86.0];
        let filter_tabs: Vec<_> = filters
            .into_iter()
            .zip(tab_widths)
            .map(|(filter, width)| {
                Tab::new(filter.action_id(), filter.title(), width).active(state.filter == filter)
            })
            .collect();
        if content_width < 410.0 {
            let mut tabs = filter_tabs.into_iter();
            body.push(
                Element::column(vec![
                    Tabs::new(tabs.by_ref().take(3).collect(), theme::segmented_palette())
                        .id("automation_filters_primary")
                        .size(ControlSize::Compact)
                        .render(),
                    Tabs::new(tabs.collect(), theme::segmented_palette())
                        .id("automation_filters_secondary")
                        .size(ControlSize::Compact)
                        .render(),
                ])
                .gap(4.0),
            );
        } else {
            body.push(
                Tabs::new(filter_tabs, theme::segmented_palette())
                    .id("automation_filters")
                    .size(ControlSize::Compact)
                    .render(),
            );
        }
        if visible.is_empty() {
            body.push(
                Empty::new(
                    "Sin resultados",
                    "Prueba otra búsqueda o cambia el filtro.",
                    theme::component_palette(),
                )
                .icon(Icon::Search)
                .compact()
                .height(150.0)
                .width(content_width)
                .render(),
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
                    )
                })
                .collect();
            body.push(grid().items(cards).render());
        }
    }
    body.extend([
        Separator::horizontal(theme::component_palette()).render(),
        section_label("Plantillas de tareas programadas"),
        grid()
            .items(
                TEMPLATES
                    .iter()
                    .map(|template| template_card(template, card_width))
                    .collect(),
            )
            .render(),
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
    // `fill_width` sets each tab's width; the one given here is replaced.
    let tabs: Vec<_> = frequencies
        .into_iter()
        .map(|frequency| {
            Tab::new(frequency.action_id(), frequency.title(), 0.0)
                .active(form.frequency == frequency)
        })
        .collect();
    let track = |id: &str, tabs: Vec<Tab>| {
        Tabs::new(tabs, theme::segmented_palette())
            .id(id)
            .size(ControlSize::Compact)
            .fill_width(available_width)
            .render()
    };
    if available_width < 390.0 {
        let mut tabs = tabs.into_iter();
        Element::column(vec![
            track("automation_frequency_primary", tabs.by_ref().take(2).collect()),
            track("automation_frequency_secondary", tabs.collect()),
        ])
        .gap(4.0)
    } else {
        track("automation_frequency", tabs)
    }
}

fn schedule_editor(state: &AutomationViewState, content_width: f32) -> Vec<Element> {
    let form = &state.form;
    let field = |label: &str, control: Element| {
        form_field(label, control, AUTOMATION_SCHEDULE_FIELD).render()
    };
    // The track has always stopped 26 px short of the form; see the ledger.
    let mut rows = vec![field(
        "Frecuencia",
        frequency_control(form, content_width - 26.0),
    )];
    match form.frequency {
        AutomationFrequency::Once => {
            let (date, time) = form.once_parts();
            rows.push(
                DateTimePicker::new(
                    "automation_once_date",
                    "automation_once_time",
                    date,
                    time,
                    theme::component_palette(),
                )
                .display_month(state.calendar_year, state.calendar_month)
                .render(),
            );
        }
        AutomationFrequency::EveryMinutes => rows.push(field(
            "Cada cuántos minutos",
            TextField::new(
                "automation_interval",
                &form.interval_minutes,
                "60",
                theme::component_palette(),
            )
            .size(ControlSize::Regular)
            .render(),
        )),
        AutomationFrequency::Daily => rows.push(field(
            "Hora local",
            TimePicker::new("automation_time", &form.time, theme::component_palette()).render(),
        )),
        AutomationFrequency::Weekly => {
            let days = ["L", "M", "X", "J", "V", "S", "D"];
            rows.push(field(
                "Día de la semana",
                Element::row(
                    days.into_iter()
                        .enumerate()
                        .map(|(index, day)| {
                            let selected = usize::from(form.weekday) == index;
                            Button::new(
                                format!("automation_weekday:{index}"),
                                day,
                                theme::component_palette(),
                            )
                            .variant(if selected {
                                ButtonVariant::Secondary
                            } else {
                                ButtonVariant::Ghost
                            })
                            .size(ControlSize::Compact)
                            .width(((content_width - 26.0 - 24.0) / 7.0).max(28.0))
                            .active(selected)
                            .render()
                        })
                        .collect(),
                )
                .gap(4.0),
            ));
            rows.push(field(
                "Hora local",
                TimePicker::new("automation_time", &form.time, theme::component_palette()).render(),
            ));
        }
    }
    rows
}

fn editor_agent_step(state: &AutomationViewState, content_width: f32) -> Element {
    let backend_controls = if state.backends.is_empty() {
        Text::new(
            "No hay agentes disponibles para programar tareas.",
            theme::component_palette(),
        )
        .muted()
        .render()
    } else {
        let backend_options: Vec<_> = state
            .backends
            .iter()
            .map(|(id, title)| {
                Button::new(
                    format!("automation_backend:{id}"),
                    title,
                    theme::component_palette(),
                )
                .variant(if state.form.backend == *id {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                })
                .size(ControlSize::Compact)
                .active(state.form.backend == *id)
                .render()
            })
            .collect();
        if content_width < 430.0 && backend_options.len() > 2 {
            Element::column(backend_options).gap(4.0)
        } else {
            Element::row(backend_options).gap(4.0)
        }
    };
    // With a single way to run a task there is nothing to choose.
    let mut backend_rows = if state.backends.len() == 1 {
        vec![section_label("Modelo del agente")]
    } else {
        vec![
            section_label("Motor del agente"),
            Text::new(
                "Elige quién iniciará cada ejecución.",
                theme::component_palette(),
            )
            .muted()
            .render(),
            backend_controls,
        ]
    };
    if state.form.backend == "native" {
        let selected = state
            .form
            .native_provider
            .as_ref()
            .map(|settings| format!("{} · {}", settings.provider_id, settings.model_id))
            .unwrap_or_else(|| "Configuración nativa actual".to_owned());
        backend_rows.push(
            Text::new(
                format!("Modelo de esta tarea: {selected}"),
                theme::component_palette(),
            )
            .muted()
            .render(),
        );
        backend_rows.push(
            Button::new(
                "automation_native_current",
                "Usar modelo actual de Settings",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Regular)
            .render(),
        );
    } else {
        backend_rows.push(
            Text::new(
                "Se usa la CLI detectada en PATH para este equipo.",
                theme::component_palette(),
            )
            .muted()
            .render(),
        );
    }
    Element::column(backend_rows)
        .gap(10.0)
        .size(Some(content_width), None)
}

pub fn editor_modal_body(state: &AutomationViewState, width: f32) -> Element {
    let form = match state.editor_step {
        AutomationEditorStep::Content => Element::column(vec![
            section_label("¿Qué hará esta tarea?"),
            form_field(
                "Título",
                TextField::new(
                    "automation_title",
                    &state.form.title,
                    "Ej. Resumen diario",
                    theme::component_palette(),
                )
                .size(ControlSize::Regular)
                .render(),
                AUTOMATION_FIELD,
            )
            .render(),
            form_field(
                "Instrucciones",
                TextArea::new(
                    "automation_prompt",
                    &state.form.prompt,
                    "Describe el resultado esperado y los límites de esta tarea…",
                    theme::component_palette(),
                )
                .height(112.0)
                .render(),
                AUTOMATION_FIELD,
            )
            .hint("Las ejecuciones sin supervisión pueden esperar una aprobación antes de modificar archivos.")
            .render(),
        ])
        .gap(8.0),
        AutomationEditorStep::Schedule => Element::column({
            let mut rows = vec![section_label("¿Cuándo se ejecutará?")];
            rows.extend(schedule_editor(state, width));
            rows
        })
        .gap(9.0),
        AutomationEditorStep::Agent => editor_agent_step(state, width),
    };
    editor_modal_layout(state, width, form)
}

fn editor_modal_layout(state: &AutomationViewState, width: f32, form: Element) -> Element {
    let stepper = Stepper::new(
        vec![
            Step::new("automation_step:content", "Contenido").description("Define la tarea"),
            Step::new("automation_step:schedule", "Horario").description("Elige cuándo"),
            Step::new("automation_step:agent", "Agente").description("Modelo y acceso"),
        ],
        state.editor_step.index(),
        theme::component_palette(),
    )
    .width(width)
    .render();
    let mut content = vec![
        stepper,
        Separator::horizontal(theme::component_palette()).render(),
    ];
    if let Some(error) = &state.error {
        content.push(
            Alert::new(
                "Revisa la automatización",
                error,
                theme::component_palette(),
            )
            .tone(AlertTone::Error)
            .compact(true)
            .render(),
        );
    }
    content.push(form);
    Element::column(content).gap(14.0).size(Some(width), None)
}

pub fn editor_modal_footer(state: &AutomationViewState) -> Element {
    let mut controls = Vec::new();
    if state.editor_step == AutomationEditorStep::Content {
        controls.push(
            Button::new("automation_cancel", "Cancelar", theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Regular)
                .render(),
        );
    } else {
        controls.push(
            Button::new(
                "automation_previous",
                "Anterior",
                theme::component_palette(),
            )
            .icon(Icon::ArrowLeft)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Regular)
            .render(),
        );
    }
    if state.editor_step == AutomationEditorStep::Agent {
        controls.push(
            Button::new(
                "automation_save",
                if state.selected_id.is_some() {
                    "Guardar cambios"
                } else {
                    "Crear tarea"
                },
                theme::component_palette(),
            )
            .icon(Icon::Check)
            .size(ControlSize::Regular)
            .render(),
        );
    } else {
        controls.push(
            Button::new("automation_next", "Siguiente", theme::component_palette())
                .icon(Icon::ArrowRight)
                .size(ControlSize::Regular)
                .render(),
        );
    }
    ButtonGroup::new(controls).align_end().render()
}

fn history_page(state: &AutomationViewState, content_width: f32) -> Vec<Element> {
    let Some(item) = state.selected() else {
        return vec![
            Button::new("automation_back", "Volver", theme::component_palette())
                .icon(Icon::ArrowLeft)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Regular)
                .render(),
            Empty::new(
                "Tarea no disponible",
                "Esta tarea ya no existe.",
                theme::component_palette(),
            )
            .icon(Icon::History)
            .compact()
            .height(150.0)
            .width(content_width)
            .render(),
        ];
    };
    let mut runs: Vec<_> = state
        .runs
        .iter()
        .filter(|run| run.automation_id == item.id)
        .collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
    let mut body = vec![
        Button::new("automation_back", "Volver", theme::component_palette())
            .icon(Icon::ArrowLeft)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Regular)
            .render(),
        Element::truncated_text(format!("Historial · {}", item.title))
            .font_family(FontFamily::Sans)
            .font_size(22.0)
            .foreground(theme::FOREGROUND),
        Text::new("El despacho y el resultado del agente se registran por separado. Los resultados nativos se verifican contra el origen y el journal de la sesión.", theme::component_palette()).muted().render(),
    ];
    for error in &state.native_result_errors {
        body.push(
            Alert::new(
                "Resultado nativo no verificable",
                error,
                theme::component_palette(),
            )
            .tone(AlertTone::Error)
            .compact(content_width < 640.0)
            .render(),
        );
    }
    if runs.is_empty() {
        body.push(
            Empty::new(
                "Sin ejecuciones",
                "Aún no hay ejecuciones de esta tarea.",
                theme::component_palette(),
            )
            .icon(Icon::History)
            .compact()
            .height(150.0)
            .width(content_width)
            .render(),
        );
    } else {
        for run in runs {
            let (dispatch_status, dispatch_color, dispatch_explanation) =
                match (run.status, run.session_id.is_some()) {
                    (AutomationRunStatus::Pending, true) => (
                        "Sesión vinculada",
                        theme::FOREGROUND_SUBTLE,
                        "Inicio del agente aún no confirmado.",
                    ),
                    (AutomationRunStatus::Pending, false) => (
                        "Despacho pendiente",
                        theme::FOREGROUND_SUBTLE,
                        "Aún no hay una sesión vinculada.",
                    ),
                    (AutomationRunStatus::Dispatched, _) => (
                        "Despacho confirmado",
                        theme::FOREGROUND,
                        "El resultado final se consulta en la sesión.",
                    ),
                    (AutomationRunStatus::Failed, true) => (
                        "Falló el despacho",
                        theme::DESTRUCTIVE,
                        "La sesión vinculada conserva su diagnóstico.",
                    ),
                    (AutomationRunStatus::Failed, false) => (
                        "Falló el despacho",
                        theme::DESTRUCTIVE,
                        "No hay una sesión vinculada.",
                    ),
                    (AutomationRunStatus::Skipped, _) => (
                        "Omitida",
                        theme::FOREGROUND_SUBTLE,
                        "No se inició esta ejecución.",
                    ),
                    (AutomationRunStatus::Interrupted, true) => (
                        "Sesión vinculada",
                        theme::DESTRUCTIVE,
                        "Despacho interrumpido; revisa la sesión para continuar.",
                    ),
                    (AutomationRunStatus::Interrupted, false) => (
                        "Despacho interrumpido",
                        theme::DESTRUCTIVE,
                        "Sin sesión vinculada; no se repite automáticamente.",
                    ),
                };
            let (status, color, explanation) = match run.native_result.as_ref() {
                Some(result) => {
                    let (status, color) = match result.outcome() {
                        NativeAutomationOutcome::Completed => ("Completada", theme::FOREGROUND),
                        NativeAutomationOutcome::Failed => ("Fallida", theme::DESTRUCTIVE),
                        NativeAutomationOutcome::Cancelled => {
                            ("Cancelada", theme::FOREGROUND_SUBTLE)
                        }
                        NativeAutomationOutcome::Interrupted => {
                            ("Interrumpida", theme::DESTRUCTIVE)
                        }
                    };
                    (status, color, "Último resultado guardado de la sesión.")
                }
                None => (dispatch_status, dispatch_color, dispatch_explanation),
            };
            let when = Local
                .timestamp_millis_opt(run.scheduled_at)
                .single()
                .map(|time| time.format("%d/%m/%Y %H:%M").to_string())
                .unwrap_or_else(|| "Fecha desconocida".to_owned());
            let status_line = if content_width < 520.0 {
                Element::column(vec![
                    Text::new(status, theme::component_palette())
                        .role(TextRole::Label)
                        .ink(color)
                        .render(),
                    Text::new(when, theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .render(),
                ])
                .gap(3.0)
            } else {
                Element::row(vec![
                    Text::new(status, theme::component_palette())
                        .role(TextRole::Label)
                        .ink(color)
                        .render(),
                    Element::spacer().flex(1.0),
                    Text::new(when, theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .render(),
                ])
            };
            let mut details = vec![
                status_line,
                Text::new(explanation, theme::component_palette())
                    .role(TextRole::Small)
                    .muted()
                    .render(),
                Text::new(
                    if run.manual {
                        "Inicio manual"
                    } else {
                        "Programada"
                    },
                    theme::component_palette(),
                )
                .role(TextRole::Small)
                .muted()
                .render(),
            ];
            if let Some(result) = &run.native_result {
                details.push(
                    Text::new(
                        format!("Despacho: {dispatch_status}"),
                        theme::component_palette(),
                    )
                    .role(TextRole::Small)
                    .muted()
                    .render(),
                );
                details.push(
                    Text::new(
                        format!(
                            "Journal observado hasta el evento {}{}",
                            result.last_sequence(),
                            result
                                .terminal_sequence()
                                .map(|sequence| format!(" · terminal {sequence}"))
                                .unwrap_or_default(),
                        ),
                        theme::component_palette(),
                    )
                    .role(TextRole::Small)
                    .muted()
                    .render(),
                );
                if result.recovery_required() {
                    details.push(
                        Text::new(
                            "Requiere resolución en la sesión",
                            theme::component_palette(),
                        )
                        .role(TextRole::Small)
                        .ink(theme::DESTRUCTIVE)
                        .render(),
                    );
                }
            }
            if let Some(error) = &run.error {
                details.push(
                    Text::new(
                        if run.native_result.is_some() {
                            format!("Diagnóstico de despacho: {error}")
                        } else {
                            error.clone()
                        },
                        theme::component_palette(),
                    )
                    .role(TextRole::Small)
                    .ink(theme::DESTRUCTIVE)
                    .render(),
                );
            }
            if let Some(session_id) = &run.session_id {
                details.push(
                    Button::new(
                        format!("automation_open_session:{session_id}"),
                        "Abrir sesión",
                        theme::component_palette(),
                    )
                    .icon(Icon::ArrowRight)
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Regular)
                    .render(),
                );
            }
            body.push(
                Card::new(Element::column(details).gap(5.0), theme::canvas_card_palette())
                    .render()
                    // Radius 9 is this card's own; see docs/limn-migration-ledger.md.
                    .radius(9.0),
            );
        }
    }
    body
}

/// Responsive page body for the center dock. `width` is its available width
/// after the shared workspace border, not the entire window width.
pub fn build_automations(state: &AutomationViewState, width: f32) -> Element {
    let page =
        PageColumn::new("automation_page", width, theme::component_palette()).min_width(240.0);
    let content_width = page.content_width();
    let mut body = match state.screen {
        AutomationScreen::List => list_page(state, content_width),
        AutomationScreen::Edit => list_page(state, content_width),
        AutomationScreen::History => history_page(state, content_width),
    };
    if let Some(notice) = &state.notice {
        body.insert(
            0,
            Alert::new(
                "Aviso de automatizaciones",
                notice,
                theme::component_palette(),
            )
            .tone(AlertTone::Info)
            .compact(content_width < 640.0)
            .render(),
        );
    }
    if state.screen != AutomationScreen::Edit {
        if let Some(error) = &state.error {
            body.insert(
                0,
                Alert::new(
                    "Error de automatizaciones",
                    error,
                    theme::component_palette(),
                )
                .tone(AlertTone::Error)
                .compact(content_width < 640.0)
                .render(),
            );
        }
    }
    let base = page.children(body).render();
    if state.screen == AutomationScreen::Edit {
        let width = Modal::fit_width(width, 560.0).max(240.0);
        let body = editor_modal_body(state, crate::ui_sizing::form_width(width));
        editor_modal(state, base, width, body)
    } else {
        base
    }
}

fn editor_modal(state: &AutomationViewState, base: Element, width: f32, body: Element) -> Element {
    let initial_focus = match state.editor_step {
        AutomationEditorStep::Content => "automation_title".to_owned(),
        AutomationEditorStep::Schedule => state.form.frequency.action_id().to_owned(),
        AutomationEditorStep::Agent => state
            .backends
            .first()
            .map(|(id, _)| format!("automation_backend:{id}"))
            .unwrap_or_else(|| "automation_previous".to_owned()),
    };
    Modal::new(
        base,
        true,
        if state.selected_id.is_some() {
            "Editar tarea"
        } else {
            "Nueva tarea programada"
        },
        "Inicia una sesión nueva en este proyecto cuando llegue el horario.",
        "automation_cancel",
        theme::component_palette(),
    )
    .width(width)
    .height(520.0)
    .body(body)
    .footer(editor_modal_footer(state))
    .initial_focus(initial_focus)
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_heading_matches_its_legacy_body() {
        for empty in [false, true] {
            for width in [300.0, 439.0, 440.0, 900.0] {
                crate::converge_pages_rows_parity_tests::assert_same_at(
                    "automations heading",
                    &[width],
                    legacy_list_heading(empty, width).size(Some(width), None),
                    list_heading(empty, width).size(Some(width), None),
                );
            }
        }
    }
    use openide_ui::{Key, Point, Size, UiAction, UiEvent, UiTree};

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
    #[ignore = "export the Automations page for manual visual review"]
    fn export_automations_visual_review() {
        let output = std::path::Path::new("/tmp/openade-automations-review");
        std::fs::create_dir_all(&output).unwrap();
        let mut state = state();
        state.items = vec![
            sample(),
            Automation {
                id: "two".into(),
                title: "Resumen semanal".into(),
                enabled: false,
                ..sample()
            },
            Automation {
                id: "three".into(),
                title: "Inspección de riesgos".into(),
                ..sample()
            },
        ];
        for width in [360.0, 640.0, 1200.0] {
            let mut tree = UiTree::new(
                Element::panel(build_automations(&state, width)).background(theme::BACKGROUND),
            );
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 760.0));
            let path = output.join(format!("automations-{width:.0}.svg"));
            crate::settings_ui::visual_qa::write_frame_svg(&frame, &path, width, 760.0).unwrap();
            eprintln!("{}", path.display());
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
    fn once_date_and_time_roundtrip_without_losing_either_value() {
        let mut form = AutomationForm::new("codex");
        form.title = "Recordatorio".into();
        form.prompt = "Prepara el resumen".into();
        form.frequency = AutomationFrequency::Once;
        form.set_once_date("2035-05-17");
        form.set_once_time("16:45");
        assert_eq!(form.once_at, "2035-05-17 16:45");
        let draft = form.to_draft(1_000_000).unwrap();
        let automation = Automation {
            id: "once".into(),
            title: form.title.clone(),
            prompt: form.prompt.clone(),
            backend: form.backend.clone(),
            native_provider: None,
            schedule: draft.schedule,
            enabled: true,
            next_run_at: None,
            last_run_at: None,
            run_count: 0,
            created_at: 0,
            updated_at: 0,
        };
        let mut restored = AutomationForm::from_automation(&automation);
        assert_eq!(restored.once_parts(), ("2035-05-17", "16:45"));
        restored.set_once_time("17:20");
        assert_eq!(restored.once_parts(), ("2035-05-17", "17:20"));
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
        assert_eq!(tree.focused_id(), Some("automation_title"));
        assert!(editor
            .regions
            .iter()
            .any(|r| r.id == "popover_surface:automation_cancel"));
        assert!(editor
            .regions
            .iter()
            .any(|r| r.id == "modal_body:automation_cancel"));
        for id in ["automation_title", "automation_prompt", "automation_next"] {
            assert!(editor.regions.iter().any(|r| r.id == id), "missing {id}");
        }
        assert!(!editor.regions.iter().any(|r| r.id == "automation_save"));
        state.editor_step = AutomationEditorStep::Schedule;
        tree.set_root(build_automations(&state, 960.0));
        let schedule = tree.layout(Size::new(960.0, 780.0));
        for id in [
            "automation_frequency:once",
            "automation_time",
            "automation_previous",
        ] {
            assert!(schedule.regions.iter().any(|r| r.id == id), "missing {id}");
        }
        assert!(!schedule.regions.iter().any(|r| r.id == "automation_prompt"));
        state.editor_step = AutomationEditorStep::Agent;
        tree.set_root(build_automations(&state, 960.0));
        let agent = tree.layout(Size::new(960.0, 780.0));
        for id in ["automation_previous", "automation_save"] {
            assert!(agent.regions.iter().any(|r| r.id == id), "missing {id}");
        }
        assert!(
            !agent.regions.iter().any(|r| r.id.starts_with("automation_backend:")),
            "a single engine leaves nothing to choose"
        );
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Escape,
                shift: false,
                ctrl: false,
            }),
            vec![UiAction::Clicked {
                id: "automation_cancel".into(),
            }]
        );
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
    fn history_distinguishes_recovered_links_from_dispatch_and_opens_session_at_small_and_large_widths(
    ) {
        for width in [320.0, 960.0] {
            let mut state = state();
            state.items.push(sample());
            state.selected_id = Some("one".into());
            state.screen = AutomationScreen::History;
            state.runs = [
                (
                    AutomationRunStatus::Interrupted,
                    "recovered-native-session",
                    3,
                ),
                (
                    AutomationRunStatus::Dispatched,
                    "dispatched-native-session",
                    2,
                ),
                (
                    AutomationRunStatus::Failed,
                    "failed-linked-native-session",
                    1,
                ),
            ]
            .into_iter()
            .map(|(status, session_id, created_at)| AutomationRun {
                run_id: format!("one:{created_at}"),
                automation_id: "one".into(),
                scheduled_at: 2_000_000,
                manual: false,
                status,
                session_id: Some(session_id.into()),
                native_result: None,
                error: None,
                created_at,
                updated_at: created_at,
            })
            .collect();
            let viewport = Size::new(width, 900.0);
            let mut tree = UiTree::new(build_automations(&state, width));
            let frame = tree.layout(viewport);
            let rendered = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    openide_ui::DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let rendered = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            for label in [
                "Sesión vinculada",
                "Despacho confirmado",
                "Falló el despacho",
                "Despacho interrumpido; revisa la sesión para continuar.",
                "El resultado final se consulta en la sesión.",
            ] {
                assert!(
                    rendered.contains(label),
                    "missing {label} at {width}: {rendered}"
                );
            }
            assert!(!rendered.contains("Completada"));
            for region in &frame.regions {
                assert!(
                    region.rect.x >= -0.5,
                    "{} starts outside at {width}",
                    region.id
                );
                assert!(
                    region.rect.x + region.rect.width <= width + 0.5,
                    "{} ends outside at {width}",
                    region.id
                );
            }
            let id = "automation_open_session:recovered-native-session";
            let button = frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect;
            assert!(button.y >= 0.0 && button.y + button.height <= viewport.height);
            let point = Point::new(
                button.x + button.width / 2.0,
                button.y + button.height / 2.0,
            );
            tree.handle_event(UiEvent::PointerDown(point));
            assert_eq!(
                tree.handle_event(UiEvent::PointerUp(point)),
                vec![UiAction::Clicked { id: id.into() }]
            );
        }
    }

    #[test]
    fn history_prioritizes_verified_native_outcomes_and_preserves_dispatch_diagnostics_and_actions()
    {
        use crate::automations::tests::{native_result_fixture, observed_result};
        use openide_core::{AgentEvent, AgentEventKind};

        for (terminal, label, requires_resolution) in [
            (None, "Interrumpida", true),
            (
                Some(AgentEventKind::TurnFailed {
                    error: "Failed before model request".into(),
                }),
                "Fallida",
                false,
            ),
            (Some(AgentEventKind::TurnCancelled), "Cancelada", false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let workspace = directory.path().join("project");
            let (mut store, sessions, checkpoints, dispatch, session) =
                native_result_fixture(directory.path(), &workspace, true);
            sessions
                .append_native_event(&AgentEvent::new(
                    &session.id,
                    2,
                    AgentEventKind::TurnStarted {
                        prompt: dispatch.prompt.clone(),
                    },
                ))
                .unwrap();
            if let Some(terminal) = terminal {
                sessions
                    .append_native_event(&AgentEvent::new(&session.id, 3, terminal))
                    .unwrap();
            }
            let (record, result) = observed_result(&sessions, &checkpoints, &session.id);
            store
                .complete_run(
                    &dispatch.run_id,
                    Err("Dispatch receipt was interrupted".into()),
                    2_000_010,
                )
                .unwrap();
            store
                .record_native_result(&record, &result, 2_000_020)
                .unwrap();
            for width in [360.0, 900.0] {
                let mut state = state();
                state.items = store.automations().to_vec();
                state.runs = store.runs().to_vec();
                state.selected_id = Some(dispatch.automation_id.clone());
                state.screen = AutomationScreen::History;
                let viewport = Size::new(width, 900.0);
                let mut tree = UiTree::new(build_automations(&state, width));
                let frame = tree.layout(viewport);
                let text = frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        openide_ui::DrawCommand::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                assert!(text.contains(label), "{width}: {text}");
                assert!(text.contains("Último resultado guardado de la sesión."));
                assert!(text.contains("Despacho: Falló el despacho"));
                assert!(text.contains("Diagnóstico de despacho: Dispatch receipt was interrupted"));
                assert_eq!(
                    text.contains("Requiere resolución en la sesión"),
                    requires_resolution
                );
                assert_eq!(
                    status_for(&state.items[0], &state.runs) == AutomationStatus::Failed,
                    label != "Cancelada"
                );
                assert!(!text.contains("Completada"));
                for region in &frame.regions {
                    assert!(
                        region.rect.x >= -0.5 && region.rect.x + region.rect.width <= width + 0.5,
                        "{} outside {width}: {:?}",
                        region.id,
                        region.rect
                    );
                }
                let id = format!("automation_open_session:{}", session.id);
                let button = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect;
                assert!(button.y >= 0.0 && button.y + button.height <= viewport.height);
                let point = Point::new(
                    button.x + button.width / 2.0,
                    button.y + button.height / 2.0,
                );
                tree.handle_event(UiEvent::PointerDown(point));
                assert_eq!(
                    tree.handle_event(UiEvent::PointerUp(point)),
                    vec![UiAction::Clicked { id }]
                );
            }
        }
    }

    #[test]
    fn history_shows_current_verification_error_without_overwriting_confirmed_result() {
        use crate::automations::tests::{native_result_fixture, observed_result};

        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("project");
        let (mut store, sessions, checkpoints, dispatch, session) =
            native_result_fixture(directory.path(), &workspace, false);
        let (record, result) = observed_result(&sessions, &checkpoints, &session.id);
        store
            .record_native_result(&record, &result, 2_000_010)
            .unwrap();
        let diagnostic = format!(
            "Resultado de {} no verificable: falta el checkpoint original; el resultado confirmado conserva el journal observado hasta el evento 1. No se admitió ni repitió una solicitud.",
            dispatch.run_id,
        );
        for width in [360.0, 900.0] {
            let mut state = state();
            state.items = store.automations().to_vec();
            state.runs = store.runs().to_vec();
            state.selected_id = Some(dispatch.automation_id.clone());
            state.screen = AutomationScreen::History;
            state.native_result_errors.push(diagnostic.clone());
            let frame =
                UiTree::new(build_automations(&state, width)).layout(Size::new(width, 900.0));
            let rendered = frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    openide_ui::DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let rendered = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                rendered.contains("Resultado nativo no verificable"),
                "{width}: {rendered}"
            );
            // A long exact run ID wraps inside its word on narrow surfaces.
            // Verify every diagnostic character survives, including its tail.
            let compact_rendered: String =
                rendered.chars().filter(|c| !c.is_whitespace()).collect();
            let compact_diagnostic: String =
                diagnostic.chars().filter(|c| !c.is_whitespace()).collect();
            assert!(
                compact_rendered.contains(&compact_diagnostic),
                "{width}: {rendered}"
            );
            assert!(rendered.contains("Interrumpida"));
            assert_eq!(
                state.runs[0].native_result.as_ref(),
                Some(result.snapshot())
            );
            assert!(state.runs[0].error.is_none());
            for region in &frame.regions {
                assert!(
                    region.rect.x >= -0.5 && region.rect.x + region.rect.width <= width + 0.5,
                    "{} outside {width}: {:?}",
                    region.id,
                    region.rect
                );
            }
        }
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
        state.editor_step = AutomationEditorStep::Schedule;
        state.form.frequency = AutomationFrequency::Once;
        state.form.set_once_date("2035-05-17");
        state.calendar_year = 2035;
        state.calendar_month = 5;
        let mut calendar_tree = UiTree::new(build_automations(&state, viewport.width));
        let calendar = calendar_tree.layout(viewport);
        assert!(calendar
            .regions
            .iter()
            .any(|r| r.id == "automation_once_date"));
        assert!(calendar_tree.focus("automation_once_time"));
        let time_visible = calendar_tree.layout(viewport);
        assert!(time_visible
            .regions
            .iter()
            .any(|r| r.id == "automation_once_time"));
        assert!(calendar_tree.scroll_offset("modal_body:automation_cancel") > 0.0);
        for region in &time_visible.regions {
            assert!(
                region.rect.x + region.rect.width <= viewport.width + 0.5,
                "{} ends outside calendar at {}",
                region.id,
                region.rect.x + region.rect.width
            );
        }
    }

    #[test]
    fn short_viewport_keeps_editor_actions_visible() {
        let mut state = state();
        state.screen = AutomationScreen::Edit;
        state.error = Some("Escribe un título y revisa las instrucciones. La fecha elegida no está disponible; elige otra fecha y comprueba el horario local antes de continuar.".into());
        let viewport = Size::new(320.0, 240.0);
        for (step, expected_action) in [
            (AutomationEditorStep::Content, "automation_next"),
            (AutomationEditorStep::Schedule, "automation_next"),
            (AutomationEditorStep::Agent, "automation_save"),
        ] {
            state.editor_step = step;
            let frame = UiTree::new(build_automations(&state, viewport.width)).layout(viewport);
            let card = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:automation_cancel")
                .expect("editor modal")
                .rect;
            for id in ["automation_cancel:close", expected_action] {
                let control = frame
                    .regions
                    .iter()
                    .find(|region| {
                        region.id == id
                            && region.rect.x >= card.x
                            && region.rect.y >= card.y
                            && region.rect.x + region.rect.width <= card.x + card.width
                            && region.rect.y + region.rect.height <= card.y + card.height
                    })
                    .unwrap_or_else(|| panic!("missing {id} in {step:?}"));
                assert!(
                    control.rect.width > 0.0 && control.rect.height > 0.0,
                    "{id}"
                );
            }
        }
    }

    #[test]
    fn editor_validation_alert_belongs_to_the_scrolling_body() {
        let mut state = state();
        state.screen = AutomationScreen::Edit;
        state.error = Some("Escribe un título".into());
        let body = UiTree::new(editor_modal_body(&state, 280.0)).layout(Size::new(280.0, 720.0));
        assert!(body.commands.iter().any(|command| matches!(command,
            openide_ui::DrawCommand::Text { text, .. } if text == "Escribe un título"
        )));
        let footer = UiTree::new(editor_modal_footer(&state)).layout(Size::new(280.0, 32.0));
        assert!(!footer.commands.iter().any(|command| matches!(command,
            openide_ui::DrawCommand::Text { text, .. } if text == "Escribe un título"
        )));
        assert!(footer
            .regions
            .iter()
            .any(|region| region.id == "automation_next"));
        assert!(footer
            .regions
            .iter()
            .any(|region| region.id == "automation_cancel"));
    }

    #[test]
    fn editor_restores_focus_to_its_trigger() {
        for trigger in ["automation_new", "automation_template:risk_scan"] {
            let mut state = state();
            let mut tree = UiTree::new(build_automations(&state, 960.0));
            tree.layout(Size::new(960.0, 780.0));
            assert!(tree.focus(trigger), "cannot focus {trigger}");

            state.screen = AutomationScreen::Edit;
            tree.set_root(build_automations(&state, 960.0));
            tree.layout(Size::new(960.0, 780.0));
            assert_eq!(tree.focused_id(), Some("automation_title"));

            state.screen = AutomationScreen::List;
            tree.set_root(build_automations(&state, 960.0));
            tree.layout(Size::new(960.0, 780.0));
            assert_eq!(tree.focused_id(), Some(trigger));
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
        // The busy button keeps its hint, so only a hover region remains.
        assert!(busy
            .regions
            .iter()
            .filter(|r| r.id == "automation_run:one")
            .all(|r| r.kind == openide_ui::HitKind::Hover));
    }
}

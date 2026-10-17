//! Native account usage surfaces. Percentages only come from a provider's
//! measured limits; catalog prices and conversation context are never billing.

use std::{
    borrow::Cow,
    time::{SystemTime, UNIX_EPOCH},
};

use openide_ui::{
    ActiveHover, Button, ButtonVariant, Card, Chip, Color, Component, ComponentPalette,
    ControlSize, Element, Icon, IconButton, ListRow, PageHeader, Popover, PopoverPanel, PopoverPlacement,
    ScrollArea, Separator, Text, TextRole, Tooltip, TooltipPlacement, TooltipSpec,
};

use crate::{
    claude_limits,
    provider_connections,
    provider_icons::icon_for_provider,
    settings_parts,
    provider_usage::{
        ProviderUsageSnapshot, ProviderUsageStatus, STALE_AFTER, UsageUnit, UsageWindow,
    },
    theme,
    view::ViewState,
};

pub(crate) const POPUP_WIDTH: f32 = 320.0;
const POPUP_BODY_MAX_HEIGHT: f32 = 320.0;

fn plan_badge(value: &str) -> Element {
    Chip::new(
        value,
        ComponentPalette {
            text: theme::SETTINGS_FOREGROUND_SUBTLE,
            ..theme::component_palette()
        },
    )
    .outline()
    .height(20.0)
    .render()
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn elapsed_label(timestamp: u64, now: u64) -> String {
    let seconds = now.saturating_sub(timestamp) / 1_000;
    match seconds {
        0..=59 => "ahora".into(),
        60..=3_599 => format!("hace {} min", seconds / 60),
        3_600..=86_399 => format!("hace {} h", seconds / 3_600),
        _ => format!("hace {} d", seconds / 86_400),
    }
}

fn stale(snapshot: &ProviderUsageSnapshot, now: u64) -> bool {
    // A local reading is as old as the CLI's last turn, which is not a failure.
    snapshot.status == ProviderUsageStatus::Error
        || !snapshot.local
            && snapshot
                .fetched_at_ms
                .is_some_and(|timestamp| now.saturating_sub(timestamp) > STALE_AFTER.as_millis() as u64)
}

fn updated(snapshot: &ProviderUsageSnapshot, now: u64) -> String {
    let Some(timestamp) = snapshot.fetched_at_ms else {
        return match snapshot.status {
            ProviderUsageStatus::Loading => "Consultando tu cuenta…",
            ProviderUsageStatus::External => "Consulta directa en tu cuenta",
            ProviderUsageStatus::Disconnected => "Sin conectar",
            ProviderUsageStatus::NeedsSignIn => "Vuelve a conectar tu cuenta",
            _ => "Sin medición reciente",
        }
        .into();
    };
    format!(
        "{} · {}",
        if stale(snapshot, now) {
            "Último dato"
        } else if snapshot.provider_id == claude_limits::PROVIDER_ID {
            "Medido por Claude Code"
        } else if snapshot.local {
            "Medido por Codex"
        } else {
            "Actualizado"
        },
        elapsed_label(timestamp, now)
    )
}

fn reset_label(window: &UsageWindow, now: u64) -> Option<String> {
    if let Some(label) = &window.reset_label {
        return Some(label.clone());
    }
    window.reset_at_ms.map(|reset| {
        if reset <= now {
            return "Pendiente de actualización".into();
        }
        let minutes = (reset.saturating_sub(now) / 60_000).max(1);
        let duration = if minutes < 60 {
            format!("{minutes} min")
        } else if minutes < 1_440 {
            let hours = minutes / 60;
            let rest = minutes % 60;
            if rest == 0 {
                format!("{hours} h")
            } else {
                format!("{hours} h {rest} min")
            }
        } else {
            format!("{} d {} h", minutes / 1_440, minutes % 1_440 / 60)
        };
        format!("Se renueva en {duration}")
    })
}

fn amount(value: f64, unit: UsageUnit) -> String {
    match unit {
        UsageUnit::Usd => format!("${value:.2}"),
        UsageUnit::Requests => format!("{value:.0}"),
        UsageUnit::Percent => format!("{value:.0}%"),
    }
}

fn usage_color(percent: f64) -> Color {
    if percent >= 100.0 {
        theme::DESTRUCTIVE
    } else if percent >= 80.0 {
        theme::WARNING
    } else {
        theme::FOREGROUND
    }
}

fn balance_amount(snapshot: &ProviderUsageSnapshot) -> Option<String> {
    let balance = snapshot.balances.first()?;
    Some(if balance.currency.eq_ignore_ascii_case("USD") {
        format!("${:.2}", balance.amount)
    } else {
        format!("{:.2} {}", balance.amount, balance.currency)
    })
}

/// The measured window closest to its limit, with its used percent.
pub(crate) fn tightest(snapshot: &ProviderUsageSnapshot) -> Option<(&UsageWindow, f64)> {
    snapshot
        .windows
        .iter()
        .filter_map(|window| Some((window, window.used_percent()?)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// The IDE footer's reading of one account.
pub(crate) struct UsageGlance {
    pub icon: Icon,
    pub value: String,
    /// Only from 80 %; below it the cell keeps the ink of its row.
    pub ink: Option<Color>,
    pub title: String,
    pub detail: String,
}

/// Every account with usage to show: the provider catalog, then Claude Code,
/// which is only present once it has reported its limits.
fn roster() -> impl Iterator<Item = (&'static str, &'static str)> {
    provider_connections::PROVIDERS
        .iter()
        .map(|(id, name, _)| (*id, *name))
        .chain([(claude_limits::PROVIDER_ID, claude_limits::PROVIDER_NAME)])
}

/// The account behind the selected session: a CLI's own subscription, or the
/// native provider.
fn active_account(state: &ViewState) -> &str {
    match state.selected_backend.strip_prefix(crate::view::CLI_BACKEND) {
        Some("claude") => claude_limits::PROVIDER_ID,
        Some("codex") => "codex",
        _ => &state.native_provider_id,
    }
}

/// The tightest measured window of the active account, or of the first
/// connected one that has any. A reading that failed to refresh is not shown.
pub(crate) fn glance(state: &ViewState) -> Option<UsageGlance> {
    let now = now_ms();
    let mut accounts: Vec<_> = roster().filter(|(id, _)| connected(state, id)).collect();
    accounts.sort_by_key(|(id, _)| *id != active_account(state));
    accounts.into_iter().find_map(|(id, name)| {
        let snapshot = snapshot(state, id, name);
        let (window, percent) = tightest(&snapshot).filter(|_| !stale(&snapshot, now))?;
        let value = amount(percent, UsageUnit::Percent);
        Some(UsageGlance {
            icon: icon_for_provider(id),
            ink: Some(usage_color(percent)).filter(|ink| *ink != theme::FOREGROUND),
            title: format!("{name} · {value} usado · {}", window.label),
            detail: reset_label(window, now)
                .into_iter()
                .chain([updated(&snapshot, now)])
                .collect::<Vec<_>>()
                .join(" · "),
            value,
        })
    })
}

fn headline(snapshot: &ProviderUsageSnapshot) -> (String, String, Color) {
    if let Some(value) = balance_amount(snapshot) {
        return (value, snapshot.balances[0].label.clone(), theme::FOREGROUND);
    }
    if let Some(window) = snapshot
        .windows
        .iter()
        .find(|window| window.unit == UsageUnit::Usd && window.remaining.is_some())
    {
        return (
            amount(window.remaining.unwrap(), UsageUnit::Usd),
            format!("disponible · {}", window.label),
            theme::FOREGROUND,
        );
    }
    if let Some((window, used)) = tightest(snapshot) {
        return (
            amount(used, UsageUnit::Percent),
            format!("usado · {}", window.label),
            usage_color(used),
        );
    }
    if let Some(window) = snapshot.windows.first() {
        return (
            amount(window.used, window.unit),
            window.label.clone(),
            theme::FOREGROUND,
        );
    }
    let value = match snapshot.status {
        ProviderUsageStatus::Disconnected => "Conecta tu cuenta",
        ProviderUsageStatus::NeedsSignIn => "Reconecta tu cuenta",
        ProviderUsageStatus::Loading => "Consultando…",
        ProviderUsageStatus::External => "Ver en tu cuenta",
        ProviderUsageStatus::Error => "No disponible",
        ProviderUsageStatus::Ready => "Sin medición",
    };
    (
        value.into(),
        status_label(snapshot.status).into(),
        theme::FOREGROUND,
    )
}

fn status_label(status: ProviderUsageStatus) -> &'static str {
    match status {
        ProviderUsageStatus::Disconnected => "Sin conectar",
        ProviderUsageStatus::NeedsSignIn => "Requiere conexión",
        ProviderUsageStatus::Loading => "Consultando",
        ProviderUsageStatus::Ready => "Conectado",
        ProviderUsageStatus::External => "En tu cuenta",
        ProviderUsageStatus::Error => "Sin actualizar",
    }
}

fn fallback(state: &ViewState, id: &str, name: &str) -> ProviderUsageSnapshot {
    let connection = state.provider_connections.get(id);
    let status = if state.native_provider_registry_error.is_some() {
        ProviderUsageStatus::Error
    } else if connection.is_some_and(|connection| connection.needs_sign_in) {
        ProviderUsageStatus::NeedsSignIn
    } else if connection.is_some_and(|connection| connection.configured) {
        ProviderUsageStatus::Loading
    } else {
        ProviderUsageStatus::Disconnected
    };
    ProviderUsageSnapshot {
        provider_id: id.into(),
        provider_name: name.into(),
        status,
        plan_label: None,
        account: connection.and_then(|connection| connection.account.clone()),
        windows: Vec::new(),
        balances: Vec::new(),
        fetched_at_ms: None,
        local: false,
        account_url: None,
        message: state.native_provider_registry_error.as_ref().map(|_| {
            "No se pudo cargar el catálogo de proveedores. Revisa su configuración.".into()
        }),
    }
}

fn snapshot<'a>(state: &'a ViewState, id: &str, name: &str) -> Cow<'a, ProviderUsageSnapshot> {
    // The connection catalog is authoritative after sign-out, even while an
    // older network result is still being discarded by the runtime.
    if state
        .provider_connections
        .get(id)
        .is_some_and(|connection| !connection.configured || connection.needs_sign_in)
    {
        return Cow::Owned(fallback(state, id, name));
    }
    state
        .provider_usage
        .get(id)
        .map(Cow::Borrowed)
        .unwrap_or_else(|| Cow::Owned(fallback(state, id, name)))
}

fn connected(state: &ViewState, id: &str) -> bool {
    // Claude Code has no connection of ours; its reported limits stand for one.
    id == claude_limits::PROVIDER_ID && state.provider_usage.contains_key(id)
        || state
            .provider_connections
            .get(id)
            .is_some_and(|connection| connection.configured && !connection.needs_sign_in)
}

fn provider_mark(id: &str) -> Element {
    Element::panel(Element::icon(icon_for_provider(id)).foreground(theme::ICON_PRIMARY))
        .size(Some(26.0), Some(26.0))
        .center_content()
        .background(theme::SELECTED)
        .radius(8.0)
}

fn progress(window: &UsageWindow) -> Option<Element> {
    let percent = window.used_percent()?;
    // Flex shares the available width, so the measured track also fits when
    // the anchored popover is narrowed by the viewport.
    let fill = percent.clamp(0.0, 100.0) as f32;
    let mut pieces = Vec::new();
    if fill > 0.0 {
        pieces.push(
            Element::spacer()
                .background(usage_color(percent))
                .flex(fill),
        );
    }
    if fill < 100.0 {
        pieces.push(Element::spacer().background(theme::TRACK).flex(100.0 - fill));
    }
    Some(
        Element::row(pieces)
            .size(None, Some(6.0))
            .radius(3.0)
            .clip_children(),
    )
}

fn window_row(window: &UsageWindow, now: u64) -> Element {
    let value = match window.limit {
        Some(limit) if window.unit != UsageUnit::Percent => format!(
            "{} / {}",
            amount(window.used, window.unit),
            amount(limit, window.unit)
        ),
        // Every row reads like the headline and the bar: what is used.
        _ if window.unit == UsageUnit::Percent => format!(
            "{} usado",
            amount(window.used_percent().unwrap_or(window.used), window.unit)
        ),
        _ => format!("{} usado", amount(window.used, window.unit)),
    };
    // The window's name and its renewal share the line above the bar.
    let mut head = vec![Text::new(&window.label, theme::component_palette()).truncate().render()];
    if let Some(reset) = reset_label(window, now) {
        head.push(
            Text::new(format!("· {reset}"), theme::component_palette())
                .role(TextRole::Small)
                .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                .truncate()
                .render()
                .centered_y(),
        );
    }
    head.push(Element::spacer().flex(1.0));
    head.push(Text::new(value, theme::component_palette()).truncate().render());
    let mut rows = vec![Element::row(head).gap(6.0)];
    if let Some(track) = progress(window) {
        rows.push(track);
    }
    Element::column(rows).gap(6.0)
}

fn account_action(snapshot: &ProviderUsageSnapshot) -> Element {
    let (id, label, icon) = match snapshot.status {
        ProviderUsageStatus::Disconnected => (
            format!("provider_usage_connect:{}", snapshot.provider_id),
            "Conectar",
            Icon::Add,
        ),
        ProviderUsageStatus::NeedsSignIn => (
            format!("provider_usage_connect:{}", snapshot.provider_id),
            "Reconectar",
            Icon::Refresh,
        ),
        _ if snapshot.account_url.is_some() => (
            format!("provider_usage_account:{}", snapshot.provider_id),
            "Abrir cuenta",
            Icon::ArrowRight,
        ),
        _ => (
            format!("provider_usage_connect:{}", snapshot.provider_id),
            "Ver conexión",
            Icon::ArrowRight,
        ),
    };
    // The same outlined action, with its icon first, as every other Settings page.
    Button::new(id, label, crate::settings_ui::control_palette())
        .icon(icon)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .render()
}

fn account_card(
    state: &ViewState,
    snapshot: &ProviderUsageSnapshot,
    width: f32,
    now: u64,
) -> Element {
    let empty = match snapshot.status {
        ProviderUsageStatus::External => "El consumo se consulta en tu cuenta",
        ProviderUsageStatus::Disconnected => "Conecta una cuenta para ver su uso",
        ProviderUsageStatus::NeedsSignIn => "La sesión necesita renovarse",
        ProviderUsageStatus::Loading => "Consultando los límites de tu cuenta…",
        ProviderUsageStatus::Error => "No se pudo consultar el consumo",
        ProviderUsageStatus::Ready => "El proveedor no devolvió una medición",
    };
    usage_card(state, snapshot, width, now, empty, Some(account_action(snapshot)))
}

fn limits_action(id: &str, label: &str, icon: Icon) -> Element {
    Button::new(id, label, crate::settings_ui::control_palette())
        .icon(icon)
        .variant(ButtonVariant::Outline)
        .size(ControlSize::Compact)
        .render()
}

/// Claude Code's subscription, for whoever has its CLI: its limits once it
/// reports them, and before that the offer to read them. Never silent.
fn claude_card(state: &ViewState, width: f32, now: u64) -> Option<Element> {
    state
        .agent_clis
        .iter()
        .find(|cli| cli.id == "claude" && cli.resolved.is_some())?;
    let on = state.claude_limits == claude_limits::Status::On;
    let (empty, message, action) = match &state.claude_limits {
        claude_limits::Status::On => (
            "Los límites aparecen después del próximo turno con Claude Code",
            None,
            Some(limits_action("claude_limits_disable", "Desactivar", Icon::Close)),
        ),
        claude_limits::Status::Off => (
            "Lee aquí los límites de tu suscripción",
            Some("Al activarla, openADE configura el comando de línea de estado de Claude Code para recibir sus límites de uso."),
            Some(limits_action("claude_limits_enable", "Activar lectura de límites", Icon::Add)),
        ),
        claude_limits::Status::Foreign => (
            claude_limits::FOREIGN,
            Some("openADE no la reemplaza. Para leer aquí tus límites, quítala antes de la configuración de Claude Code."),
            None,
        ),
        claude_limits::Status::Refused(reason) => {
            ("No se puede activar la lectura de límites", Some(reason.as_str()), None)
        }
    };
    let snapshot = state
        .provider_usage
        .get(claude_limits::PROVIDER_ID)
        .filter(|_| on)
        .map(Cow::Borrowed)
        .unwrap_or_else(|| {
            Cow::Owned(ProviderUsageSnapshot {
                provider_id: claude_limits::PROVIDER_ID.into(),
                provider_name: claude_limits::PROVIDER_NAME.into(),
                account: None,
                status: if on {
                    ProviderUsageStatus::Ready
                } else {
                    ProviderUsageStatus::Disconnected
                },
                plan_label: None,
                windows: Vec::new(),
                balances: Vec::new(),
                fetched_at_ms: None,
                local: true,
                account_url: None,
                message: message.map(str::to_owned),
            })
        });
    Some(usage_card(state, &snapshot, width, now, empty, action))
}

/// `empty` is said in place of a measurement; a card without `action` has no footer.
fn usage_card(
    state: &ViewState,
    snapshot: &ProviderUsageSnapshot,
    width: f32,
    now: u64,
    empty: &str,
    action: Option<Element>,
) -> Element {
    let compact = width < 500.0;
    let status_color = if stale(snapshot, now)
        || matches!(
            snapshot.status,
            ProviderUsageStatus::Error | ProviderUsageStatus::NeedsSignIn
        ) {
        theme::WARNING
    } else {
        theme::SETTINGS_FOREGROUND_SUBTLE
    };
    let mut titles = vec![
        Text::new(&snapshot.provider_name, theme::component_palette())
            .truncate()
            .render(),
    ];
    let subtitle = crate::fonts::account_label(snapshot
        .account
        .as_deref()
        .unwrap_or(status_label(snapshot.status)));
    titles.push(
        Text::new(&subtitle, theme::component_palette())
            .role(TextRole::Small)
            .ink(status_color)
            .truncate()
            .render(),
    );
    let mut header = vec![
        provider_mark(&snapshot.provider_id),
        Element::column(titles).gap(4.0).flex(1.0),
    ];
    if width >= 420.0 {
        if let Some(plan) = &snapshot.plan_label {
            header.push(plan_badge(plan).centered_y());
        }
    }
    // The account's action sits in the header; a narrow card keeps it below.
    let mut action = action;
    if width >= 420.0 {
        if let Some(action) = action.take() {
            // Its own height: the row would stretch it to a fractional one.
            header.push(action.size(None, Some(28.0)));
        }
    }
    let mut rows = vec![Element::row(header).gap(6.0)];
    if width < 420.0 {
        if let Some(plan) = &snapshot.plan_label {
            rows.push(Element::row(vec![
                plan_badge(plan),
                Element::spacer().flex(1.0),
            ]));
        }
    }
    let measured = !snapshot.windows.is_empty() || !snapshot.balances.is_empty();
    if measured {
        let (value, label, color) = headline(snapshot);
        // The figure and what it is, on one line.
        rows.push(
            Element::row(vec![
                Text::new(value, theme::component_palette())
                    .role(TextRole::Display)
                    .ink(color)
                    .render(),
                Text::new(label, theme::component_palette())
                    .role(TextRole::Small)
                    .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                    .render()
                    .centered_y(),
            ])
            .gap(8.0),
        );
        for balance in snapshot.balances.iter().skip(1) {
            rows.push(
                Element::row(vec![
                    Text::new(&balance.label, theme::component_palette())
                        .role(TextRole::Small)
                        .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                        .render()
                        .flex(1.0),
                    Text::new(
                        format!("{:.2} {}", balance.amount, balance.currency),
                        theme::component_palette(),
                    )
                    .render(),
                ])
                .gap(8.0),
            );
        }
        rows.extend(
            snapshot
                .windows
                .iter()
                .map(|window| window_row(window, now)),
        );
    } else {
        rows.push(Text::new(empty, theme::component_palette()).render());
    }
    // The small print under a card reads as one block, not as separate rows.
    let mut notes = Vec::new();
    if let Some(message) = &snapshot.message {
        notes.push(
            Text::new(message, theme::component_palette())
                .role(TextRole::Small)
                .ink(status_color)
                .render(),
        );
    }
    // The model count shares the same authoritative catalog as Settings and
    // the composer. A missing catalog stays absent, never a invented count.
    // What is known of the measurement, in one line.
    let mut facts = Vec::new();
    if !compact {
        if let Some(provider) = state
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.providers.get(&snapshot.provider_id))
        {
            facts.push(format!("{} modelos en tu catálogo", provider.models.len()));
        }
    }
    if snapshot.fetched_at_ms.is_some()
        || matches!(
            snapshot.status,
            ProviderUsageStatus::Loading | ProviderUsageStatus::Error
        )
    {
        facts.push(updated(snapshot, now));
    }
    if !facts.is_empty() {
        notes.push(
            Text::new(facts.join(" · "), theme::component_palette())
                .role(TextRole::Small)
                .ink(status_color)
                .render(),
        );
    }
    if !notes.is_empty() {
        rows.push(Element::column(notes).gap(3.0));
    }
    // Keep actions independent of copy length and account state. The body
    // absorbs the extra height of its row, so every footer shares a baseline.
    let mut parts = vec![Element::column(rows).gap(10.0).flex(1.0)];
    if let Some(action) = action {
        parts.push(
            Element::column(vec![
                Separator::horizontal(theme::component_palette()).render(),
                Element::row(vec![Element::spacer().flex(1.0), action]),
            ])
            .gap(10.0),
        );
    }
    Card::new(Element::column(parts).gap(10.0), theme::card_palette()).render()
}

fn refresh(state: &ViewState, compact: bool) -> Element {
    if compact {
        IconButton::new("usage_refresh", Icon::Refresh, theme::menu_palette())
            .variant(ButtonVariant::Subtle)
            .size(ControlSize::Regular)
            .loading(state.provider_usage_refreshing)
            .tooltip_with(
                TooltipSpec::new("Actualizar consumo")
                    .placement(TooltipPlacement::Below)
                    .palette(theme::component_palette()),
            )
            .render()
    } else {
        Button::new("usage_refresh", "Actualizar", theme::component_palette())
            .icon(Icon::Refresh)
            .variant(ButtonVariant::Outline)
            .size(ControlSize::Compact)
            .loading(state.provider_usage_refreshing)
            .render()
    }
}

fn popup_account(snapshot: &ProviderUsageSnapshot, now: u64) -> (Element, f32) {
    let (action, action_label, icon) = match snapshot.status {
        ProviderUsageStatus::Disconnected => (
            format!("provider_usage_connect:{}", snapshot.provider_id),
            "Conectar cuenta",
            Icon::Add,
        ),
        ProviderUsageStatus::NeedsSignIn => (
            format!("provider_usage_connect:{}", snapshot.provider_id),
            "Reconectar cuenta",
            Icon::Refresh,
        ),
        _ if snapshot.account_url.is_some() => (
            format!("provider_usage_account:{}", snapshot.provider_id),
            "Abrir cuenta en el navegador",
            Icon::ArrowRight,
        ),
        _ => (
            format!("provider_usage_connect:{}", snapshot.provider_id),
            "Ver conexión del proveedor",
            Icon::Settings,
        ),
    };
    let mut identity = vec![
        Text::new(&snapshot.provider_name, theme::component_palette())
            .role(TextRole::Label)
            .truncate()
            .render()
            .size(None, Some(16.0)),
    ];
    if let Some(plan) = &snapshot.plan_label {
        identity.push(
            Text::new(plan, theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .truncate()
                .render()
                .size(None, Some(16.0)),
        );
    }
    if let Some(account) = &snapshot.account {
        identity.push(
            Text::new(crate::fonts::account_label(account), theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .truncate()
                .render()
                .size(None, Some(16.0)),
        );
    }
    let header_height = (identity.len() as f32 * 18.0 - 2.0).max(36.0);
    let account = snapshot
        .account
        .as_deref()
        .unwrap_or(status_label(snapshot.status));
    let mut help = vec![crate::fonts::account_label(account)];
    if let Some(plan) = &snapshot.plan_label {
        help.push(plan.clone());
    }
    if let Some(message) = &snapshot.message {
        help.push(message.clone());
    }
    // Narrow numeric columns may elide unusually large limits. Preserve the
    // exact provider values in the identity tooltip as well as Settings.
    for window in &snapshot.windows {
        help.push(match window.limit {
            Some(limit) => format!(
                "{}: {} / {}",
                window.label,
                amount(window.used, window.unit),
                amount(limit, window.unit)
            ),
            None => format!(
                "{}: {} usado",
                window.label,
                amount(window.used, window.unit)
            ),
        });
    }
    for balance in &snapshot.balances {
        help.push(format!(
            "{}: {:.2} {}",
            balance.label, balance.amount, balance.currency
        ));
    }
    let identity = Tooltip::new(
        Element::column(identity).gap(2.0).flex(1.0),
        format!("provider_usage_identity:{}", snapshot.provider_id),
        &snapshot.provider_name,
        theme::component_palette(),
    )
    .description(help.join("\n"))
    .max_width(320.0)
    .render()
    .flex(1.0);
    let action = IconButton::new(action, icon, theme::component_palette())
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Regular)
        .tooltip_with(
            TooltipSpec::new(action_label)
                .description(format!("{} · {account}", snapshot.provider_name))
                .placement(TooltipPlacement::Above),
        )
        .render();
    let mut rows = vec![
        Element::row(vec![
            Element::icon(icon_for_provider(&snapshot.provider_id))
                .foreground(theme::ICON_PRIMARY)
                .size(Some(24.0), Some(header_height)),
            identity,
            action,
        ])
        .gap(8.0)
        .size(None, Some(header_height)),
    ];
    let mut content_height = header_height;
    // Show each measured value once. The Settings page retains the expanded
    // explanation; this menu does not repeat the first window as a headline.
    for balance in &snapshot.balances {
        let value = if balance.currency.eq_ignore_ascii_case("USD") {
            format!("${:.2}", balance.amount)
        } else {
            format!("{:.2} {}", balance.amount, balance.currency)
        };
        rows.push(
            Element::row(vec![
                Text::new(&balance.label, theme::component_palette())
                    .muted()
                    .truncate()
                    .render()
                    .flex(1.0),
                Text::new(value, theme::component_palette())
                    .role(TextRole::Label)
                    .truncate()
                    .render()
                    .size(Some(112.0), Some(20.0)),
            ])
            .gap(8.0)
            .size(None, Some(20.0)),
        );
        content_height += 20.0;
    }
    for window in &snapshot.windows {
        let percent = window.used_percent();
        let value = match window.limit {
            Some(limit) if window.unit != UsageUnit::Percent => format!(
                "{} / {}",
                amount(window.used, window.unit),
                amount(limit, window.unit)
            ),
            _ => format!("{} usado", amount(window.used, window.unit)),
        };
        let mut parts = vec![Element::row(vec![
            Text::new(&window.label, theme::component_palette())
                .muted()
                .truncate()
                .render()
                .flex(1.0),
            Text::new(value, theme::component_palette())
                .ink(percent.map_or(theme::FOREGROUND, usage_color))
                .truncate()
                .render()
                .size(Some(112.0), Some(18.0)),
        ])
        .gap(8.0)
        .size(None, Some(18.0))];
        let mut height = 18.0;
        if let Some(track) = progress(window) {
            parts.push(track);
            height += 8.0;
        }
        if let Some(reset) = reset_label(window, now) {
            parts.push(
                Text::new(reset, theme::component_palette())
                    .font_size(10.5)
                    .muted()
                    .truncate()
                    .render()
                    .size(None, Some(14.0)),
            );
            height += 18.0;
        }
        rows.push(Element::column(parts).gap(4.0).size(None, Some(height)));
        content_height += height;
    }
    let measured = !snapshot.windows.is_empty() || !snapshot.balances.is_empty();
    let notice = if stale(snapshot, now) && measured {
        format!(
            "Sin actualizar · {}",
            snapshot.fetched_at_ms.map_or_else(
                || "último dato".into(),
                |timestamp| elapsed_label(timestamp, now)
            )
        )
    } else if measured {
        updated(snapshot, now)
    } else {
        match snapshot.status {
            ProviderUsageStatus::External => "Consulta el consumo en su web",
            ProviderUsageStatus::Loading => "Consultando consumo…",
            ProviderUsageStatus::NeedsSignIn => "Vuelve a conectar tu cuenta",
            ProviderUsageStatus::Disconnected => "Conecta una cuenta para ver su uso",
            ProviderUsageStatus::Error => "No se pudo actualizar el consumo",
            ProviderUsageStatus::Ready => "Sin datos de consumo disponibles",
        }
        .into()
    };
    rows.push(
        Text::new(notice, theme::component_palette())
            .font_size(10.5)
            .ink(
                if stale(snapshot, now) || snapshot.status == ProviderUsageStatus::NeedsSignIn {
                    theme::WARNING
                } else {
                    theme::FOREGROUND_SUBTLE
                },
            )
            .truncate()
            .render()
            .size(None, Some(16.0)),
    );
    content_height += 16.0 + 8.0 * rows.len().saturating_sub(1) as f32 + 12.0;
    (
        Element::column(rows)
            .gap(8.0)
            .padding(6.0)
            .size(None, Some(content_height)),
        content_height,
    )
}

pub(crate) fn popup(state: &ViewState) -> Element {
    let now = now_ms();
    let mut cards = Vec::new();
    let mut accounts: Vec<_> = roster()
        .filter(|(id, _)| {
            connected(state, id)
                || state
                    .provider_connections
                    .get(*id)
                    .is_some_and(|connection| connection.needs_sign_in)
        })
        .collect();
    // The provider in the compact summary is the first account inspected.
    // Remaining accounts keep their stable catalog order.
    accounts.sort_by_key(|(id, _)| *id != active_account(state));
    let mut body_height = 0.0;
    for (id, name) in accounts {
        if !cards.is_empty() {
            cards.push(Separator::horizontal(theme::component_palette()).render());
            body_height += 1.0;
        }
        let (row, height) = popup_account(&snapshot(state, id, name), now);
        cards.push(row);
        body_height += height;
    }
    if cards.is_empty() {
        let (id, name, _) = provider_connections::PROVIDERS
            .iter()
            .find(|(id, _, _)| *id == state.native_provider_id)
            .unwrap_or(&provider_connections::PROVIDERS[0]);
        let (row, height) = popup_account(&snapshot(state, id, name), now);
        cards.push(row);
        body_height = height;
    }
    popup_panel(state, cards, body_height)
}

fn popup_panel(state: &ViewState, cards: Vec<Element>, body_height: f32) -> Element {
    PopoverPanel::new(theme::component_palette())
        .header(
            openide_ui::PanelHeader::new("", theme::menu_palette())
                .leading(
                    Text::new("Cuenta y uso", theme::component_palette())
                        .role(TextRole::Label)
                        .render()
                        .size(None, Some(18.0))
                        .centered_y(),
                )
                .trailing(vec![refresh(state, true)])
                .sizing(crate::ui_sizing::TALL_HEADER)
                .render(),
        )
        .body(
            // The accounts are summed by hand: the body has a known height
            // before layout, which the tests of a short window pin.
            ScrollArea::new("provider_usage_popup", Element::column(cards))
                .tint(theme::FOREGROUND_SUBTLE)
                .render()
                .size(None, Some(body_height.min(POPUP_BODY_MAX_HEIGHT))),
        )
        .footer(
            Button::new(
                "usage_details",
                "Ver uso y proveedores",
                theme::menu_palette(),
            )
            .trailing_icon(Icon::ArrowRight)
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Regular)
            .render()
            .size(None, Some(32.0)),
        )
        .render()
}

/// The sidebar's account row: the provider's mark, "Cuenta y uso" over the
/// headline in its status color, and a chevron.
fn usage_anchor(
    icon: Icon,
    summary: String,
    color: Color,
    bar: Option<Element>,
    count: usize,
    open: bool,
    width: f32,
) -> Element {
    let centered = |icon: Icon, ink: Color| {
        Element::panel(
            Element::icon(icon)
                .foreground(ink)
                .size(Some(16.0), Some(16.0)),
        )
        .center_content()
    };
    let palette = theme::action_palette(theme::SIDEBAR, theme::FOREGROUND);
    let mut row = ListRow::new(
        "usage_toggle",
        "Cuenta y uso",
        ComponentPalette {
            surface: if open {
                theme::SELECTED
            } else {
                theme::SIDEBAR
            },
            ..palette
        },
    )
    .leading(centered(icon, theme::ICON_PRIMARY))
    .subtitle(summary)
    .subtitle_ink(color)
    .trailing(centered(Icon::ChevronRight, theme::FOREGROUND_SUBTLE).size(Some(16.0), Some(32.0)))
    .filled()
    .sizing(crate::ui_sizing::USAGE_ANCHOR);
    // Beside the title, so the row keeps its two lines and its height.
    if let Some(bar) = bar.filter(|_| width >= 200.0) {
        row = row.meta_element(bar.size(Some(48.0), Some(4.0)).centered_y());
    }
    if count > 1 {
        row = row.meta_element(
            Text::new(format!("+{}", count - 1), theme::component_palette())
                .role(TextRole::Small)
                .muted()
                .render(),
        );
    }
    // Sized after rendering: `ListRow::width` would also clip the row.
    row.render().size(Some(width), Some(44.0))
}

/// Compact card for the sidebar footer. Its height stays independent of the
/// number of accounts, while the anchored body has its own scroll position.
pub(crate) fn sidebar_card(state: &ViewState, width: f32) -> Element {
    let padding = 6.0;
    let summary_height = 32.0;
    let anchor_height = summary_height + 2.0 * padding;
    let content_width = (width - 2.0 * padding).max(0.0);
    let available = |id: &str| {
        connected(state, id)
            || state
                .provider_connections
                .get(id)
                .is_some_and(|connection| connection.needs_sign_in)
    };
    let count = roster().filter(|(id, _)| available(id)).count();
    let selected = roster()
        .find(|(id, _)| *id == active_account(state) && available(id))
        .or_else(|| roster().find(|(id, _)| available(id)));
    let (icon, summary, help, color, bar) = if let Some((id, name)) = selected {
        let snapshot = snapshot(state, id, name);
        let (value, label, color) = headline(&snapshot);
        let out_of_date = stale(&snapshot, now_ms());
        // A plan's windows are told apart by name: "58% semanal".
        let plan = tightest(&snapshot).filter(|(window, _)| window.unit == UsageUnit::Percent);
        let detail = if out_of_date {
            "Sin actualizar".to_owned()
        } else if let Some((window, _)) = plan {
            // Said before it is lowered: the catalog knows the label as written.
            let label = crate::i18n::text(&window.label).unwrap_or_else(|| window.label.clone());
            format!("{value} {}", label.to_lowercase())
        } else {
            value.clone()
        };
        let bar = plan
            .filter(|_| !out_of_date)
            .and_then(|(window, _)| progress(window));
        (
            icon_for_provider(id),
            format!("{name} · {detail}"),
            format!(
                "{name}\n{value} · {label}\n{}",
                updated(&snapshot, now_ms())
            ),
            if out_of_date || snapshot.status == ProviderUsageStatus::NeedsSignIn {
                theme::WARNING
            } else {
                color
            },
            bar,
        )
    } else {
        (
            crate::provider_icons::CONTEXT_RING,
            "Conectar proveedor".into(),
            "Conecta un proveedor para consultar su cuenta y consumo.".into(),
            theme::FOREGROUND_SUBTLE,
            None,
        )
    };
    let anchor = usage_anchor(
        icon,
        summary,
        color,
        bar,
        count,
        state.provider_usage_open,
        content_width,
    );
    let anchor = Tooltip::new(
        anchor,
        "usage_toggle",
        "Cuenta y uso",
        theme::component_palette(),
    )
    .description(help)
    .max_width(320.0)
    .render();
    let surface = Popover::new(
        anchor,
        popup(state),
        state.provider_usage_open,
        "usage_dismiss",
        theme::menu_palette(),
    )
    .placement(PopoverPlacement::TopEnd)
    .width(POPUP_WIDTH)
    .render()
    .padding(padding)
    .size(Some(width), Some(anchor_height + 2.0 * padding));
    Element::column(vec![surface])
}

/// Short windows keep account usage reachable beside the account footer.
pub(crate) fn sidebar_compact(state: &ViewState) -> Element {
    let anchor = IconButton::new(
        "usage_toggle",
        crate::provider_icons::CONTEXT_RING,
        theme::sidebar_palette(),
    )
    .variant(ButtonVariant::Ghost)
    .size(ControlSize::Dense)
    .active(state.provider_usage_open)
    .active_hover(ActiveHover::Palette)
    .tooltip_with(TooltipSpec::new("Cuenta y uso").placement(TooltipPlacement::Right))
    .render();
    let surface = Popover::new(
        anchor,
        popup(state),
        state.provider_usage_open,
        "usage_dismiss",
        theme::menu_palette(),
    )
    .placement(PopoverPlacement::TopEnd)
    .width(POPUP_WIDTH)
    .render()
    .size(Some(24.0), Some(24.0));
    Element::column(vec![surface]).size(Some(24.0), Some(24.0))
}

/// Settings cards for the same four-account catalog used by provider setup.
/// The outer Settings page owns scrolling and the page heading.
pub(crate) fn usage_page(state: &ViewState, content_width: f32) -> Vec<Element> {
    let count = roster().filter(|(id, _)| connected(state, id)).count();
    let subtle = ComponentPalette {
        muted: theme::SETTINGS_FOREGROUND_SUBTLE,
        ..theme::component_palette()
    };
    let intro = PageHeader::new("Suscripciones y consumo", subtle)
        .description(format!(
            "{count} {} · Información de cada cuenta",
            if count == 1 {
                "proveedor conectado"
            } else {
                "proveedores conectados"
            }
        ))
        .trailing(refresh(state, false))
        .trailing_top(true)
        .stacked(content_width < 500.0)
        .sizing(crate::ui_sizing::USAGE_HEADER)
        .render();
    let mut cards = vec![intro];
    let now = now_ms();
    let two_columns = content_width >= 700.0;
    let card_width = if two_columns {
        (content_width - 12.0) / 2.0
    } else {
        content_width
    };
    // Accounts with something to say come first, measured ones before the
    // rest; the ones never connected wait in one compact list below them.
    let mut accounts = Vec::new();
    let mut unconnected = Vec::new();
    for (id, name, _) in provider_connections::PROVIDERS {
        let snapshot = snapshot(state, id, name);
        if snapshot.status == ProviderUsageStatus::Disconnected {
            unconnected.push(settings_parts::settings_row(
                *name,
                "Conecta una cuenta para ver su uso",
                account_action(&snapshot),
                content_width < 500.0,
            ));
        } else {
            accounts.push((connected(state, id), account_card(state, &snapshot, card_width, now)));
        }
    }
    if let Some(card) = claude_card(state, card_width, now) {
        accounts.push((connected(state, claude_limits::PROVIDER_ID), card));
    }
    accounts.sort_by_key(|(connected, _)| !connected);
    let accounts = accounts
        .into_iter()
        .map(|(_, card)| card.size(Some(card_width), None));
    if two_columns {
        // Two independent columns: a tall card does not stretch its neighbour.
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for (index, card) in accounts.enumerate() {
            if index % 2 == 0 { left.push(card) } else { right.push(card) }
        }
        cards.push(
            Element::row(vec![
                Element::column(left).gap(12.0).size(Some(card_width), None),
                Element::column(right).gap(12.0).size(Some(card_width), None),
            ])
            .gap(12.0),
        );
    } else {
        cards.extend(accounts);
    }
    if !unconnected.is_empty() {
        cards.push(settings_parts::settings_group("Sin conectar", unconnected));
    }
    cards.push(Text::new("Los límites y el saldo pertenecen a cada proveedor. El contexto de una conversación y los precios del catálogo no representan el consumo de tu suscripción.", theme::component_palette()).role(TextRole::Small).ink(theme::SETTINGS_FOREGROUND_SUBTLE).render());
    cards
}

/// The catalog's connection detail can embed the exact same account data.
pub(crate) fn provider_card(state: &ViewState, id: &str, width: f32) -> Option<Element> {
    let (_, name, _) = provider_connections::PROVIDERS
        .iter()
        .find(|(provider, _, _)| *provider == id)?;
    Some(account_card(
        state,
        &snapshot(state, id, name),
        width,
        now_ms(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_connections::ConnectionSummary;
    use openide_ui::{DrawCommand, FontFamily, Frame, Key, Point, Size, UiAction, UiEvent, UiTree};

    fn fixture() -> ViewState {
        let mut state = crate::view::tests::empty_state();
        state.native_provider_id = "opencode".into();
        let now = now_ms();
        for (id, name, _) in provider_connections::PROVIDERS {
            let connected = matches!(*id, "codex" | "openrouter" | "opencode");
            state.provider_connections.insert(
                (*id).into(),
                ConnectionSummary {
                    configured: connected,
                    account: connected.then(|| "cuenta@example.com".into()),
                    ..Default::default()
                },
            );
            let mut account = ProviderUsageSnapshot {
                provider_id: (*id).into(),
                provider_name: (*name).into(),
                status: if matches!(*id, "nvidia" | "minimax" | "zai") {
                    ProviderUsageStatus::Disconnected
                } else {
                    ProviderUsageStatus::External
                },
                plan_label: None,
                account: connected.then(|| "cuenta@example.com".into()),
                windows: Vec::new(),
                balances: Vec::new(),
                fetched_at_ms: None,
                local: false,
                account_url: Some("https://example.com/account".into()),
                message: None,
            };
            if *id == "codex" {
                account.plan_label = Some("Plan de ChatGPT".into());
                account.message = Some("Los límites de tu plan se consultan en ChatGPT.".into());
            }
            if *id == "openrouter" {
                account.status = ProviderUsageStatus::Ready;
                account.fetched_at_ms = Some(now - 120_000);
                account.plan_label = Some("Créditos de API".into());
                account.message = Some(
                    "Consumo de la clave conectada; el saldo total se consulta en OpenRouter."
                        .into(),
                );
                account.windows.push(UsageWindow {
                    label: "Límite de la clave".into(),
                    used: 3.0,
                    limit: Some(10.0),
                    remaining: Some(7.0),
                    unit: UsageUnit::Usd,
                    reset_at_ms: None,
                    reset_label: None,
                });
            }
            if *id == "opencode" {
                account.status = ProviderUsageStatus::Ready;
                account.plan_label = Some("OpenCode Go".into());
                account.fetched_at_ms = Some(now);
                account.windows = vec![
                    UsageWindow {
                        label: "Sesión · 5 h".into(),
                        used: 82.0,
                        limit: Some(100.0),
                        remaining: Some(18.0),
                        unit: UsageUnit::Percent,
                        reset_at_ms: Some(now + 5_400_000),
                        reset_label: None,
                    },
                    UsageWindow {
                        label: "Semanal".into(),
                        used: 36.0,
                        limit: Some(100.0),
                        remaining: Some(64.0),
                        unit: UsageUnit::Percent,
                        reset_at_ms: Some(now + 345_600_000),
                        reset_label: None,
                    },
                ];
            }
            state.provider_usage.insert((*id).into(), account);
        }
        state
    }

    fn frame_text(frame: &Frame) -> String {
        frame
            .commands
            .iter()
            .filter_map(|command| {
                if let DrawCommand::Text { text, .. } = command {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn point_for(frame: &Frame, id: &str) -> Point {
        let rect = frame
            .regions
            .iter()
            .find(|region| region.id == id)
            .unwrap_or_else(|| panic!("missing action {id}"))
            .rect;
        Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
    }

    fn click(tree: &mut UiTree, frame: &Frame, id: &str) -> Vec<UiAction> {
        let point = point_for(frame, id);
        tree.handle_event(UiEvent::PointerDown(point));
        tree.handle_event(UiEvent::PointerUp(point))
    }

    fn native_tree(root: Element) -> UiTree {
        use eframe::egui;
        use openide_ui::FontMetrics;
        let context = egui::Context::default();
        crate::fonts::install(&context);
        context.begin_pass(egui::RawInput::default());
        let mut tree = UiTree::new(root);
        tree.set_reduced_motion(true);
        let mono = egui::FontId::monospace(14.0);
        tree.set_metrics(context.fonts_mut(|fonts| FontMetrics {
            char_width: fonts.glyph_width(&mono, 'M'),
            line_height: fonts.row_height(&mono),
        }));
        let sans = egui::FontId::proportional(14.0);
        tree.set_sans_metrics(context.fonts_mut(|fonts| {
            FontMetrics {
                char_width: "abcdefghijklmnopqrstuvwxyz"
                    .chars()
                    .map(|character| fonts.glyph_width(&sans, character))
                    .sum::<f32>()
                    / 26.0,
                line_height: fonts.row_height(&sans),
            }
        }));
        tree.set_text_measurer(move |text, size, family| {
            context.fonts_mut(|fonts| {
                fonts
                    .layout_no_wrap(
                        text.to_owned(),
                        egui::FontId::new(
                            size,
                            match family {
                                FontFamily::Sans | FontFamily::SansItalic | FontFamily::SansMedium | FontFamily::SansBold => egui::FontFamily::Proportional,
                                FontFamily::Monospace => egui::FontFamily::Monospace,
                            },
                        ),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
            })
        });
        tree
    }

    fn popup_tree(state: &ViewState, width: f32) -> UiTree {
        let mut tree = native_tree(
            Element::row(vec![
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    sidebar_card(state, width.min(264.0)),
                ])
                .size(Some(width.min(264.0)), None)
                .background(theme::SIDEBAR),
                Element::spacer().flex(1.0),
            ])
            .background(theme::BACKGROUND),
        );
        tree.set_reduced_motion(true);
        tree
    }

    /// Codex as the local read leaves it: (label, used percent, reset in hours).
    fn codex_local(windows: &[(&str, f64, u64)], age_ms: u64) -> ViewState {
        let mut state = fixture();
        only_account(&mut state, "codex");
        let now = now_ms();
        let account = state.provider_usage.get_mut("codex").unwrap();
        account.status = ProviderUsageStatus::Ready;
        account.local = true;
        account.plan_label = Some("ChatGPT Pro".into());
        account.fetched_at_ms = Some(now - age_ms);
        account.message = Some("Límites de tu última sesión local de Codex.".into());
        account.windows = windows
            .iter()
            .map(|(label, used, hours)| UsageWindow {
                label: (*label).into(),
                used: *used,
                limit: Some(100.0),
                remaining: Some(100.0 - used),
                unit: UsageUnit::Percent,
                reset_at_ms: Some(now + hours * 3_600_000 + 60_000),
                reset_label: None,
            })
            .collect();
        state
    }

    #[test]
    fn local_codex_windows_show_bars_plan_reset_and_honest_age_everywhere() {
        let bar = |frame: &Frame, color: Color, height: f32| {
            frame.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect, color: painted, .. }
                    if *painted == color && rect.height == height && rect.width > 0.0))
        };
        // Twenty hours old: far past STALE_AFTER, and still not a failure.
        let mut state = codex_local(&[("Semanal", 58.0, 72)], 72_000_000);
        let mut tree = native_tree(sidebar_card(&state, 264.0));
        let frame = tree.layout(Size::new(264.0, 160.0));
        let painted = frame_text(&frame);
        assert!(painted.contains("Codex · 58% semanal"), "{painted}");
        assert!(!painted.contains("Sin actualizar"), "{painted}");
        assert!(bar(&frame, theme::FOREGROUND, 4.0) && bar(&frame, theme::TRACK, 4.0));
        let row = frame.regions.iter().find(|r| r.id == "usage_toggle").unwrap().rect;
        assert_eq!(row.height, 44.0);
        // A narrow sidebar keeps the text and gives up the bar.
        let narrow = native_tree(sidebar_card(&state, 180.0)).layout(Size::new(180.0, 160.0));
        assert!(!bar(&narrow, theme::TRACK, 4.0));

        let mut card = native_tree(provider_card(&state, "codex", 420.0).unwrap());
        let frame = card.layout(Size::new(420.0, 600.0));
        let painted = frame_text(&frame);
        for text in ["58%", "usado · Semanal", "58% usado", "ChatGPT Pro", "Se renueva en 3 d 0 h", "Medido por Codex · hace 20 h"] {
            assert!(painted.contains(text), "missing {text}: {painted}");
        }
        assert!(!painted.contains("se consulta en tu cuenta"), "{painted}");
        assert!(bar(&frame, theme::FOREGROUND, 6.0));

        state.provider_usage_open = true;
        let frame = popup_tree(&state, 360.0).layout(Size::new(360.0, 720.0));
        let painted = frame_text(&frame);
        for text in ["58% usado", "ChatGPT Pro", "Se renueva en 3 d 0 h", "Medido por Codex · hace 20 h"] {
            assert!(painted.contains(text), "missing {text}: {painted}");
        }
        assert!(!painted.contains("Sin actualizar") && !painted.contains("en su web"));

        // The summary follows the tightest window, in its warning ink from 80 %.
        let state = codex_local(&[("5 horas", 30.0, 2), ("Semanal", 85.0, 72)], 60_000);
        let frame = native_tree(sidebar_card(&state, 264.0)).layout(Size::new(264.0, 160.0));
        assert!(frame_text(&frame).contains("Codex · 85% semanal"));
        assert!(bar(&frame, theme::WARNING, 4.0));
        let account = &state.provider_usage["codex"];
        assert_eq!(tightest(account).map(|(window, used)| (window.label.as_str(), used)), Some(("Semanal", 85.0)));
        assert_eq!(headline(account).2, theme::WARNING);
    }

    /// The Claude Code CLI found, its limits being read, and these
    /// (label, used percent, hours to reset) windows reported two minutes ago.
    fn with_claude(mut state: ViewState, windows: &[(&str, f64, u64)]) -> ViewState {
        let now = now_ms();
        state.agent_clis = vec![crate::view::AgentCliRowView {
            id: "claude".into(),
            label: "Claude Code".into(),
            resolved: Some("/usr/bin/claude".into()),
            manual: String::new(),
        }];
        state.claude_limits = claude_limits::Status::On;
        if windows.is_empty() {
            return state;
        }
        state.provider_usage.insert(
            claude_limits::PROVIDER_ID.into(),
            ProviderUsageSnapshot {
                provider_id: claude_limits::PROVIDER_ID.into(),
                provider_name: claude_limits::PROVIDER_NAME.into(),
                account: None,
                status: ProviderUsageStatus::Ready,
                plan_label: Some("Claude Max".into()),
                windows: windows
                    .iter()
                    .map(|(label, used, hours)| UsageWindow {
                        label: (*label).into(),
                        used: *used,
                        limit: Some(100.0),
                        remaining: Some(100.0 - used),
                        unit: UsageUnit::Percent,
                        reset_at_ms: Some(now + hours * 3_600_000 + 60_000),
                        reset_label: None,
                    })
                    .collect(),
                balances: Vec::new(),
                fetched_at_ms: Some(now - 120_000),
                local: true,
                account_url: Some("https://claude.ai/settings/usage".into()),
                message: Some("Límites de tu última sesión de Claude Code.".into()),
            },
        );
        state
    }

    #[test]
    fn claude_code_card_offers_the_reading_and_shows_its_limits_once_reported() {
        let page = |state: &ViewState| {
            let mut tree = native_tree(Element::column(usage_page(state, 860.0)).gap(16.0));
            let frame = tree.layout(Size::new(860.0, 1_400.0));
            (tree, frame)
        };
        let has = |frame: &Frame, id: &str| frame.regions.iter().any(|region| region.id == id);
        // Without its CLI there is nothing to offer.
        let mut state = fixture();
        state.claude_limits = claude_limits::Status::Off;
        assert!(!frame_text(&page(&state).1).contains("Claude Code"));

        let mut state = with_claude(fixture(), &[]);
        state.claude_limits = claude_limits::Status::Off;
        let (mut tree, frame) = page(&state);
        let painted = frame_text(&frame);
        assert!(painted.contains("Activar lectura de límites"), "{painted}");
        assert!(painted.contains("línea de estado de Claude Code"), "{painted}");
        assert_eq!(
            click(&mut tree, &frame, "claude_limits_enable"),
            vec![UiAction::Clicked { id: "claude_limits_enable".into() }]
        );

        state.claude_limits = claude_limits::Status::On;
        let (mut tree, frame) = page(&state);
        let painted = frame_text(&frame);
        assert!(
            painted.contains("Los límites aparecen después del próximo turno con Claude Code"),
            "{painted}"
        );
        assert!(!has(&frame, "claude_limits_enable"));
        assert_eq!(
            click(&mut tree, &frame, "claude_limits_disable"),
            vec![UiAction::Clicked { id: "claude_limits_disable".into() }]
        );

        // Someone else's status line, or settings that cannot be read: said, and nothing offered.
        for (status, said) in [
            (claude_limits::Status::Foreign, claude_limits::FOREIGN),
            (claude_limits::Status::Refused("Motivo de prueba.".into()), "Motivo de prueba."),
        ] {
            state.claude_limits = status;
            let frame = page(&state).1;
            let painted = frame_text(&frame);
            assert!(painted.contains(said), "{painted}");
            assert!(!has(&frame, "claude_limits_enable") && !has(&frame, "claude_limits_disable"));
        }

        let state = with_claude(fixture(), &[("5 horas", 30.0, 2), ("Semanal", 85.0, 72)]);
        let frame = page(&state).1;
        let painted = frame_text(&frame);
        for expected in [
            "Claude Max",
            "85%",
            "usado · Semanal",
            "30% usado",
            "85% usado",
            "Se renueva en 2 h",
            "Medido por Claude Code · hace 2 min",
        ] {
            assert!(painted.contains(expected), "{expected}: {painted}");
        }
        assert!(has(&frame, "claude_limits_disable"));
        assert!(frame.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. }
                if *color == theme::WARNING && rect.height == 6.0 && rect.width > 0.0)));
    }

    #[test]
    fn claude_code_limits_join_the_summary_and_follow_the_selected_backend() {
        let sidebar = |state: &ViewState| {
            frame_text(&native_tree(sidebar_card(state, 264.0)).layout(Size::new(264.0, 160.0)))
        };
        let mut state = with_claude(
            codex_local(&[("Semanal", 58.0, 72)], 60_000),
            &[("5 horas", 30.0, 2), ("Semanal", 85.0, 72)],
        );
        // A native session on Codex keeps reading Codex.
        assert_eq!(glance(&state).unwrap().title, "Codex · 58% usado · Semanal");
        let painted = sidebar(&state);
        assert!(painted.contains("Codex · 58% semanal") && painted.contains("+1"), "{painted}");

        // A Claude Code session reads its own subscription, tightest window first.
        state.selected_backend = "cli:claude".into();
        let usage = glance(&state).unwrap();
        assert_eq!(usage.title, "Claude Code · 85% usado · Semanal");
        assert_eq!(usage.ink, Some(theme::WARNING));
        assert!(usage.detail.ends_with("Medido por Claude Code · hace 2 min"), "{}", usage.detail);
        assert!(sidebar(&state).contains("Claude Code · 85% semanal"));

        // A Codex CLI session reads Codex whatever the native provider is.
        state.selected_backend = "cli:codex".into();
        state.native_provider_id = "openrouter".into();
        assert_eq!(glance(&state).unwrap().title, "Codex · 58% usado · Semanal");

        // Both accounts are in the popover, the active one first.
        state.selected_backend = "cli:claude".into();
        state.provider_usage_open = true;
        let frame = popup_tree(&state, 360.0).layout(Size::new(360.0, 720.0));
        let painted = frame_text(&frame);
        let (claude, codex) = (painted.find("Claude Max").unwrap(), painted.find("ChatGPT Pro").unwrap());
        assert!(claude < codex, "{painted}");
        assert!(frame.regions.iter().any(|r| r.id == "provider_usage_account:claude-code"));

        // With no other account measured, Claude Code is the one shown.
        state.selected_backend = "native".into();
        state.provider_connections.get_mut("codex").unwrap().configured = false;
        assert_eq!(glance(&state).unwrap().title, "Claude Code · 85% usado · Semanal");
        assert!(sidebar(&state).contains("Claude Code · 85% semanal"));
        // Reading turned off: its limits leave every surface.
        state.provider_usage.remove(claude_limits::PROVIDER_ID);
        assert!(glance(&state).is_none());
    }

    fn only_account(state: &mut ViewState, selected: &str) {
        state.native_provider_id = selected.into();
        for (id, connection) in &mut state.provider_connections {
            connection.configured = id == selected;
            connection.needs_sign_in = false;
        }
    }

    #[test]
    fn one_account_popover_has_content_height_and_real_32px_actions_without_empty_scroll() {
        for (provider, maximum_height) in [("codex", 200.0), ("opencode", 300.0)] {
            let mut state = fixture();
            only_account(&mut state, provider);
            state.provider_usage_open = true;
            for width in [280.0, 360.0, 900.0] {
                let mut tree = popup_tree(&state, width);
                let frame = tree.layout(Size::new(width, 720.0));
                let surface = frame
                    .regions
                    .iter()
                    .find(|r| r.id == "popover_surface:usage_dismiss")
                    .unwrap()
                    .rect;
                assert!(
                    surface.width <= POPUP_WIDTH && surface.height <= maximum_height,
                    "{provider} has an oversized popup: {surface:?}"
                );
                assert!(
                    tree.is_scroll_at_end("provider_usage_popup", 0.1),
                    "one account should not need scrolling"
                );
                for id in [
                    "usage_refresh".to_owned(),
                    format!("provider_usage_account:{provider}"),
                ] {
                    let rect = frame
                        .regions
                        .iter()
                        .find(|r| r.id == id && r.kind == openide_ui::HitKind::Button)
                        .unwrap()
                        .rect;
                    assert_eq!((rect.width, rect.height), (32.0, 32.0), "{id}: {rect:?}");
                    assert!(
                        rect.x >= surface.x
                            && rect.x + rect.width <= surface.x + surface.width + 0.1
                    );
                    assert!(
                        rect.y >= surface.y
                            && rect.y + rect.height <= surface.y + surface.height + 0.1
                    );
                    assert!(click(&mut tree, &frame, &id).iter().any(
                        |action| matches!(action, UiAction::Clicked { id: actual } if actual == &id)
                    ));
                }
                if provider == "codex" {
                    assert!(frame_text(&frame).contains("Plan de ChatGPT"));
                    assert!(!frame_text(&frame).contains("Límites informados por tus proveedores"));
                }
            }
        }
    }

    #[test]
    fn compact_popover_preserves_connection_loading_error_and_stale_states() {
        for status in [
            ProviderUsageStatus::Disconnected,
            ProviderUsageStatus::NeedsSignIn,
            ProviderUsageStatus::Loading,
            ProviderUsageStatus::Error,
            ProviderUsageStatus::Ready,
        ] {
            let mut state = fixture();
            only_account(&mut state, "codex");
            state.provider_usage_open = true;
            let connection = state.provider_connections.get_mut("codex").unwrap();
            connection.configured = status != ProviderUsageStatus::Disconnected;
            connection.needs_sign_in = status == ProviderUsageStatus::NeedsSignIn;
            let account = state.provider_usage.get_mut("codex").unwrap();
            account.status = status;
            let mut tree = popup_tree(&state, 360.0);
            let frame = tree.layout(Size::new(360.0, 480.0));
            let visible = frame_text(&frame);
            let (notice, action) = match status {
                ProviderUsageStatus::Disconnected => {
                    ("Conecta una cuenta", "provider_usage_connect:codex")
                }
                ProviderUsageStatus::NeedsSignIn => {
                    ("Vuelve a conectar", "provider_usage_connect:codex")
                }
                ProviderUsageStatus::Loading => {
                    ("Consultando consumo", "provider_usage_account:codex")
                }
                ProviderUsageStatus::Error => {
                    ("No se pudo actualizar", "provider_usage_account:codex")
                }
                _ => ("Sin datos de consumo", "provider_usage_account:codex"),
            };
            assert!(visible.contains(notice), "{status:?}: {visible}");
            assert!(
                !visible.contains('%'),
                "unknown usage must never become a percentage"
            );
            assert!(tree.focus(action));
        }
        let mut state = fixture();
        only_account(&mut state, "opencode");
        state.provider_usage_open = true;
        state.provider_usage.get_mut("opencode").unwrap().status = ProviderUsageStatus::Error;
        let mut tree = popup_tree(&state, 360.0);
        let frame = tree.layout(Size::new(360.0, 720.0));
        assert!(frame_text(&frame).contains("82% usado"));
        assert!(frame_text(&frame).contains("Sin actualizar"));
        state
            .provider_connections
            .get_mut("opencode")
            .unwrap()
            .configured = false;
        tree.set_root(Element::column(vec![
            Element::spacer().flex(1.0),
            sidebar_card(&state, 264.0),
        ]));
        let disconnected = tree.layout(Size::new(360.0, 720.0));
        assert!(!frame_text(&disconnected).contains('%'));
        assert!(tree.focus("provider_usage_connect:opencode"));
    }

    #[test]
    fn sidebar_is_compact_and_uses_selected_accounts_actual_limit() {
        let state = fixture();
        for width in [180.0, 264.0, 340.0] {
            let mut tree = UiTree::new(sidebar_card(&state, width));
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, 160.0));
            let action = frame
                .regions
                .iter()
                .find(|region| region.id == "usage_toggle")
                .unwrap();
            assert_eq!(action.rect.height, 44.0);
            assert!(action.rect.x >= 0.0 && action.rect.x + action.rect.width <= width);
            let painted = frame_text(&frame);
            assert!(painted.contains("OpenCode"), "{painted}");
            // The tightest window, not the first one, and by how much is used.
            assert!(painted.contains("82%") && !painted.contains("18%"), "{painted}");
            assert!(
                !painted.contains("$7.00"),
                "unselected balance leaked into summary: {painted}"
            );
            assert_eq!(
                click(&mut tree, &frame, "usage_toggle"),
                vec![UiAction::Clicked {
                    id: "usage_toggle".into()
                }]
            );
            assert!(tree.focus("usage_toggle"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "usage_toggle".into()
                }]
            );
        }
    }

    #[test]
    fn sidebar_usage_hover_has_visible_surface_feedback_and_keeps_its_hitbox() {
        use std::time::{Duration, Instant};
        let state = fixture();
        for compact in [false, true] {
            let viewport = Size::new(264.0, 150.0);
            let root = if compact {
                sidebar_compact(&state)
            } else {
                sidebar_card(&state, viewport.width)
            };
            let mut tree = native_tree(root);
            tree.set_reduced_motion(false);
            let now = Instant::now();
            let initial = tree.layout_at(viewport, now);
            let rect = initial
                .regions
                .iter()
                .find(|region| {
                    region.id == "usage_toggle" && region.kind == openide_ui::HitKind::Button
                })
                .unwrap()
                .rect;
            let fill = |frame: &Frame| {
                frame.commands.iter().find_map(|command| match command {
                    DrawCommand::FillRect {
                        rect: painted,
                        color,
                        ..
                    } if *painted == rect => Some(*color),
                    _ => None,
                })
            };
            let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            tree.handle_event_at(UiEvent::PointerMove(point), now);
            tree.layout_at(viewport, now);
            let hovered = tree.layout_at(viewport, now + Duration::from_millis(160));
            assert_eq!(hovered.hit_test(point), Some("usage_toggle"));
            assert_ne!(
                fill(&initial),
                fill(&hovered),
                "hover must remain visible over the sidebar, compact={compact}"
            );
            assert_eq!(fill(&hovered), Some(theme::hover_for(theme::SIDEBAR)));
            let hovered_rect = hovered
                .regions
                .iter()
                .find(|region| {
                    region.id == "usage_toggle" && region.kind == openide_ui::HitKind::Button
                })
                .unwrap()
                .rect;
            assert_eq!(rect, hovered_rect);
            tree.handle_event_at(UiEvent::PointerLeave, now + Duration::from_millis(160));
            tree.layout_at(viewport, now + Duration::from_millis(160));
            let restored = tree.layout_at(viewport, now + Duration::from_millis(320));
            assert_eq!(fill(&restored), fill(&initial));
        }
    }

    #[test]
    fn external_accounts_and_unknown_limits_never_draw_a_percentage() {
        let mut state = fixture();
        for id in ["codex", "nvidia"] {
            state.provider_connections.get_mut(id).unwrap().configured = true;
            state.provider_usage.get_mut(id).unwrap().status = ProviderUsageStatus::External;
            let mut tree = UiTree::new(provider_card(&state, id, 340.0).unwrap());
            let frame = tree.layout(Size::new(340.0, 600.0));
            let painted = frame_text(&frame);
            assert!(
                !painted.contains('%'),
                "external account fabricated percentage: {painted}"
            );
            assert!(painted.contains("se consulta en tu cuenta"));
            assert!(
                frame
                    .regions
                    .iter()
                    .any(|region| region.id == format!("provider_usage_account:{id}"))
            );
        }
        let account = state.provider_usage.get_mut("openrouter").unwrap();
        account.balances.clear();
        account.windows[0].limit = None;
        account.windows[0].remaining = None;
        let mut tree = UiTree::new(provider_card(&state, "openrouter", 340.0).unwrap());
        let frame = tree.layout(Size::new(340.0, 600.0));
        assert!(!frame_text(&frame).contains('%'));
        assert!(frame_text(&frame).contains("$3.00"));
    }

    #[test]
    fn errors_keep_the_last_measurement_but_disconnect_removes_it() {
        let mut state = fixture();
        let account = state.provider_usage.get_mut("openrouter").unwrap();
        account.status = ProviderUsageStatus::Error;
        account.message = Some("No se pudo actualizar. Se conserva la última medición.".into());
        let mut tree = UiTree::new(provider_card(&state, "openrouter", 360.0).unwrap());
        let frame = tree.layout(Size::new(360.0, 600.0));
        let painted = frame_text(&frame);
        assert!(painted.contains("$7.00"));
        assert!(painted.contains("Último dato"));
        assert!(painted.contains("No se pudo actualizar"));
        state.native_provider_id = "openrouter".into();
        let mut sidebar = UiTree::new(sidebar_card(&state, 264.0));
        let summary = sidebar.layout(Size::new(264.0, 160.0));
        assert!(frame_text(&summary).contains("Sin actualizar"));
        state.provider_usage.get_mut("openrouter").unwrap().status = ProviderUsageStatus::Ready;
        state
            .provider_usage
            .get_mut("openrouter")
            .unwrap()
            .fetched_at_ms = Some(now_ms() - 181_000);
        sidebar.set_root(sidebar_card(&state, 264.0));
        assert!(frame_text(&sidebar.layout(Size::new(264.0, 160.0))).contains("Sin actualizar"));
        state
            .provider_connections
            .get_mut("openrouter")
            .unwrap()
            .configured = false;
        tree.set_root(provider_card(&state, "openrouter", 360.0).unwrap());
        let disconnected = tree.layout(Size::new(360.0, 600.0));
        assert!(!frame_text(&disconnected).contains('$'));
        assert!(!frame_text(&disconnected).contains("Último dato"));
        assert!(tree.focus("provider_usage_connect:openrouter"));
        assert!(!tree.focus("provider_usage_account:openrouter"));
    }

    #[test]
    fn popup_fits_the_viewport_and_keeps_actions_and_all_accounts_reachable() {
        let mut state = fixture();
        state.provider_usage_open = true;
        state
            .provider_connections
            .get_mut("nvidia")
            .unwrap()
            .configured = true;
        state.provider_usage.get_mut("nvidia").unwrap().status = ProviderUsageStatus::External;
        for width in [280.0, 360.0, 900.0] {
            let mut tree = popup_tree(&state, width);
            let frame = tree.layout(Size::new(width, 720.0));
            let surface = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:usage_dismiss")
                .unwrap()
                .rect;
            assert!(
                frame_text(&frame).contains("OpenCode"),
                "selected account is not visible first"
            );
            assert!(
                surface.x >= 0.0 && surface.x + surface.width <= width,
                "{surface:?}"
            );
            assert!(
                surface.y >= 0.0 && surface.y + surface.height <= 720.0,
                "{surface:?}"
            );
            for action in ["usage_refresh", "usage_details"] {
                assert!(tree.focus(action));
                assert_eq!(
                    click(&mut tree, &frame, action),
                    vec![UiAction::Clicked { id: action.into() }]
                );
            }
            let mut observed = frame_text(&frame);
            for _ in 0..12 {
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(
                        surface.x + surface.width / 2.0,
                        surface.y + surface.height / 2.0,
                    ),
                    delta_y: -100.0,
                });
                let scrolled = tree.layout(Size::new(width, 720.0));
                observed.push_str(&frame_text(&scrolled));
            }
            assert!(observed.contains("Codex"));
            assert!(observed.contains("OpenRouter"));
            assert!(observed.contains("OpenCode"));
            assert!(observed.contains("NVIDIA NIM"));
            assert!(tree.focus("provider_usage_account:nvidia"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "provider_usage_account:nvidia".into()
                }]
            );
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Escape,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "usage_dismiss".into()
                }]
            );
        }
    }

    #[test]
    fn short_windows_shrink_the_scroll_body_and_preserve_the_fixed_footer() {
        let mut state = fixture();
        state.provider_usage_open = true;
        for (width, height) in [(360.0, 320.0), (360.0, 400.0), (520.0, 320.0)] {
            let mut tree = popup_tree(&state, width);
            let frame = tree.layout(Size::new(width, height));
            let surface = frame
                .regions
                .iter()
                .find(|region| region.id == "popover_surface:usage_dismiss")
                .unwrap()
                .rect;
            let details = frame
                .regions
                .iter()
                .find(|region| region.id == "usage_details")
                .unwrap()
                .rect;
            assert!(
                details.y >= surface.y && details.y + details.height <= surface.y + surface.height,
                "footer {details:?} exceeds {surface:?}"
            );
            assert!(
                details.y + details.height <= height,
                "footer below viewport: {details:?}"
            );
            assert_eq!(
                click(&mut tree, &frame, "usage_details"),
                vec![UiAction::Clicked {
                    id: "usage_details".into()
                }]
            );
            tree.scroll_to_end("provider_usage_popup");
            let scrolled = tree.layout(Size::new(width, height));
            assert!(frame_text(&scrolled).contains("OpenCode"));
            assert!(tree.focus("provider_usage_account:opencode"));
        }
    }

    #[test]
    fn compact_usage_trigger_retains_pointer_keyboard_and_popover_actions() {
        let mut state = fixture();
        let mut tree = native_tree(sidebar_compact(&state));
        let frame = tree.layout(Size::new(48.0, 60.0));
        let rect = frame
            .regions
            .iter()
            .find(|region| region.id == "usage_toggle")
            .unwrap()
            .rect;
        assert_eq!((rect.width, rect.height), (24.0, 24.0));
        assert_eq!(
            click(&mut tree, &frame, "usage_toggle"),
            vec![UiAction::Clicked {
                id: "usage_toggle".into()
            }]
        );
        assert!(tree.focus("usage_toggle"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "usage_toggle".into()
            }]
        );
        state.provider_usage_open = true;
        tree.set_root(Element::column(vec![
            Element::spacer().flex(1.0),
            sidebar_compact(&state),
        ]));
        let open = tree.layout(Size::new(520.0, 320.0));
        let details = open
            .regions
            .iter()
            .find(|region| region.id == "usage_details")
            .unwrap()
            .rect;
        assert!(details.y + details.height <= 320.0);
        assert!(tree.focus("usage_refresh"));
        assert!(tree.focus("usage_details"));
    }

    #[test]
    fn settings_uses_exact_four_account_catalog_and_connect_actions_at_both_widths() {
        let mut state = fixture();
        state
            .provider_usage
            .insert("other".into(), state.provider_usage["openrouter"].clone());
        for width in [320.0, 900.0] {
            let mut tree = UiTree::new(
                ScrollArea::new(
                    "usage_test_page",
                    Element::column(usage_page(&state, width)).gap(16.0),
                )
                .render(),
            );
            tree.set_reduced_motion(true);
            let mut observed = String::new();
            let mut actions = std::collections::BTreeSet::new();
            for _ in 0..24 {
                let frame = tree.layout(Size::new(width, 720.0));
                observed.push_str(&frame_text(&frame));
                for region in &frame.regions {
                    if region.id.starts_with("provider_usage_") {
                        assert!(
                            region.rect.x >= 0.0
                                && region.rect.x + region.rect.width <= width + 0.01,
                            "{}: {:?}",
                            region.id,
                            region.rect
                        );
                        actions.insert(region.id.clone());
                    }
                }
                tree.handle_event(UiEvent::Scroll {
                    position: Point::new(width / 2.0, 360.0),
                    delta_y: -100.0,
                });
            }
            for (_, name, _) in provider_connections::PROVIDERS {
                assert!(observed.contains(name), "missing {name}");
            }
            assert!(actions.contains("provider_usage_connect:nvidia"));
            assert!(actions.contains("provider_usage_account:codex"));
            assert!(actions.contains("provider_usage_account:openrouter"));
            assert!(actions.contains("provider_usage_account:opencode"));
            assert_eq!(actions.len(), provider_connections::PROVIDERS.len());
            assert!(provider_card(&state, "other", width).is_none());
        }
    }

    #[test]
    fn settings_plan_badges_follow_native_text_width_and_keep_actions_separate() {
        let state = fixture();
        let mut account = state.provider_usage["codex"].clone();
        for plan in ["Plan de ChatGPT", "Créditos de API", "OpenCode Go", "API"] {
            account.plan_label = Some(plan.into());
            for width in [280.0, 360.0, 420.0, 640.0] {
                let mut tree = native_tree(account_card(&state, &account, width, now_ms()));
                let frame = tree.layout(Size::new(width, 500.0));
                let blocks = frame.text_layouts();
                let label = blocks
                    .iter()
                    .find(|block| block.source == plan)
                    .unwrap_or_else(|| panic!("missing plan {plan} at {width}px"));
                assert_eq!(
                    label.runs.len(),
                    1,
                    "plan must remain a compact single line"
                );
                assert_eq!(label.runs[0].text, plan);
                let text = label.runs[0].rect;
                let badge = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::StrokeRect {
                            rect,
                            radius,
                            color,
                            ..
                        } if *radius == 10.0
                            && rect.height == 20.0
                            && *color == theme::BORDER
                            && rect.contains(Point::new(
                                text.x + text.width / 2.0,
                                text.y + text.height / 2.0,
                            )) =>
                        {
                            Some(*rect)
                        }
                        _ => None,
                    })
                    .expect("shared passive Chip border");
                assert!(
                    (badge.width - text.width - 16.0).abs() < 0.1,
                    "badge stretched independently of its text: {plan}, {badge:?}, {text:?}"
                );
                assert!(badge.width < 140.0, "old fixed-width pill returned");
                assert!(badge.x >= 10.0 && badge.x + badge.width <= width - 10.0 + 0.1);
                assert!((text.x + text.width / 2.0 - badge.x - badge.width / 2.0).abs() < 0.1);
                assert!((text.y + text.height / 2.0 - badge.y - badge.height / 2.0).abs() < 0.1);
                let point = Point::new(badge.x + badge.width / 2.0, badge.y + badge.height / 2.0);
                assert_eq!(
                    frame.hit_test(point),
                    None,
                    "plan is information, not an action"
                );
                assert!(tree.handle_event(UiEvent::PointerDown(point)).is_empty());
                assert!(tree.handle_event(UiEvent::PointerUp(point)).is_empty());
                let action = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "provider_usage_account:codex")
                    .unwrap();
                assert_eq!(action.rect.height, 28.0);
                assert!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Tab,
                        ctrl: false,
                        shift: false
                    })
                    .is_empty()
                );
                assert_eq!(tree.focused_id(), Some("provider_usage_account:codex"));
            }
        }
        account.plan_label = None;
        let frame = native_tree(account_card(&state, &account, 420.0, now_ms()))
            .layout(Size::new(420.0, 500.0));
        assert!(!frame.commands.iter().any(|command| matches!(command,
            DrawCommand::StrokeRect { rect, radius, .. } if *radius == 10.0 && rect.height == 20.0
        )), "missing plan must not reserve a badge");
    }

    #[test]
    fn plan_badge_stays_passive_without_hover_or_keyboard_state() {
        let mut tree = native_tree(Element::row(vec![
            plan_badge("Plan de ChatGPT"),
            Element::spacer().flex(1.0),
        ]));
        let size = Size::new(280.0, 32.0);
        let before = tree.layout(size);
        assert!(before.regions.is_empty());
        for event in [
            UiEvent::PointerMove(Point::new(20.0, 10.0)),
            UiEvent::PointerDown(Point::new(20.0, 10.0)),
            UiEvent::PointerUp(Point::new(20.0, 10.0)),
            UiEvent::Key {
                key: Key::Tab,
                shift: false,
                ctrl: false,
            },
            UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false,
            },
        ] {
            assert!(tree.handle_event(event).is_empty());
        }
        assert_eq!(tree.focused_id(), None);
        let after = tree.layout(size);
        assert!(after.regions.is_empty());
        assert_eq!(frame_text(&before), frame_text(&after));
        assert!(!tree.is_animating());
    }

    #[test]
    fn settings_account_actions_keep_equal_size_and_row_baselines() {
        let state = fixture();
        for width in [280.0, 360.0, 700.0, 900.0, 1280.0] {
            let mut tree = native_tree(Element::column(usage_page(&state, width)).gap(16.0));
            let frame = tree.layout(Size::new(width, 2400.0));
            let actions: Vec<_> = frame
                .regions
                .iter()
                .filter(|region| {
                    region.kind == openide_ui::HitKind::Button
                        && (region.id.starts_with("provider_usage_account:")
                            || region.id.starts_with("provider_usage_connect:"))
                })
                .collect();
            assert_eq!(actions.len(), provider_connections::PROVIDERS.len());
            for action in &actions {
                // Pixel snapping at a fractional offset is float noise.
                assert!(
                    (action.rect.height - 28.0).abs() < 0.01,
                    "{width}px {}: {:?}",
                    action.id,
                    action.rect
                );
                assert!(
                    action.rect.x >= 10.0
                        && action.rect.x + action.rect.width <= width - 10.0 + 0.01
                );
                assert!(
                    click(&mut tree, &frame, &action.id)
                        .iter()
                        .any(|event| matches!(event, UiAction::Clicked { id } if id == &action.id))
                );
                assert!(tree.focus(&action.id));
                assert!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    })
                    .iter()
                    .any(|event| matches!(event, UiAction::Clicked { id } if id == &action.id))
                );
            }
            if width >= 700.0 {
                let cards: Vec<_> = frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::StrokeRect { rect, radius, .. }
                            if *radius == 8.0
                                && actions.iter().any(|action| {
                                    rect.width > action.rect.width
                                        && rect.height > action.rect.height
                                        && rect.contains(Point::new(action.rect.x, action.rect.y))
                                        && rect.contains(Point::new(
                                            action.rect.x + action.rect.width,
                                            action.rect.y + action.rect.height,
                                        ))
                                }) =>
                        {
                            Some(rect)
                        }
                        _ => None,
                    })
                    .collect();
                // Each account with something to say has its card; the ones
                // never connected share one wider list below them.
                let unconnected = provider_connections::PROVIDERS
                    .iter()
                    .filter(|(id, name, _)| snapshot(&state, id, name).status == ProviderUsageStatus::Disconnected)
                    .count();
                assert!(unconnected > 0);
                assert_eq!(cards.len(), provider_connections::PROVIDERS.len() - unconnected + 1);
                let narrowest = cards.iter().map(|card| card.width).fold(f32::MAX, f32::min);
                let cards: Vec<_> = cards.into_iter().filter(|card| card.width == narrowest).collect();
                assert_eq!(cards.len(), provider_connections::PROVIDERS.len() - unconnected);
                // Two independent columns: equal widths, and a card keeps its
                // own height instead of stretching to a taller neighbour.
                assert!(cards.iter().all(|card| card.width == cards[0].width));
                let mut columns: Vec<f32> = cards.iter().map(|card| card.x).collect();
                columns.dedup_by(|a, b| a == b);
                columns.sort_by(f32::total_cmp);
                columns.dedup();
                assert_eq!(columns.len(), 2);
                let tallest = cards.iter().map(|card| card.height).fold(0.0, f32::max);
                assert!(cards.iter().any(|card| card.height < tallest), "no card is stretched to the tallest");
            }
            let painted = frame_text(&frame);
            assert!(painted.contains("Abrir cuenta") && painted.contains("Conectar"));
        }
    }

    #[test]
    fn account_action_hover_preserves_geometry_and_label_for_every_state() {
        use std::time::{Duration, Instant};
        let state = fixture();
        let mut account = state.provider_usage["codex"].clone();
        for (status, url, label) in [
            (ProviderUsageStatus::Disconnected, false, "Conectar"),
            (ProviderUsageStatus::NeedsSignIn, false, "Reconectar"),
            (ProviderUsageStatus::External, true, "Abrir cuenta"),
            (ProviderUsageStatus::Ready, false, "Ver conexión"),
        ] {
            account.status = status;
            account.account_url = url.then(|| "https://example.com/account".into());
            let mut tree = native_tree(account_action(&account));
            let now = Instant::now();
            let viewport = Size::new(136.0, 28.0);
            let before = tree.layout_at(viewport, now);
            let rect = before.regions[0].rect;
            let point = Point::new(rect.x + 40.0, rect.y + 18.0);
            tree.handle_event_at(UiEvent::PointerMove(point), now);
            tree.layout_at(viewport, now);
            let hovered = tree.layout_at(viewport, now + Duration::from_millis(200));
            assert_eq!(rect, hovered.regions[0].rect);
            assert!(
                frame_text(&hovered).contains(label),
                "full action label: {label}"
            );
            assert!(
                hovered.commands.iter().any(|command| matches!(command,
                DrawCommand::Icon { rect: icon_rect, .. }
                    if icon_rect.width == 16.0 && icon_rect.height == 16.0)),
                "the action icon must remain legible: {label}"
            );
            assert!(hovered.commands.iter().any(|command| matches!(command,
                DrawCommand::FillRect { rect: painted, color, .. }
                    if *painted == rect && *color == crate::settings_ui::control_palette().hover)));
        }
    }

    #[test]
    fn reset_and_refresh_times_have_clear_elapsed_and_expired_states() {
        assert_eq!(elapsed_label(1_000, 30_000), "ahora");
        assert_eq!(elapsed_label(1_000, 121_000), "hace 2 min");
        assert_eq!(elapsed_label(1_000, 7_201_000), "hace 2 h");
        let state = fixture();
        let mut window = state.provider_usage["opencode"].windows[0].clone();
        window.reset_at_ms = Some(5_401_000);
        assert_eq!(
            reset_label(&window, 1_000),
            Some("Se renueva en 1 h 30 min".into())
        );
        assert_eq!(
            reset_label(&window, 5_401_000),
            Some("Pendiente de actualización".into())
        );
    }

    #[test]
    #[ignore = "exports native account usage frames for visual review"]
    fn export_provider_usage_visual_review() {
        let output = std::path::PathBuf::from(format!(
            "{}/../../target/provider-usage-visual-qa",
            env!("CARGO_MANIFEST_DIR")
        ));
        std::fs::create_dir_all(&output).unwrap();
        let mut state = fixture();
        let export = |tree: &mut UiTree, name: &str, width: f32, height: f32| {
            tree.set_reduced_motion(true);
            let frame = tree.layout(Size::new(width, height));
            let path = output.join(format!("{name}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, width, height)
                .unwrap();
            let svg = std::fs::read_to_string(&path).unwrap();
            let options = crate::settings_ui::visual_qa::native_svg_options();
            let parsed = resvg::usvg::Tree::from_str(&svg, &options).unwrap();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
            resvg::render(
                &parsed,
                resvg::tiny_skia::Transform::identity(),
                &mut pixmap.as_mut(),
            );
            pixmap.save_png(path.with_extension("png")).unwrap();
            eprintln!("{}", path.with_extension("png").display());
        };
        for width in [360.0, 900.0] {
            let root = Element::column(vec![
                Text::new("Cuenta y uso · datos de prueba", theme::component_palette())
                    .role(TextRole::Body)
                    .ink(theme::SETTINGS_FOREGROUND_SUBTLE)
                    .render(),
                Element::column(usage_page(&state, width - 40.0)).gap(16.0),
            ])
            .gap(24.0)
            .padding(20.0)
            .background(theme::BACKGROUND);
            export(
                &mut native_tree(root),
                &format!("usage-settings-{width:.0}"),
                width,
                if width >= 700.0 { 1_000.0 } else { 1_750.0 },
            );
        }
        state.provider_usage_open = true;
        for width in [360.0, 900.0] {
            export(
                &mut popup_tree(&state, width),
                &format!("usage-popover-{width:.0}"),
                width,
                720.0,
            );
        }
        let mut single = fixture();
        only_account(&mut single, "codex");
        single.provider_usage_open = true;
        single.provider_usage.get_mut("codex").unwrap().account =
            Some("cuenta.de.ejemplo.con.nombre.largo@correo.example".into());
        for width in [280.0, 360.0, 900.0] {
            export(
                &mut popup_tree(&single, width),
                &format!("usage-popover-single-codex-{width:.0}"),
                width,
                520.0,
            );
        }
        for (label, status) in [
            ("loading", ProviderUsageStatus::Loading),
            ("error", ProviderUsageStatus::Error),
            ("reconnect", ProviderUsageStatus::NeedsSignIn),
            ("disconnected", ProviderUsageStatus::Disconnected),
        ] {
            single
                .provider_connections
                .get_mut("codex")
                .unwrap()
                .configured = status != ProviderUsageStatus::Disconnected;
            single
                .provider_connections
                .get_mut("codex")
                .unwrap()
                .needs_sign_in = status == ProviderUsageStatus::NeedsSignIn;
            single.provider_usage.get_mut("codex").unwrap().status = status;
            export(
                &mut popup_tree(&single, 360.0),
                &format!("usage-popover-{label}"),
                360.0,
                520.0,
            );
        }
        // Codex as read from its local sessions.
        for (name, windows) in [
            ("58", &[("Semanal", 58.0, 72)][..]),
            ("85", &[("Semanal", 85.0, 72)][..]),
            ("both", &[("5 horas", 30.0, 2), ("Semanal", 85.0, 72)][..]),
        ] {
            let mut local = codex_local(windows, 72_000_000);
            export(
                &mut popup_tree(&local, 264.0),
                &format!("usage-bars-sidebar-{name}"),
                264.0,
                80.0,
            );
            let card = Element::column(vec![provider_card(&local, "codex", 420.0).unwrap()])
                .padding(20.0)
                .background(theme::BACKGROUND);
            export(
                &mut native_tree(card),
                &format!("usage-bars-settings-{name}"),
                460.0,
                360.0,
            );
            local.provider_usage_open = true;
            export(
                &mut popup_tree(&local, 360.0),
                &format!("usage-bars-popover-{name}"),
                360.0,
                360.0,
            );
        }
        // Claude Code's subscription: offered, waiting for a turn, and measured.
        let claude_windows = [("5 horas", 30.0, 2), ("Semanal", 85.0, 72)];
        for (name, status, windows) in [
            ("off", claude_limits::Status::Off, &[][..]),
            ("waiting", claude_limits::Status::On, &[][..]),
            ("foreign", claude_limits::Status::Foreign, &[][..]),
            ("both", claude_limits::Status::On, &claude_windows[..]),
        ] {
            let mut claude = with_claude(fixture(), windows);
            claude.claude_limits = status;
            let card = Element::column(vec![claude_card(&claude, 420.0, now_ms()).unwrap()])
                .padding(20.0)
                .background(theme::BACKGROUND);
            export(&mut native_tree(card), &format!("claude-settings-{name}"), 460.0, 360.0);
        }
        let mut together = with_claude(
            codex_local(&[("5 horas", 30.0, 2), ("Semanal", 58.0, 72)], 72_000_000),
            &claude_windows,
        );
        together.selected_backend = "cli:claude".into();
        export(&mut popup_tree(&together, 264.0), "claude-sidebar", 264.0, 80.0);
        together.provider_usage_open = true;
        export(&mut popup_tree(&together, 360.0), "claude-popover-with-codex", 360.0, 520.0);
        let page = Element::column(usage_page(&together, 860.0))
            .gap(16.0)
            .padding(20.0)
            .background(theme::BACKGROUND);
        export(&mut native_tree(page), "claude-settings-page", 900.0, 1_100.0);
        state.provider_usage_open = false;
        export(
            &mut popup_tree(&state, 264.0),
            "usage-sidebar",
            264.0,
            150.0,
        );
        let mut hover = popup_tree(&state, 264.0);
        let frame = hover.layout(Size::new(264.0, 150.0));
        hover.handle_event(UiEvent::PointerMove(point_for(&frame, "usage_toggle")));
        // Native exports deliberately use reduced motion, so this layout
        // settles the actual pointer hover before its SVG/PNG is captured.
        hover.layout(Size::new(264.0, 150.0));
        export(&mut hover, "usage-sidebar-hover", 264.0, 150.0);
    }
}

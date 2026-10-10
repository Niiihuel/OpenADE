//! Native charts using account contributions and the available task history.

use std::collections::BTreeMap;

use chrono::Datelike;
use openide_ui::{
    Alert, BarChart, Button, ButtonContent, ButtonSize, ButtonVariant, ChartAxisLabel, ChartDatum,
    ChartLegend, Color, Component, ControlSize, Element, FontFamily, HeatmapCell, HeatmapChart,
    HeatmapScale, Icon, Skeleton, TooltipPlacement,
};

use crate::{
    github_contributions::{GithubContributionCalendar, GithubContributionsStatus},
    settings_parts, theme,
    view::{SessionView, ViewState},
};

const MONTHS: [&str; 12] = [
    "Ene", "Feb", "Mar", "Abr", "May", "Jun", "Jul", "Ago", "Sep", "Oct", "Nov", "Dic",
];
const TASK_BAR_COLOR: Color = Color::rgb(107, 147, 218);

pub(super) fn task_count_label(count: usize) -> String {
    format!("{count} {}", if count == 1 { "tarea" } else { "tareas" })
}

pub(super) fn execution_count_label(count: usize) -> String {
    format!(
        "{count} {}",
        if count == 1 {
            "ejecución"
        } else {
            "ejecuciones"
        }
    )
}
fn text(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .foreground(color)
        .font_family(FontFamily::Sans)
        .font_size(size)
}

fn contribution_scale() -> HeatmapScale {
    HeatmapScale::new([
        theme::PANEL,
        Color::rgb(14, 68, 41),
        Color::rgb(0, 109, 50),
        Color::rgb(38, 166, 65),
        Color::rgb(57, 211, 83),
    ])
}

fn calendar_chart(calendar: &GithubContributionCalendar, width: f32) -> Element {
    let mut months = Vec::new();
    let mut previous_month = None;
    let columns = calendar
        .weeks
        .iter()
        .enumerate()
        .map(|(column, week)| {
            week.iter()
                .map(|day| {
                    day.map(|day| {
                        let month = (day.date.year(), day.date.month0());
                        if previous_month != Some(month) {
                            let tick =
                                ChartAxisLabel::new(column, MONTHS[day.date.month0() as usize]);
                            if months
                                .last()
                                .is_some_and(|previous: &ChartAxisLabel| previous.column == column)
                            {
                                *months.last_mut().unwrap() = tick;
                            } else {
                                months.push(tick);
                            }
                            previous_month = Some(month);
                        }
                        // Keep the measured count and full date legible in
                        // the compact native tooltip at narrow viewports.
                        let count = format!("{} contrib.", day.count);
                        HeatmapCell::new(
                            day.date.to_string(),
                            day.level,
                            format!("{count} · {}", day.date.format("%d/%m/%Y")),
                        )
                    })
                })
                .collect()
        })
        .collect();
    HeatmapChart::new("github-contributions", columns, theme::component_palette())
        .width(width)
        .row_labels(["", "Lun", "", "Mié", "", "Vie", ""])
        .column_labels(months)
        .scale(contribution_scale())
        .render()
}

pub(super) fn github_card(state: &ViewState, content_width: f32) -> Element {
    let width = (content_width - 20.0).max(0.0);
    let palette = theme::component_palette();
    match &state.github_contributions {
        GithubContributionsStatus::Ready(calendar) => {
            let refresh = Button::with_content(
                "github_contributions_refresh",
                ButtonContent::Icon(Icon::Refresh),
                theme::action_palette(theme::CARD, theme::FOREGROUND),
            )
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::IconExtraSmall)
            .tooltip("Actualizar contribuciones")
            .tooltip_placement(TooltipPlacement::Below)
            .render();
            let title = match calendar.total {
                1 => "1 contribución en el último año".to_owned(),
                count => format!("{count} contribuciones en el último año"),
            };
            let header = Element::row(vec![
                text(title, theme::FOREGROUND, 12.0).flex(1.0),
                refresh,
            ])
            .gap(6.0);
            let help = Button::new(
                "github_contributions_help",
                "Cómo se cuentan las contribuciones",
                palette,
            )
            .variant(ButtonVariant::Link)
            .size(ControlSize::Dense)
            .render();
            let legend =
                ChartLegend::new(contribution_scale().colors, "Menos", "Más", palette).render();
            let footer = if width < 560.0 {
                Element::column(vec![help, legend]).gap(6.0)
            } else {
                Element::row(vec![help, Element::spacer().flex(1.0), legend]).gap(6.0)
            };
            settings_parts::settings_card(vec![Element::column(vec![
                header,
                text(
                    format!("@{}", calendar.login),
                    theme::SETTINGS_FOREGROUND_SUBTLE,
                    11.0,
                ),
                calendar_chart(calendar, width),
                footer,
            ])
            .gap(10.0)
            .padding(10.0)])
        }
        GithubContributionsStatus::Idle | GithubContributionsStatus::Loading => {
            settings_parts::settings_card(vec![Element::column(vec![
                text("Cargando contribuciones…", theme::FOREGROUND, 12.0),
                Skeleton::new("github-contributions-loading", palette)
                    .height(110.0)
                    .radius(4.0)
                    .render(),
            ])
            .gap(10.0)
            .padding(10.0)])
        }
        GithubContributionsStatus::Unavailable(error) => Alert::new(
            "No se pudieron cargar las contribuciones",
            error,
            theme::component_palette(),
        )
        .tone(settings_parts::FeedbackTone::Error)
        .compact(true)
        .action(
            Element::row(vec![
                super::outline("github_contributions_refresh", "Reintentar").render(),
                super::outline("github_account_connect", "Reconectar GitHub").render(),
            ])
            .gap(6.0),
        )
        .render(),
    }
}

fn task_data(sessions: &[SessionView], backend_options: &[(String, String)]) -> Vec<ChartDatum> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for session in sessions {
        *counts.entry(&session.backend).or_default() += 1;
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_by(|(left, left_count), (right, right_count)| {
        right_count.cmp(left_count).then(left.cmp(right))
    });
    counts
        .into_iter()
        .map(|(backend, count)| {
            let name = backend_options
                .iter()
                .find(|(id, _)| id == backend)
                .map(|(_, name)| name.as_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(if backend.trim().is_empty() {
                    "Sin agente registrado"
                } else {
                    backend
                });
            ChartDatum::new(backend, name, count as f64).tooltip(format!(
                "{name} · {} en el historial visible",
                task_count_label(count),
            ))
        })
        .collect()
}

pub(super) fn tasks_card(state: &ViewState, content_width: f32) -> Element {
    let width = (content_width - 20.0).max(0.0);
    let total = state.sessions.len();
    let data = task_data(&state.sessions, &state.backend_options);
    let agent_count = data.len();
    let title = text("Tareas por agente", theme::FOREGROUND, 12.0);
    let total = text(
        format!(
            "{} {}",
            task_count_label(total),
            if total == 1 { "visible" } else { "visibles" }
        ),
        theme::SETTINGS_FOREGROUND_SUBTLE,
        11.0,
    );
    let header = if width < 400.0 {
        Element::column(vec![title, total]).gap(4.0)
    } else {
        Element::row(vec![title.flex(1.0), total]).gap(10.0)
    };
    let mut rows = vec![
        header,
        text(
            "Historial visible en este equipo, agrupado por agente.",
            theme::SETTINGS_FOREGROUND_SUBTLE,
            11.0,
        ),
    ];
    if data.is_empty() {
        let mut palette = theme::component_palette();
        palette.muted = theme::SETTINGS_FOREGROUND_SUBTLE;
        rows.push(
            openide_ui::Empty::new(
                "No hay tareas en esta vista",
                "Las tareas guardadas aparecerán aquí según el filtro actual del historial.",
                palette,
            )
            .icon(openide_ui::Icon::Clock)
            .compact()
            .render(),
        );
    } else {
        // Category/value axes and restrained column widths follow the bar
        // chart model documented by ECharts, using our native element tree.
        // https://echarts.apache.org/handbook/en/how-to/chart-types/bar/basic-bar/
        // No minimum bar height: every painted height reflects its count.
        rows.push(
            BarChart::new(
                "usage-tasks",
                data,
                openide_ui::ComponentPalette {
                    surface: theme::CARD,
                    border: theme::TREE_GUIDE,
                    muted: theme::SETTINGS_FOREGROUND_SUBTLE,
                    ..theme::component_palette()
                },
            )
            .width(width)
            .color(TASK_BAR_COLOR)
            .vertical()
            .height(if width < 400.0 { 220.0 } else { 240.0 })
            .max_bar_width(48.0)
            .integer_ticks(true)
            .render(),
        );
        rows.push(text(
            format!(
                "{agent_count} {} · Ordenadas por cantidad",
                if agent_count == 1 {
                    "agente"
                } else {
                    "agentes"
                }
            ),
            theme::SETTINGS_FOREGROUND_SUBTLE,
            11.0,
        ));
    }
    settings_parts::settings_card(vec![Element::column(rows).gap(10.0).padding(10.0)])
}

#[cfg(test)]
#[path = "activity_charts_tests.rs"]
mod task_chart_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_contributions::GithubContributionDay;
    use openide_ui::{DrawCommand, HitKind, Point, Size, UiEvent, UiTree};
    use std::time::{Duration, Instant};

    #[test]
    fn native_calendar_handles_leap_day_and_partial_weeks_at_multiple_widths() {
        let first = chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        let mut weeks = vec![[None; 7]];
        for offset in 0..366 {
            let date = first + chrono::Duration::days(offset);
            let row = date.weekday().num_days_from_sunday() as usize;
            if row == 0 {
                weeks.push([None; 7]);
            }
            weeks.last_mut().unwrap()[row] = Some(GithubContributionDay {
                date,
                count: 0,
                level: 0,
            });
        }
        let calendar = GithubContributionCalendar {
            login: "octocat".into(),
            total: 0,
            weeks,
        };
        for width in [260.0, 480.0, 900.0] {
            let mut tree = UiTree::new(calendar_chart(&calendar, width));
            let now = Instant::now();
            let frame = tree.layout_at(Size::new(width, 250.0), now);
            assert_eq!(frame.regions.len(), 366);
            assert!(frame
                .regions
                .iter()
                .all(|region| region.kind == HitKind::Hover
                    && region.rect.x + region.rect.width <= width + 0.01));
            let first_month = frame.commands.iter().find_map(|command| match command {
                DrawCommand::Text { text, .. } if MONTHS.contains(&text.as_str()) => {
                    Some(text.as_str())
                }
                _ => None,
            });
            // February 29 and March 1 share the partial first week. Its tick
            // names the new month, including at widths that cull later ticks.
            assert_eq!(first_month, Some("Mar"), "first month at {width}px");
            let leap_day = frame
                .regions
                .iter()
                .find(|region| region.id == "github-contributions:2024-02-29")
                .expect("leap-day cell");
            tree.handle_event(UiEvent::PointerMove(Point::new(
                leap_day.rect.x + leap_day.rect.width / 2.0,
                leap_day.rect.y + leap_day.rect.height / 2.0,
            )));
            let tooltip_frame =
                tree.layout_at(Size::new(width, 250.0), now + Duration::from_secs(2));
            let tooltip_text = tooltip_frame
                .commands
                .iter()
                .filter_map(|command| match command {
                    DrawCommand::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            let tooltip_text = tooltip_text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                tooltip_text.contains("0 contrib. · 29/02/2024"),
                "leap-day tooltip at {width}px: {tooltip_text}"
            );
            assert!(
                !tooltip_text.contains('…'),
                "leap-day tooltip must not truncate its date at {width}px: {tooltip_text}"
            );
        }
    }

    #[test]
    fn task_bars_count_available_sessions_and_use_agent_names() {
        let sessions = ["native", "codex", "native"]
            .into_iter()
            .enumerate()
            .map(|(index, backend)| SessionView {
                id: index.to_string(),
                title: "Task".into(),
                project: "/project".into(),
                backend: backend.into(),
                native_automation: None,
                pinned: false,
                archived: false,
                unread: false,
            })
            .collect::<Vec<_>>();
        let data = task_data(&sessions, &[("native".into(), "Agente Rust".into())]);
        assert_eq!(data.len(), 2);
        assert_eq!(data[0].label, "Agente Rust");
        assert_eq!(data[0].value, 2.0);
        assert_eq!(data[1].label, "codex");
        assert_eq!(data[1].value, 1.0);
        assert!(task_data(&[], &[]).is_empty());
    }
}

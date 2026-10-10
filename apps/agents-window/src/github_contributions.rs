//! The connected account's last year of contributions, fetched off the UI
//! thread. Credentials stay in the worker; only validated calendar data returns.

use std::{
    io::{self, Read},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use chrono::{Datelike, NaiveDate};
use serde::Deserialize;

use crate::github_account::auth_token_for_account;

const GRAPHQL_URL: &str = "https://api.github.com/graphql";
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const INVALID_CALENDAR: &str = "GitHub devolvió un calendario de contribuciones no válido";
// GitHub defaults this collection to one year ago through the current time.
// https://docs.github.com/en/graphql/reference/users#user
const CALENDAR_QUERY: &str = r#"
query OpenIDEContributions {
  viewer {
    login
    contributionsCollection {
      contributionCalendar {
        totalContributions
        weeks {
          contributionDays { date weekday contributionCount contributionLevel }
        }
      }
    }
  }
}
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GithubContributionDay {
    pub date: NaiveDate,
    pub count: u32,
    pub level: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubContributionCalendar {
    pub login: String,
    pub total: u32,
    /// Sunday through Saturday. Missing days fall outside the API's period.
    pub weeks: Vec<[Option<GithubContributionDay>; 7]>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum GithubContributionsStatus {
    #[default]
    Idle,
    Loading,
    Ready(GithubContributionCalendar),
    Unavailable(String),
}

#[derive(Clone, Debug)]
struct Request {
    login: String,
    generation: u64,
}

#[derive(Clone, Debug)]
pub struct GithubContributionsResponse {
    login: String,
    generation: u64,
    pub result: Result<GithubContributionCalendar, String>,
}

pub struct GithubContributionsWorker {
    request_tx: Sender<Request>,
    result_rx: Receiver<GithubContributionsResponse>,
}

impl GithubContributionsWorker {
    pub fn spawn() -> io::Result<Self> {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (result_tx, result_rx) = mpsc::channel();
        thread::Builder::new()
            .name("openide-github-contributions".into())
            .spawn(move || {
                while let Ok(mut request) = request_rx.recv() {
                    while let Ok(next) = request_rx.try_recv() {
                        request = next;
                    }
                    let result = load_calendar(&request.login);
                    if result_tx
                        .send(GithubContributionsResponse {
                            login: request.login,
                            generation: request.generation,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            request_tx,
            result_rx,
        })
    }

    pub fn request(&self, login: &str, generation: u64) -> bool {
        self.request_tx
            .send(Request {
                login: login.to_owned(),
                generation,
            })
            .is_ok()
    }

    pub fn poll_latest(&self, login: &str, generation: u64) -> Option<GithubContributionsResponse> {
        let mut latest = None;
        while let Ok(response) = self.result_rx.try_recv() {
            if response.generation == generation && response.login.eq_ignore_ascii_case(login) {
                latest = Some(response);
            }
        }
        latest
    }
}

fn load_calendar(login: &str) -> Result<GithubContributionCalendar, String> {
    let token = auth_token_for_account(login)
        .ok_or_else(|| "Reconecta esta cuenta para consultar sus contribuciones.".to_owned())?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|_| "No se pudo preparar la consulta de contribuciones".to_owned())?;
    let response = client
        .post(GRAPHQL_URL)
        .header(reqwest::header::USER_AGENT, "OpenIDE-Agent-Window")
        .bearer_auth(&token)
        .json(&serde_json::json!({ "query": CALENDAR_QUERY }))
        .send()
        .map_err(|_| "No se pudieron consultar las contribuciones de GitHub".to_owned())?;
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 => "La autorización de GitHub expiró. Reconecta tu cuenta.".to_owned(),
            403 | 429 => {
                "GitHub no permitió la consulta o se agotó el límite de solicitudes.".to_owned()
            }
            status => format!("GitHub respondió con HTTP {status} al consultar contribuciones"),
        });
    }
    let mut bytes = Vec::new();
    response
        .take((MAX_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "No se pudo leer el calendario de contribuciones".to_owned())?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err("El calendario de GitHub excede el límite de tamaño".to_owned());
    }
    parse_calendar(&bytes, login)
}

#[derive(Deserialize)]
struct ApiResponse {
    data: Option<ApiData>,
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct ApiData {
    viewer: ApiViewer,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiViewer {
    login: String,
    contributions_collection: ApiCollection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiCollection {
    contribution_calendar: ApiCalendar,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiCalendar {
    total_contributions: u32,
    weeks: Vec<ApiWeek>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiWeek {
    contribution_days: Vec<ApiDay>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiDay {
    date: NaiveDate,
    weekday: usize,
    contribution_count: u32,
    contribution_level: ApiLevel,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ApiLevel {
    None,
    FirstQuartile,
    SecondQuartile,
    ThirdQuartile,
    FourthQuartile,
}

impl ApiLevel {
    fn intensity(self) -> u8 {
        match self {
            Self::None => 0,
            Self::FirstQuartile => 1,
            Self::SecondQuartile => 2,
            Self::ThirdQuartile => 3,
            Self::FourthQuartile => 4,
        }
    }
}

fn parse_calendar(bytes: &[u8], login: &str) -> Result<GithubContributionCalendar, String> {
    let response: ApiResponse =
        serde_json::from_slice(bytes).map_err(|_| INVALID_CALENDAR.to_owned())?;
    if response.errors.is_some_and(|errors| !errors.is_empty()) {
        // GraphQL can return HTTP 200 with errors. Never display partial data
        // or repeat server-provided messages that could contain sensitive data.
        return Err(
            "GitHub no pudo entregar el calendario. Comprueba el acceso de tu cuenta.".to_owned(),
        );
    }
    let viewer = response.data.ok_or(INVALID_CALENDAR)?.viewer;
    if !viewer.login.eq_ignore_ascii_case(login) {
        return Err(
            "La credencial disponible pertenece a otra cuenta. Reconecta la cuenta seleccionada."
                .to_owned(),
        );
    }
    let calendar = viewer.contributions_collection.contribution_calendar;
    if calendar.weeks.is_empty() || calendar.weeks.len() > 54 {
        return Err(INVALID_CALENDAR.to_owned());
    }
    let mut weeks = Vec::with_capacity(calendar.weeks.len());
    let mut previous_date: Option<NaiveDate> = None;
    let mut previous_week: Option<NaiveDate> = None;
    let mut first_date = None;
    for week in calendar.weeks {
        if week.contribution_days.is_empty() || week.contribution_days.len() > 7 {
            return Err(INVALID_CALENDAR.to_owned());
        }
        let mut days = [None; 7];
        let mut week_start = None;
        for day in week.contribution_days {
            if day.weekday >= 7
                || day.date.weekday().num_days_from_sunday() as usize != day.weekday
                || previous_date.is_some_and(|previous| previous.succ_opt() != Some(day.date))
            {
                return Err(INVALID_CALENDAR.to_owned());
            }
            let sunday = day
                .date
                .checked_sub_signed(chrono::Duration::days(day.weekday as i64))
                .ok_or(INVALID_CALENDAR)?;
            if week_start.is_some_and(|start| start != sunday) {
                return Err(INVALID_CALENDAR.to_owned());
            }
            week_start = Some(sunday);
            first_date.get_or_insert(day.date);
            previous_date = Some(day.date);
            days[day.weekday] = Some(GithubContributionDay {
                date: day.date,
                count: day.contribution_count,
                level: day.contribution_level.intensity(),
            });
        }
        if previous_week.is_some_and(|previous| {
            previous.checked_add_signed(chrono::Duration::days(7)) != week_start
        }) {
            return Err(INVALID_CALENDAR.to_owned());
        }
        previous_week = week_start;
        weeks.push(days);
    }
    if first_date
        .zip(previous_date)
        // GitHub aligns the first calendar week to Sunday, which can add up
        // to six days before the collection's one-year (possibly leap) period.
        .is_none_or(|(first, last)| (last - first).num_days() > 366 + 6)
    {
        return Err(INVALID_CALENDAR.to_owned());
    }
    Ok(GithubContributionCalendar {
        login: viewer.login,
        total: calendar.total_contributions,
        weeks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn fixture() -> Value {
        json!({ "data": { "viewer": {
            "login": "octocat",
            "contributionsCollection": { "contributionCalendar": {
                "totalContributions": 8,
                "weeks": [
                    { "contributionDays": [{ "date": "2026-09-05", "weekday": 6,
                        "contributionCount": 0, "contributionLevel": "NONE" }] },
                    { "contributionDays": [
                        { "date": "2026-09-06", "weekday": 0, "contributionCount": 3,
                          "contributionLevel": "SECOND_QUARTILE" },
                        { "date": "2026-09-07", "weekday": 1, "contributionCount": 5,
                          "contributionLevel": "FOURTH_QUARTILE" }
                    ] }
                ]
            } }
        } } })
    }

    #[test]
    fn calendar_keeps_partial_weeks_zero_days_and_api_intensities() {
        let calendar = parse_calendar(&serde_json::to_vec(&fixture()).unwrap(), "Octocat").unwrap();
        assert_eq!(calendar.total, 8);
        assert!(calendar.weeks[0][0].is_none());
        assert_eq!(calendar.weeks[0][6].unwrap().level, 0);
        assert_eq!(calendar.weeks[1][0].unwrap().level, 2);
        assert_eq!(calendar.weeks[1][1].unwrap().level, 4);
        assert!(calendar.weeks[1][2].is_none());
    }

    #[test]
    fn calendar_rejects_other_accounts_errors_and_invalid_dates() {
        assert!(parse_calendar(&serde_json::to_vec(&fixture()).unwrap(), "other-account").is_err());
        let mut value = fixture();
        value["errors"] = json!([{ "message": "private server detail" }]);
        let error = parse_calendar(&serde_json::to_vec(&value).unwrap(), "octocat").unwrap_err();
        assert!(!error.contains("private server detail"));
        for (field, invalid) in [
            ("date", json!("2026-09-99")),
            ("weekday", json!(5)),
            ("contributionLevel", json!("UNKNOWN")),
            ("contributionCount", json!(-1)),
        ] {
            let mut value = fixture();
            value["data"]["viewer"]["contributionsCollection"]["contributionCalendar"]["weeks"]
                [0]["contributionDays"][0][field] = invalid;
            assert!(
                parse_calendar(&serde_json::to_vec(&value).unwrap(), "octocat").is_err(),
                "{field}"
            );
        }
        let mut value = fixture();
        value["data"]["viewer"]["contributionsCollection"]["contributionCalendar"]["weeks"][1]
            ["contributionDays"][0]["date"] = json!("2026-09-13");
        assert!(parse_calendar(&serde_json::to_vec(&value).unwrap(), "octocat").is_err());
    }

    #[test]
    fn worker_discards_responses_for_old_requests_and_other_accounts() {
        let (request_tx, _) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = GithubContributionsWorker {
            request_tx,
            result_rx,
        };
        for (login, generation) in [("octocat", 1), ("other-account", 2), ("octocat", 2)] {
            result_tx
                .send(GithubContributionsResponse {
                    login: login.into(),
                    generation,
                    result: Err(login.into()),
                })
                .unwrap();
        }
        assert_eq!(
            worker.poll_latest("octocat", 2).unwrap().result,
            Err("octocat".into())
        );
        assert!(worker.poll_latest("octocat", 2).is_none());
    }
}

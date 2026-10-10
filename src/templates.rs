use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

use crate::error::AppResult;

#[derive(Debug, Template)]
#[template(path = "login.html")]
pub struct LoginTemplate<'a> {
    pub site_name: &'a str,
}

#[derive(Debug, Template)]
#[template(path = "dashboard.html")]
pub struct DashboardTemplate<'a> {
    pub site_name: &'a str,
    pub github_login: &'a str,
    pub github_name: &'a str,
    pub github_email: &'a str,
    pub github_id: i64,
    pub avatar_url: &'a str,
    pub api_key: &'a str,
    pub api_base_url: &'a str,
    pub created_at: &'a str,
    pub last_login_at: &'a str,
}

#[derive(Debug, Template)]
#[template(path = "error.html")]
pub struct ErrorTemplate<'a> {
    pub site_name: &'a str,
    pub status: StatusCode,
    pub title: &'a str,
    pub message: &'a str,
}

#[derive(Debug, Clone)]
pub struct RankingOption {
    pub value: String,
    pub label: String,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct RankingEntry {
    pub rank: String,
    pub avatar_url: String,
    pub display_name: String,
    pub secondary_name: String,
    pub value: String,
    pub components: Vec<RankingComponent>,
}

#[derive(Debug, Clone)]
pub struct RankingComponent {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Template)]
#[template(path = "ranking.html")]
pub struct RankingTemplate<'a> {
    pub site_name: &'a str,
    pub period_label: &'a str,
    pub metric_label: &'a str,
    pub periods: &'a [RankingOption],
    pub metrics: &'a [RankingOption],
    pub generated_at: &'a str,
    pub stale: bool,
    pub score_explanation: &'a str,
    pub entries: &'a [RankingEntry],
    pub empty_message: &'a str,
}

pub(crate) fn render<T: Template>(template: T) -> AppResult<Response> {
    Ok(Html(template.render()?).into_response())
}

//! The read-only dashboard (section 15): askama templates, refreshed by htmx
//! served from this binary (no CDN, no frontend build). With `HX-Request:
//! true` a view returns only its `<main>` fragment. No route writes anything.

pub mod views;

use crate::AppState;
use crate::auth::{
    AuthOutcome, AuthProvider, AuthRequest, OpenAuthProvider, Principal, TokenAuthProvider,
};
use askama::Template;
use axum::Extension;
use axum::extract::{Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};
use guard_core::ports::{CalibrationQuery, StoredGateResult};
use std::sync::Arc;
use views::*;

/// Pinned htmx, vendored from `htmx.org@2.0.4` (npm integrity
/// `sha512-HLxMCdfXDOJirs3vBZl/ZLoY+c7PfM4Ahr2Ad4YXh6d22T5ltbTXFFkpx9Tgb2vvmWFMbIc3LqN2ToNkZJvyYQ==`
/// for the tarball; license in `assets/htmx-LICENSE.txt`).
pub const HTMX_JS: &str = include_str!("../../assets/htmx-2.0.4.min.js");
pub const HTMX_PATH: &str = "/assets/htmx-2.0.4.min.js";
/// Subresource integrity of [`HTMX_JS`].
pub const HTMX_SRI: &str =
    "sha384-HGfztofotfshcF7+8n44JQL2oJmowVChPTg48S+jvZoztPfvwD79OC/LTtG6dMp+";

const NOT_CONFIGURED: &str = "Decision log not configured";
const UNAVAILABLE: &str = "Store unavailable: showing no data";

pub async fn htmx() -> Response {
    (
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (
                CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
        ],
        HTMX_JS,
    )
        .into_response()
}

fn provider(state: &AppState) -> Arc<dyn AuthProvider> {
    if let Some(p) = &state.dashboard_auth {
        return p.clone();
    }
    match &state.dashboard_token {
        Some(token) => Arc::new(TokenAuthProvider::new(token.clone())),
        None => Arc::new(OpenAuthProvider),
    }
}

#[derive(Template)]
#[template(path = "unauthorized.html")]
struct Unauthorized;

pub async fn require_login(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let outcome = {
        let auth = AuthRequest {
            path: request.uri().path(),
            query: request.uri().query(),
            headers: request.headers(),
        };
        provider(&state).authenticate(&auth).await
    };
    match outcome {
        AuthOutcome::Allowed(principal) => {
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        AuthOutcome::Redirect {
            location,
            set_cookie,
        } => {
            let mut response = StatusCode::SEE_OTHER.into_response();
            let headers = response.headers_mut();
            if let Ok(v) = HeaderValue::from_str(&location) {
                headers.insert(LOCATION, v);
            }
            if let Some(v) = set_cookie.and_then(|c| HeaderValue::from_str(&c).ok()) {
                headers.insert(SET_COOKIE, v);
            }
            response
        }
        AuthOutcome::Denied => {
            (StatusCode::UNAUTHORIZED, html(Unauthorized.render())).into_response()
        }
    }
}

/// The dashboard's Content-Security-Policy: scripts only from this origin
/// (the bundled htmx), inline styles for the layout, no framing, no plugins.
pub const CONTENT_SECURITY_POLICY: &str = concat!(
    "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; ",
    "img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; ",
    "frame-ancestors 'none'"
);

/// Adds the CSP and related headers to every dashboard response.
pub async fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

fn html(rendered: askama::Result<String>) -> Response {
    match rendered {
        Ok(body) => Html(body).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "template rendering failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("<p>rendering failed</p>".to_string()),
            )
                .into_response()
        }
    }
}

#[derive(Template)]
#[template(path = "layout.html")]
struct Layout<'a> {
    title: &'a str,
    login: &'a str,
    htmx_path: &'a str,
    htmx_sri: &'a str,
    main: &'a str,
}

/// Full page, or only the `<main>` fragment for htmx.
fn page(
    headers: &HeaderMap,
    principal: &Principal,
    title: &str,
    main: askama::Result<String>,
) -> Response {
    let main = match main {
        Ok(main) => main,
        Err(e) => return html(Err(e)),
    };
    if headers.get("hx-request").is_some_and(|v| v == "true") {
        return Html(main).into_response();
    }
    html(
        Layout {
            title,
            login: &principal.login,
            htmx_path: HTMX_PATH,
            htmx_sri: HTMX_SRI,
            main: &main,
        }
        .render(),
    )
}

/// `recent_gate_results(limit)`, or the empty-state notice.
async fn recent(state: &AppState, limit: u32) -> (Vec<StoredGateResult>, Option<&'static str>) {
    match &state.gates {
        None => (Vec::new(), Some(NOT_CONFIGURED)),
        Some(gates) => match gates.recent_gate_results(limit).await {
            Ok(rows) => (rows, None),
            Err(e) => {
                tracing::warn!(error = %e, "dashboard: gate history unavailable");
                (Vec::new(), Some(UNAVAILABLE))
            }
        },
    }
}

#[derive(Template)]
#[template(path = "overview.html")]
struct Overview {
    rows: Vec<GateRow>,
    notice: Option<&'static str>,
}

pub async fn overview(
    State(state): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
) -> Response {
    let (results, notice) = recent(&state, 10).await;
    let view = Overview {
        rows: gate_rows(&results),
        notice,
    };
    page(&headers, &p, "Overview", view.render())
}

#[derive(Template)]
#[template(path = "catalog.html")]
struct CatalogView {
    groups: Vec<AreaGroup>,
    notice: Option<&'static str>,
}

pub async fn catalog(
    State(state): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
) -> Response {
    let (results, notice) = recent(&state, 50).await;
    let view = CatalogView {
        groups: catalog_groups(&state.catalog, &results),
        notice,
    };
    page(&headers, &p, "Catalog", view.render())
}

#[derive(Template)]
#[template(path = "coverage.html")]
struct CoverageView {
    rows: Vec<CoverageRow>,
}

pub async fn coverage(
    State(state): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
) -> Response {
    let view = CoverageView {
        rows: coverage_rows(&state.config, &state.catalog),
    };
    page(&headers, &p, "Coverage", view.render())
}

#[derive(Template)]
#[template(path = "flaky.html")]
struct FlakyView {
    rows: Vec<FlakyRow>,
    notice: Option<&'static str>,
}

pub async fn flaky(
    State(state): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
) -> Response {
    let (results, notice) = recent(&state, 50).await;
    let view = FlakyView {
        rows: flaky_rows(&state.catalog, &results),
        notice,
    };
    page(&headers, &p, "Flaky", view.render())
}

#[derive(Template)]
#[template(path = "durations.html")]
struct DurationsView {
    d: Durations,
    notice: Option<&'static str>,
}

pub async fn durations_view(
    State(state): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
) -> Response {
    let (results, notice) = recent(&state, 50).await;
    let view = DurationsView {
        d: durations(&results),
        notice,
    };
    page(&headers, &p, "Durations", view.render())
}

#[derive(Template)]
#[template(path = "guards.html")]
struct GuardsView {
    rows: Vec<GateRow>,
    notice: Option<&'static str>,
}

pub async fn guards(
    State(state): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
) -> Response {
    let (results, notice) = recent(&state, 50).await;
    let view = GuardsView {
        rows: gate_rows(&results),
        notice,
    };
    page(&headers, &p, "Guards", view.render())
}

#[derive(Template)]
#[template(path = "decisions.html")]
struct DecisionsView {
    d: Decisions,
    notice: Option<&'static str>,
}

pub async fn decisions_view(
    State(state): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
) -> Response {
    let (rows, notice) = match &state.decisions {
        None => (Vec::new(), Some(NOT_CONFIGURED)),
        Some(log) => {
            let query = CalibrationQuery {
                limit: 200,
                ..CalibrationQuery::default()
            };
            match log.list_for_calibration(&query).await {
                Ok(rows) => (rows, None),
                Err(e) => {
                    tracing::warn!(error = %e, "dashboard: decision log unavailable");
                    (Vec::new(), Some(UNAVAILABLE))
                }
            }
        }
    };
    let view = DecisionsView {
        d: decisions(&rows),
        notice,
    };
    page(&headers, &p, "Decisions", view.render())
}

#[derive(Template)]
#[template(path = "agents.html")]
struct AgentsView;

pub async fn agents(Extension(p): Extension<Principal>, headers: HeaderMap) -> Response {
    page(&headers, &p, "Agents", AgentsView.render())
}

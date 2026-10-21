//! The web app: server-rendered HTML (maud) with htmx for partial updates.
//! No build step; htmx and the stylesheet are embedded in the binary.

mod layout;
mod print;
mod questions;
mod quizzes;

use crate::service::auth::{self, Session};
use crate::service::{App, AppError};
use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use maud::html;

pub const COOKIE: &str = "qf_session";

pub fn router(app: App) -> Router {
    Router::new()
        .route("/", get(questions::dashboard))
        .route("/healthz", get(|| async { "ok" }))
        .route("/static/htmx.min.js", get(htmx_js))
        .route("/static/app.css", get(app_css))
        .route("/login", get(login_help))
        .route("/login/{token}", get(login))
        .route("/logout", post(logout))
        .merge(questions::routes())
        .merge(quizzes::routes())
        .merge(print::routes())
        .layer(middleware::from_fn_with_state(app.clone(), csrf))
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}

// ---------------------------------------------------------------- auth

/// The signed-in member. Pages that take a `Viewer` need a session; without
/// one, browsers are sent to /login.
pub struct Viewer(pub Session);

impl Viewer {
    pub fn member(&self) -> &crate::service::members::Member {
        &self.0.member
    }
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

impl FromRequestParts<App> for Viewer {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> Result<Self, Self::Rejection> {
        if let Some(Viewer(s)) = parts.extensions.get::<Viewer>() {
            return Ok(Viewer(s.clone()));
        }
        let raw = cookie_value(&parts.headers, COOKIE);
        let session = match raw {
            Some(raw) => auth::session(app, &raw, app.now())
                .await
                .map_err(|e| WebError(e).into_response())?,
            None => None,
        };
        match session {
            Some(s) => Ok(Viewer(s)),
            None => Err(to_login(&parts.headers)),
        }
    }
}

impl Clone for Viewer {
    fn clone(&self) -> Self {
        Viewer(self.0.clone())
    }
}

fn is_htmx(headers: &HeaderMap) -> bool {
    headers.contains_key("hx-request")
}

fn to_login(headers: &HeaderMap) -> Response {
    if is_htmx(headers) {
        ([("hx-redirect", "/login")], StatusCode::UNAUTHORIZED).into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

/// Every state-changing request carries the session's CSRF token in the
/// `X-CSRF-Token` header (htmx adds it from the page). The session cookie is
/// also SameSite=Lax; this is the second lock.
async fn csrf(State(app): State<App>, req: Request, next: Next) -> Response {
    if req.method() == Method::GET || req.method() == Method::HEAD {
        return next.run(req).await;
    }
    let Some(raw) = cookie_value(req.headers(), COOKIE) else {
        return to_login(req.headers());
    };
    let session = match auth::session(&app, &raw, app.now()).await {
        Ok(Some(s)) => s,
        Ok(None) => return to_login(req.headers()),
        Err(e) => return WebError(e).into_response(),
    };
    let sent = req
        .headers()
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok());
    if sent != Some(session.csrf.as_str()) {
        return WebError(AppError::Forbidden(
            "This form has expired. Reload the page and try again.".into(),
        ))
        .into_response();
    }
    let mut req = req;
    req.extensions_mut().insert(Viewer(session));
    next.run(req).await
}

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; media-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    res
}

async fn login_help(State(app): State<App>) -> maud::Markup {
    let _ = app;
    layout::bare(
        "Sign in",
        html! {
            main.narrow {
                h1 { "quiz-forge" }
                p { "This question bank belongs to a Telegram group. To sign in, send " code { "/web" } " to the group's bot in a private chat. It replies with a one-time link." }
            }
        },
    )
}

async fn login(
    State(app): State<App>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Response {
    match auth::redeem(&app, &token, app.now()).await {
        Ok(s) => {
            let secure = if app.cfg.https() { "; Secure" } else { "" };
            let max_age = auth::SESSION_TTL.num_seconds();
            let cookie = format!(
                "{COOKIE}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}",
                s.raw_id
            );
            ([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response()
        }
        Err(e) => WebError(e).into_response(),
    }
}

async fn logout(State(app): State<App>, headers: HeaderMap) -> Response {
    if let Some(raw) = cookie_value(&headers, COOKIE) {
        let _ = auth::logout(&app, &raw).await;
    }
    let cookie = format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    (
        [
            (header::SET_COOKIE, cookie),
            (
                header::HeaderName::from_static("hx-redirect"),
                "/login".to_string(),
            ),
        ],
        Redirect::to("/login"),
    )
        .into_response()
}

async fn htmx_js() -> Response {
    static_file(
        "text/javascript",
        include_bytes!("../../static/htmx.min.js"),
    )
}

async fn app_css() -> Response {
    static_file("text/css", include_bytes!("../../static/app.css"))
}

fn static_file(ctype: &'static str, body: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, ctype),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response()
}

// ---------------------------------------------------------------- errors

pub struct WebError(pub AppError);

impl From<AppError> for WebError {
    fn from(e: AppError) -> Self {
        WebError(e)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self.0 {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found.".to_string()),
            AppError::Forbidden(m) => (StatusCode::FORBIDDEN, m.clone()),
            AppError::Invalid(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            AppError::Rules(r) => (
                StatusCode::BAD_REQUEST,
                r.errors
                    .iter()
                    .map(|e| e.message())
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            AppError::Db(_) | AppError::Other(_) => {
                tracing::error!("request failed: {}", self.0);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong on our side.".to_string(),
                )
            }
        };
        let page = layout::bare(
            "Error",
            html! {
                main.narrow {
                    h1 { "Can't do that" }
                    p.error { (msg) }
                    p { a href="/" { "← Home" } }
                }
            },
        );
        (status, page).into_response()
    }
}

pub type WebResult<T = Response> = Result<T, WebError>;

/// After a POST: a full page load for htmx (boosted forms), a 303 otherwise.
pub fn go(headers: &HeaderMap, to: &str) -> Response {
    if is_htmx(headers) {
        ([("hx-redirect", to.to_string())], StatusCode::OK).into_response()
    } else {
        Redirect::to(to).into_response()
    }
}

/// `path` with a notice for the next page.
pub fn with_notice(path: &str, notice: &str) -> String {
    let sep = if path.contains('?') { '&' } else { '?' };
    let (base, frag) = path
        .split_once('#')
        .map(|(b, f)| (b, format!("#{f}")))
        .unwrap_or((path, String::new()));
    format!(
        "{base}{sep}notice={}{frag}",
        url::form_urlencoded::byte_serialize(notice.as_bytes()).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parsing_and_notices() {
        let mut h = HeaderMap::new();
        h.append(
            header::COOKIE,
            HeaderValue::from_static("a=1; qf_session=abc"),
        );
        assert_eq!(cookie_value(&h, COOKIE).as_deref(), Some("abc"));
        assert_eq!(cookie_value(&h, "x"), None);
        assert_eq!(with_notice("/q/1", "Saved!"), "/q/1?notice=Saved%21");
        assert_eq!(with_notice("/q?x=1#p-2", "ok"), "/q?x=1&notice=ok#p-2");
    }
}

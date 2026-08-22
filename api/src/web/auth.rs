//! Sign in, sign out, registration and API-key reset.

use leasetrack_core::{
    User, authenticate_user, generate_api_key, issue_reset_token, load_users, redeem_reset_token,
    save_users,
};
use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        content::Form,
        error::see_other,
        header, HeaderValue,
        query_params, route,
        response::{IntoResponse, Response},
    },
    session,
    view::view,
};

use super::{base_url, current_email, layout::document, limiter, store};
use crate::ratelimit;

// ─── Index ────────────────────────────────────────────────────────────────────

/// `GET /` — go to the dashboard if already authenticated, else sign in.
#[route(GET "/")]
async fn index(cx: &Cx) -> Result<Response> {
    let target = if current_email(cx).await?.is_some() {
        "/dashboard"
    } else {
        "/login"
    };
    see_other(target).into_response(cx)
}

// ─── Login ────────────────────────────────────────────────────────────────────

/// `GET /login`
#[route(GET "/login")]
async fn login_page(cx: &Cx) -> Result<Response> {
    if current_email(cx).await?.is_some() {
        return see_other("/dashboard").into_response(cx);
    }
    login_view(cx, "").await
}

async fn login_view(cx: &Cx, error: &str) -> Result<Response> {
    view! { cx =>
        document(
            title: "LeaseTrack — Login",
            body_class: "centered",
            <div class="card">
                <h1>"LeaseTrack"</h1>
                <p class="subtitle">"Sign in with your email and API key"</p>
                if !error.is_empty() {
                    <div class="error">(error)</div>
                }
                <form method="post" action="/login" class="auth-form">
                    <label for="email">"Email"</label>
                    <input type="text" id="email" name="email" autofocus="" placeholder="you@example.com" autocomplete="email">
                    <label for="api_key">"API Key"</label>
                    <input type="password" id="api_key" name="api_key" placeholder="••••••••••••••••">
                    <button type="submit">"Sign in"</button>
                </form>
                <a class="back" href="/register">"No account yet? Register"</a>
                <a class="back tight" href="/forgot">"Forgot your API key?"</a>
            </div>
        )
    }?
    .into_response(cx)
}

#[derive(Deserialize)]
struct LoginForm {
    email: String,
    api_key: String,
}

/// `POST /login`
#[route(POST "/login")]
async fn login_post(cx: &Cx, Form(form): Form<LoginForm>) -> Result<Response> {
    let Some(user) = authenticate_user(form.email.trim(), &form.api_key) else {
        return login_view(cx, "Invalid email or API key. Please try again.").await;
    };

    // Identity comes from the stored user record, never from what was typed.
    // `start` always mints a fresh token, so signing in cannot be used to fixate
    // a session on someone else.
    let session = session::start(cx).await?;
    store(cx).create(session, user.email);

    see_other("/dashboard").into_response(cx)
}

/// `POST /logout` — destroys the session server-side, so the credential is
/// genuinely revoked rather than just dropped by the browser.
///
/// A POST rather than a GET: Topcoat's origin policy exempts safe methods, so a
/// state-changing GET would be reachable cross-site.
#[route(POST "/logout")]
async fn logout(cx: &Cx) -> Result<Response> {
    if let Some(token_hash) = session::stop(cx).await? {
        store(cx).delete(&token_hash);
    }
    see_other("/login").into_response(cx)
}

// ─── Registration ─────────────────────────────────────────────────────────────

async fn register_view(cx: &Cx, error: &str, success: bool) -> Result<Response> {
    view! { cx =>
        document(
            title: "LeaseTrack — Register",
            body_class: "centered",
            <div class="card">
                <h1>"LeaseTrack"</h1>
                <p class="subtitle">"Create your account"</p>
                if success {
                    <div class="success">
                        "Check your email — we've sent your API key to sign in with."
                    </div>
                    <a class="back" href="/login">"Back to sign in"</a>
                } else {
                    if !error.is_empty() {
                        <div class="error">(error)</div>
                    }
                    <form method="post" action="/register" class="auth-form">
                        <label for="email">"Email address"</label>
                        <input type="text" id="email" name="email" autofocus="" placeholder="you@example.com" autocomplete="email">
                        <button type="submit">"Send me my API key"</button>
                    </form>
                    <a class="back" href="/login">"Already have an account? Sign in"</a>
                }
            </div>
        )
    }?
    .into_response(cx)
}

/// `GET /register`
#[route(GET "/register")]
async fn register_page(cx: &Cx) -> Result<Response> {
    register_view(cx, "", false).await
}

#[derive(Deserialize)]
struct EmailForm {
    email: String,
}

/// `POST /register`
#[route(POST "/register")]
async fn register_post(cx: &Cx, Form(form): Form<EmailForm>) -> Result<Response> {
    let email = form.email.trim().to_lowercase();

    // Basic email sanity check
    if !email.contains('@') || email.len() < 3 {
        return register_view(cx, "Please enter a valid email address.", false).await;
    }

    if ratelimit::check_email_quota(limiter(cx), &email).is_err() {
        tracing::warn!("per-address registration quota exhausted");
        return register_view(cx, "", true).await;
    }

    let mut users = load_users().unwrap_or_default();

    // Already registered? We can no longer resend the key — only its hash is
    // stored — so send a reset link instead. The response is identical either
    // way, so the page still does not reveal which addresses are registered.
    if users.users.iter().any(|u| u.email == email) {
        send_reset_link(&email).await;
        return register_view(cx, "", true).await;
    }

    let api_key = generate_api_key();
    users.users.push(User::new(email.clone(), api_key.clone()));
    if let Err(e) = save_users(&users) {
        tracing::error!("Failed to save users file: {e}");
        return register_view(cx, "Something went wrong. Please try again.", false).await;
    }

    // Send email (or log to console locally). Still show success on failure —
    // the key is saved, so the user can request a reset.
    if let Err(e) = crate::email::send_registration_email(&email, &api_key).await {
        tracing::error!("Failed to send registration email: {e}");
    }

    register_view(cx, "", true).await
}

// ─── Forgot API key ───────────────────────────────────────────────────────────

async fn forgot_view(cx: &Cx, success: bool) -> Result<Response> {
    view! { cx =>
        document(
            title: "LeaseTrack — Forgot API Key",
            body_class: "centered",
            <div class="card">
                <h1>"LeaseTrack"</h1>
                <p class="subtitle">"Reset your API key"</p>
                if success {
                    <div class="success">
                        "If that email is registered, a reset link is on its way. \
                         The link expires in 30 minutes."
                    </div>
                    <a class="back" href="/login">"Back to sign in"</a>
                } else {
                    <form method="post" action="/forgot" class="auth-form">
                        <label for="email">"Email address"</label>
                        <input type="text" id="email" name="email" autofocus="" placeholder="you@example.com" autocomplete="email">
                        <button type="submit">"Send reset link"</button>
                    </form>
                    <a class="back" href="/login">"Back to sign in"</a>
                }
            </div>
        )
    }?
    .into_response(cx)
}

/// `GET /forgot`
#[route(GET "/forgot")]
async fn forgot_page(cx: &Cx) -> Result<Response> {
    forgot_view(cx, false).await
}

/// `POST /forgot` — email a single-use reset link if the address is registered.
///
/// The existing API key is left intact. Only following the emailed link rotates
/// it, so an unauthenticated request cannot lock a user out of their account.
#[route(POST "/forgot")]
async fn forgot_post(cx: &Cx, Form(form): Form<EmailForm>) -> Result<Response> {
    let email = form.email.trim().to_lowercase();

    // Cap how much mail one address can be made to receive, whatever the
    // source. Report success regardless, so this cannot be used to probe which
    // addresses are registered.
    if ratelimit::check_email_quota(limiter(cx), &email).is_err() {
        tracing::warn!("per-address reset quota exhausted");
        return forgot_view(cx, true).await;
    }

    send_reset_link(&email).await;
    forgot_view(cx, true).await
}

/// Issue and email a reset link, logging rather than surfacing any failure so
/// the caller's response cannot reveal whether the address is registered.
async fn send_reset_link(email: &str) {
    match issue_reset_token(email) {
        Ok(Some(token)) => {
            let link = format!("{}/reset?token={}", base_url(), token);
            if let Err(e) = crate::email::send_reset_email(email, &link).await {
                tracing::error!("Failed to send reset email: {e}");
            }
        }
        // No such user. Fall through to the same response so the page does not
        // reveal which addresses are registered.
        Ok(None) => {}
        Err(e) => tracing::error!("Failed to issue reset token: {e}"),
    }
}

// ─── Reset ────────────────────────────────────────────────────────────────────

#[query_params]
struct ResetQuery {
    #[serde(default)]
    token: String,
}

/// `GET /reset?token=…` — ask for confirmation, without redeeming anything.
///
/// Rotating the key here would let anything that merely *fetches* the URL do it:
/// mail scanners, link previewers and browser prefetchers all follow links in
/// email. Because the token is single-use, that burns it before the user ever
/// clicks, leaving them with an expired link and a key they did not ask to
/// change. The rotation therefore happens in [`reset_post`] below.
#[route(GET "/reset")]
async fn reset_page(cx: &Cx) -> Result<Response> {
    // A malformed query string is treated as a bad token rather than an error,
    // matching what a stale or truncated link should do.
    let token = match query_params::<ResetQuery>(cx) {
        Ok(query) => query.token.clone(),
        Err(_) => String::new(),
    };

    if token.is_empty() {
        return reset_result_view(cx, "Invalid or expired reset link.", "").await;
    }

    let mut response = view! { cx =>
        document(
            title: "LeaseTrack — Reset API Key",
            body_class: "centered",
            no_referrer: true,
            <div class="card card-wide">
                <h1>"LeaseTrack"</h1>
                <p class="subtitle">"Reset your API key"</p>
                <p class="hint-block">
                    "Confirm to generate a new API key. Your current key stops working \
                     immediately, and you will be signed out everywhere."
                </p>
                <form method="post" action="/reset">
                    <input type="hidden" name="token" value=(&token)>
                    <button type="submit">"Reset my API key"</button>
                </form>
                <a class="back" href="/login">"Cancel"</a>
            </div>
        )
    }?
    .into_response(cx)?;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    Ok(response)
}

#[derive(Deserialize)]
struct ResetForm {
    #[serde(default)]
    token: String,
}

/// `POST /reset` — redeem the token and show the new API key once.
#[route(POST "/reset")]
async fn reset_post(cx: &Cx, Form(form): Form<ResetForm>) -> Result<Response> {
    let (error, api_key) = match redeem_reset_token(&form.token) {
        Ok((email, new_key)) => {
            // The old key is gone, so any session resting on it must go too.
            store(cx).delete_for_email(&email);
            if let Err(e) = crate::email::send_registration_email(&email, &new_key).await {
                tracing::error!("Failed to send new key email: {e}");
            }
            (String::new(), new_key)
        }
        Err(e) => (e, String::new()),
    };

    reset_result_view(cx, &error, &api_key).await
}

/// The outcome page: either the freshly minted key, or why the link failed.
async fn reset_result_view(cx: &Cx, error: &str, api_key: &str) -> Result<Response> {
    let mut response = view! { cx =>
        document(
            title: "LeaseTrack — Reset API Key",
            body_class: "centered",
            no_referrer: true,
            <div class="card card-wide">
                <h1>"LeaseTrack"</h1>
                <p class="subtitle">"Reset your API key"</p>
                if !error.is_empty() {
                    <div class="error">(error)</div>
                    <p class="hint-block">
                        "Reset links can only be used once and expire after 30 minutes."
                    </p>
                    <a class="back" href="/forgot">"Request a new link"</a>
                } else {
                    <div class="success">"Your API key has been reset."</div>
                    <code class="key">(api_key)</code>
                    <p class="hint-block">
                        "Copy it now — this is the only time it is shown. A copy has also \
                         been emailed to you. Your previous key no longer works."
                    </p>
                    <a class="back" href="/login">"Go to sign in"</a>
                }
            </div>
        )
    }?
    .into_response(cx)?;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    Ok(response)
}

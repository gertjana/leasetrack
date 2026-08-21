//! The shared page chrome every LeaseTrack page renders inside.
//!
//! Replaces the `<head>`/preview-banner boilerplate that used to be copied
//! verbatim into all six templates.

use topcoat::{
    Result,
    context::Cx,
    view::{View, component, view},
};

use super::AppEnv;

/// URL of the shared stylesheet. Served by [`super::app_css`].
pub const APP_CSS: &str = "/assets/app.css";

/// The `<html>` document wrapper: head, preview banner, and page body.
///
/// `body_class` is `"centered"` for the single-card pages and empty for the
/// dashboard, which lays out a full-width grid instead. `script` is the URL of
/// a page-specific script, if any.
#[component]
pub async fn document(
    cx: &Cx,
    #[into] title: String,
    #[default("")] body_class: &str,
    #[default(None)] script: Option<&str>,
    #[default(false)] no_referrer: bool,
    child: View,
) -> Result {
    let app_env = super::app_env(cx);
    let centered = body_class.contains("centered");

    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width,initial-scale=1">
                if no_referrer {
                    <meta name="referrer" content="no-referrer">
                }
                <title>(title)</title>
                <link rel="stylesheet" href=(APP_CSS)>
                if let Some(src) = script {
                    <script src=(src) defer=""></script>
                }
            </head>
            <body class=(body_class)>
                preview_banner(app_env: app_env, spacer: centered)
                (child)
            </body>
        </html>
    }
}

/// A loud reminder that a non-production deployment is not the real thing.
#[component]
async fn preview_banner(app_env: &AppEnv, spacer: bool) -> Result {
    view! {
        if !app_env.is_production() {
            <div class="preview-banner">"⚠ PREVIEW — " (app_env.name())</div>
            if spacer {
                <div class="banner-spacer"></div>
            }
        }
    }
}

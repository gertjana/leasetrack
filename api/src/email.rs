/// Send a single-use API key reset link.
///
/// The link is what actually rotates the key, so it must only ever go to the
/// registered address.
pub async fn send_reset_email(to: &str, link: &str) -> Result<(), String> {
    let subject = "Reset your LeaseTrack API key";
    let body_text = format!(
        "Someone asked to reset the API key for this LeaseTrack account.\n\nOpen this link to get a new key:\n\n  {link}\n\nThe link works once and expires in 30 minutes.\n\nIf this wasn't you, ignore this email — your current API key still works.\n"
    );
    let body_html = format!(
        r#"<p>Someone asked to reset the API key for this LeaseTrack account.</p>
<p><a href="{link}">Click here to get a new API key</a></p>
<p>The link works once and expires in 30 minutes.</p>
<p><em>If this wasn't you, ignore this email — your current API key still works.</em></p>"#
    );
    send(to, subject, &body_text, &body_html).await
}

/// Send the registration email containing the generated API key via Resend's HTTP API.
///
/// If `RESEND_API_KEY` is not set, logs the email content to stdout instead
/// of calling the Resend API — useful for local development.
pub async fn send_registration_email(to: &str, api_key: &str) -> Result<(), String> {
    let subject = "Your LeaseTrack API key";
    let body_text = format!(
        "Welcome to LeaseTrack!\n\nYour API key is:\n\n  {api_key}\n\nUse this together with your email address to sign in.\n\nKeep it safe — anyone with this key can access your account.\n"
    );
    let body_html = format!(
        r#"<p>Welcome to LeaseTrack!</p>
<p>Your API key is:</p>
<pre style="background:#f6f8fa;padding:1rem;border-radius:6px;font-size:1.1rem">{api_key}</pre>
<p>Use this together with your email address to sign in.</p>
<p><em>Keep it safe — anyone with this key can access your account.</em></p>"#
    );
    send(to, subject, &body_text, &body_html).await
}

/// Deliver an email through Resend, or log it when running without credentials.
async fn send(to: &str, subject: &str, body_text: &str, body_html: &str) -> Result<(), String> {
    let from = std::env::var("RESEND_FROM")
        .unwrap_or_else(|_| "LeaseTrack <noreply@leasetrack.app>".to_string());

    match std::env::var("RESEND_API_KEY") {
        Ok(resend_key) => {
            tracing::info!("Sending email via Resend to={to} from={from}");
            let client = reqwest::Client::new();
            let res = client
                .post("https://api.resend.com/emails")
                .bearer_auth(&resend_key)
                .json(&serde_json::json!({
                    "from": from,
                    "to": [to],
                    "subject": subject,
                    "text": body_text,
                    "html": body_html,
                }))
                .send()
                .await
                .map_err(|e| format!("Failed to reach Resend API: {e}"))?;

            let status = res.status();
            let body = res.text().await.unwrap_or_default();

            if status.is_success() {
                tracing::info!("Resend accepted email: {body}");
                Ok(())
            } else {
                let err = format!("Resend API error {status}: {body}");
                tracing::error!("{err}");
                Err(err)
            }
        }
        Err(_) => {
            // No RESEND_API_KEY — log to console for local dev
            tracing::info!(
                "--- [DEV] Email (not sent) ---\nTo: {to}\nSubject: {subject}\n\n{body_text}---"
            );
            Ok(())
        }
    }
}

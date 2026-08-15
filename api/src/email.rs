/// Send the registration email containing the generated API key via Resend's HTTP API.
///
/// If `RESEND_API_KEY` is not set, logs the email content to stdout instead
/// of calling the Resend API — useful for local development.
pub async fn send_registration_email(to: &str, api_key: &str) -> Result<(), String> {
    let from = std::env::var("RESEND_FROM")
        .unwrap_or_else(|_| "LeaseTrack <noreply@leasetrack.app>".to_string());

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

    match std::env::var("RESEND_API_KEY") {
        Ok(resend_key) => {
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
                .map_err(|e| format!("Failed to send email: {e}"))?;

            if res.status().is_success() {
                Ok(())
            } else {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                Err(format!("Resend API error {status}: {body}"))
            }
        }
        Err(_) => {
            // No RESEND_API_KEY — log to console for local dev
            tracing::info!(
                "--- [DEV] Registration email (not sent) ---\nTo: {to}\nSubject: {subject}\n\n{body_text}---"
            );
            Ok(())
        }
    }
}

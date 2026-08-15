use resend_rs::types::CreateEmailBaseOptions;
use resend_rs::Resend;

/// Send the registration email containing the generated API key.
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
            let client = Resend::new(&resend_key);
            let email = CreateEmailBaseOptions::new(&from, [to], subject)
                .with_text(&body_text)
                .with_html(&body_html);

            client
                .emails
                .send(email)
                .await
                .map(|_| ())
                .map_err(|e| format!("Failed to send email: {e}"))
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

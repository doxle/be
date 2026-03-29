use aws_sdk_sesv2::Client as SesClient;
use aws_sdk_sesv2::types::{Body, Content, Destination, EmailContent, Message};

/// Send invite email via AWS SES
pub async fn send_invite_email(
    ses_client: &SesClient,
    to_email: &str,
    invite_code: &str,
    frontend_url: &str,
) -> Result<(), String> {
    // Use root URL + query param so production static hosting doesn't 404 on deep links.
    // Frontend will detect `?code=...` and route to signup client-side.
    let base_url = frontend_url.trim_end_matches('/');
    let signup_link = format!("{}/?code={}", base_url, invite_code);
    
    let icon_url = "https://send-email-logo.s3.ap-southeast-2.amazonaws.com/logo.png";
    
    let html_body = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
</head>
<body style="margin: 0; padding: 0; font-family: Helvetica, Arial, sans-serif; background: #ffffff;">
    <table role="presentation" width="100%" cellspacing="0" cellpadding="0" border="0">
        <tr>
            <td align="center" style="padding: 60px 20px;">
                <table role="presentation" width="600" cellspacing="0" cellpadding="0" border="0" style="max-width: 600px; background: #ffffff; border: 1px solid #e5e5e5;">
                    <tr>
                        <td style="padding: 60px 50px;">
                            <!-- Logo -->
                            <table role="presentation" width="100%" cellspacing="0" cellpadding="0" border="0">
                                <tr>
                                    <td align="center" style="padding-bottom: 40px;">
                                        <img src="{}" alt="Doxle" width="40" style="display: block; height: auto;" />
                                    </td>
                                </tr>
                            </table>
                            
                            <!-- Title -->
                            <h2 style="font-family: Helvetica, Arial, sans-serif; font-size: 20px; font-weight: 300; color: #000000; margin: 0 0 24px 0;">You've been invited</h2>
                            
                            <!-- Text -->
                            <p style="font-family: Helvetica, Arial, sans-serif; font-size: 15px; font-weight: 400; color: #333333; margin: 0 0 24px 0; line-height: 1.6;">
                                You've been invited to join Doxle. Click the button below to create your account and get started.
                            </p>
                            
                            <!-- Button -->
                            <table role="presentation" width="100%" cellspacing="0" cellpadding="0" border="0">
                                <tr>
                                    <td align="center" style="padding: 32px 0;">
                                        <table role="presentation" cellspacing="0" cellpadding="0" border="0">
                                            <tr>
                                                <td bgcolor='#4f5bf8' style="background: #4f5bf8; background-color: #4f5bf8; padding: 18px 48px;">
                                                    <a href="{}" style="font-family: Helvetica, Arial, sans-serif; font-size: 15px; font-weight: 400; color: #ffffff !important; text-decoration: none !important; display: block; -webkit-text-fill-color: #ffffff !important;">Create Account</a>
                                                </td>
                                            </tr>
                                        </table>
                                    </td>
                                </tr>
                            </table>
                            
                            <!-- Code label -->
                            <p style="font-family: Helvetica, Arial, sans-serif; font-size: 13px; font-weight: 500; color: #666666; margin: 32px 0 8px 0;">Or use this invite code:</p>
                            
                            <!-- Code -->
                            <p style="font-family: 'Courier New', monospace; font-size: 13px; color: #000000; background: #f5f5f5; padding: 14px 16px; border: 1px solid #e5e5e5; margin: 0 0 16px 0; word-break: break-all;">{}</p>
                            
                            <!-- Expiry note -->
                            <p style="font-family: Helvetica, Arial, sans-serif; font-size: 13px; color: #666666; margin: 32px 0 0 0; line-height: 1.6;">
                                This invitation expires in 7 days. If you didn't expect this, you can safely ignore this email.
                            </p>
                            
                            <!-- Footer -->
                            <table role="presentation" width="100%" cellspacing="0" cellpadding="0" border="0" style="margin-top: 48px; border-top: 1px solid #e5e5e5;">
                                <tr>
                                    <td align="center" style="padding-top: 24px;">
                                        <p style="font-family: Helvetica, Arial, sans-serif; font-size: 13px; font-weight: 300; color: #666666; margin: 0;">© 2025 Doxle</p>
                                    </td>
                                </tr>
                            </table>
                        </td>
                    </tr>
                </table>
            </td>
        </tr>
    </table>
</body>
</html>"#,
        icon_url, signup_link, invite_code
    );

    let text_body = format!(
        r#"Doxle

You've been invited

You've been invited to join Doxle. Click the link below to create your account:

{}

Or use this invite code: {}

This invitation expires in 7 days. If you didn't expect this, you can safely ignore this email.

© 2025 Doxle"#,
        signup_link, invite_code
    );

    let destination = Destination::builder()
        .to_addresses(to_email)
        .build();

    let subject = Content::builder()
        .data("You've been invited to join Doxle")
        .charset("UTF-8")
        .build()
        .map_err(|e| format!("Failed to build subject: {:?}", e))?;

    let html_content = Content::builder()
        .data(html_body)
        .charset("UTF-8")
        .build()
        .map_err(|e| format!("Failed to build HTML content: {:?}", e))?;

    let text_content = Content::builder()
        .data(text_body)
        .charset("UTF-8")
        .build()
        .map_err(|e| format!("Failed to build text content: {:?}", e))?;

    let body = Body::builder()
        .html(html_content)
        .text(text_content)
        .build();

    let message = Message::builder()
        .subject(subject)
        .body(body)
        .build();

    let email_content = EmailContent::builder()
        .simple(message)
        .build();

    let from_email = std::env::var("SES_FROM_EMAIL")
        .unwrap_or_else(|_| "Doxle <noreply@doxle.com>".to_string());
    
    ses_client
        .send_email()
        .from_email_address(from_email)
        .destination(destination)
        .content(email_content)
        .send()
        .await
        .map_err(|e| format!("Failed to send email: {:?}", e))?;

    Ok(())
}

/// Send contact form email via AWS SES
pub async fn send_contact_email(
    ses_client: &SesClient,
    from_email_address: &str,
    message: &str,
) -> Result<(), String> {
    let to_email = "help@doxle.com";
    
    let html_body = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <style>
        body {{
            font-family: 'HelveticaNeue', Helvetica, Arial, sans-serif;
            line-height: 1.6;
            color: #333333;
            background: #ffffff;
            margin: 0;
            padding: 20px;
        }}
        .container {{
            max-width: 600px;
            margin: 0 auto;
            padding: 30px;
            border: 1px solid #e5e5e5;
        }}
        .title {{
            font-size: 20px;
            font-weight: 300;
            margin-bottom: 20px;
        }}
        .from {{
            font-size: 14px;
            color: #666;
            margin-bottom: 20px;
        }}
        .message {{
            font-size: 15px;
            white-space: pre-wrap;
        }}
    </style>
</head>
<body>
    <div class="container">
        <h2 class="title">New message from Doxle</h2>
        <p class="from"><strong>From:</strong> {}</p>
        <div class="message">{}</div>
    </div>
</body>
</html>"#,
        from_email_address, message
    );

    let text_body = format!(
        "New Contact Form Message\n\nFrom: {}\n\nMessage:\n{}",
        from_email_address, message
    );

    let destination = Destination::builder()
        .to_addresses(to_email)
        .build();

    let subject = Content::builder()
        .data(format!("New message from Doxle: {}", from_email_address))
        .charset("UTF-8")
        .build()
        .map_err(|e| format!("Failed to build subject: {:?}", e))?;

    let html_content = Content::builder()
        .data(html_body)
        .charset("UTF-8")
        .build()
        .map_err(|e| format!("Failed to build HTML content: {:?}", e))?;

    let text_content = Content::builder()
        .data(text_body)
        .charset("UTF-8")
        .build()
        .map_err(|e| format!("Failed to build text content: {:?}", e))?;

    let body = Body::builder()
        .html(html_content)
        .text(text_content)
        .build();

    let message_obj = Message::builder()
        .subject(subject)
        .body(body)
        .build();

    let email_content = EmailContent::builder()
        .simple(message_obj)
        .build();

    let ses_from_email = std::env::var("SES_FROM_EMAIL")
        .unwrap_or_else(|_| "Doxle <noreply@doxle.com>".to_string());
    
    ses_client
        .send_email()
        .from_email_address(ses_from_email)
        .destination(destination)
        .content(email_content)
        .send()
        .await
        .map_err(|e| format!("Failed to send email: {:?}", e))?;

    Ok(())
}

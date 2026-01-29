use aws_sdk_sesv2::Client as SesClient;
use aws_sdk_sesv2::types::{Body, Content, Destination, EmailContent, Message};

/// Send invite email via AWS SES
pub async fn send_invite_email(
    ses_client: &SesClient,
    to_email: &str,
    invite_code: &str,
    frontend_url: &str,
) -> Result<(), String> {
    let signup_link = format!("{}/signup?code={}", frontend_url, invite_code);
    
    let html_body = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <style>
        body {{
            font-family: 'HelveticaNeue', Helvetica, Arial, sans-serif;
            line-height: 1.6;
            color: #333333;
            background: #ffffff;
            margin: 0;
            padding: 0;
        }}
        .wrapper {{
            max-width: 600px;
            margin: 0 auto;
            padding: 60px 20px;
        }}
        .container {{
            background: #ffffff;
            border: 1px solid #e5e5e5;
            padding: 60px 50px;
        }}
        .logo {{
            font-size: 24px;
            font-weight: 300;
            color: #000000;
            margin: 0 0 40px 0;
            text-align: center;
            letter-spacing: -0.5px;
        }}
        .title {{
            font-size: 20px;
            font-weight: 300;
            color: #000000;
            margin: 0 0 24px 0;
        }}
        .text {{
            font-size: 15px;
            font-weight: 400;
            color: #333333;
            margin: 0 0 24px 0;
            line-height: 1.6;
        }}
        .button-wrapper {{
            text-align: center;
            margin: 32px 0;
        }}
        .button {{
            display: inline-block;
            width: 100%;
            max-width: 280px;
            padding: 18px 24px;
            background: #4f5bf8;
            color: #ffffff;
            text-decoration: none;
            font-weight: 400;
            font-size: 15px;
            text-align: center;
            box-sizing: border-box;
        }}
        .button:hover {{
            background: rgba(79, 91, 248, 0.9);
        }}
        .code-label {{
            font-size: 13px;
            font-weight: 500;
            color: #666666;
            margin: 32px 0 8px 0;
        }}
        .code {{
            background: #f5f5f5;
            padding: 14px 16px;
            font-family: 'Courier New', monospace;
            font-size: 13px;
            color: #000000;
            border: 1px solid #e5e5e5;
            word-break: break-all;
            margin: 0 0 16px 0;
        }}
        .footer {{
            margin-top: 48px;
            padding-top: 24px;
            border-top: 1px solid #e5e5e5;
            font-size: 13px;
            font-weight: 300;
            color: #666666;
            text-align: center;
        }}
        .footer-text {{
            margin: 0 0 8px 0;
        }}
        @media only screen and (max-width: 600px) {{
            .container {{
                padding: 40px 24px;
            }}
            .wrapper {{
                padding: 40px 16px;
            }}
        }}
    </style>
</head>
<body>
    <div class="wrapper">
        <div class="container">
            <h1 class="logo">Doxle</h1>
            
            <h2 class="title">You've been invited</h2>
            
            <p class="text">
                You've been invited to join Doxle. Click the button below to create your account and get started.
            </p>
            
            <div class="button-wrapper">
                <a href="{}" class="button">Create Account</a>
            </div>
            
            <div class="code-label">Or use this invite code:</div>
            <div class="code">{}</div>
            
            <p class="text" style="margin-top: 32px; font-size: 13px; color: #666666;">
                This invitation expires in 7 days. If you didn't expect this, you can safely ignore this email.
            </p>
            
            <div class="footer">
                <p class="footer-text">© 2025 Doxle</p>
            </div>
        </div>
    </div>
</body>
</html>"#,
        signup_link, invite_code
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
        .unwrap_or_else(|_| "noreply@doxle.ai".to_string());
    
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
        <h2 class="title">New Contact Form Message</h2>
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
        .data(format!("Contact Form: Message from {}", from_email_address))
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
        .unwrap_or_else(|_| "noreply@doxle.ai".to_string());
    
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

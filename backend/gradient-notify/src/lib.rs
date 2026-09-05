/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use gradient_types::EmailConfig;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use tokio::fs;
use tracing::info;

/// Outbound email delivery. Production impl is `EmailService` (SMTP via lettre);
/// tests use an in-memory recorder.
#[async_trait]
pub trait EmailSender: Send + Sync + std::fmt::Debug + 'static {
    fn is_enabled(&self) -> bool;

    async fn send_verification_email(
        &self,
        to_email: &str,
        to_name: &str,
        verification_token: &str,
        base_url: &str,
    ) -> Result<()>;

    async fn send_password_reset_email(
        &self,
        to_email: &str,
        to_name: &str,
        reset_token: &str,
        base_url: &str,
    ) -> Result<()>;

    async fn send_action_mail(
        &self,
        to: &[String],
        subject: &str,
        body: &str,
    ) -> Result<MailDeliveryResult>;

    async fn send_invitation_email(
        &self,
        to_email: &str,
        to_name: &str,
        invite: &InvitationMail<'_>,
    ) -> Result<()>;

    async fn send_subscription_mail(
        &self,
        to: &[String],
        mail: &SubscriptionMail<'_>,
    ) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct MailDeliveryResult {
    pub status_code: i32,
    pub server_response: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteScope {
    Project,
    Cache,
}

impl InviteScope {
    fn label(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Cache => "cache",
        }
    }
}

#[derive(Clone, Debug)]
pub struct InvitationMail<'a> {
    pub scope: InviteScope,
    pub scope_display_name: &'a str,
    pub role: &'a str,
    pub inviter: &'a str,
    pub accept_url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriptionEvent {
    Requested,
    Approved,
    Denied,
}

#[derive(Clone, Debug)]
pub struct SubscriptionMail<'a> {
    pub event: SubscriptionEvent,
    pub project_display_name: &'a str,
    pub cache_display_name: &'a str,
    pub mode: &'a str,
    pub actor: &'a str,
    pub link: String,
}

#[derive(Debug)]
pub struct EmailService {
    transport: Option<SmtpTransport>,
    from_address: String,
    from_name: String,
    enabled: bool,
}

impl EmailService {
    pub async fn new(config: Option<EmailConfig>) -> Result<Self> {
        let Some(config) = config else {
            return Ok(Self {
                transport: None,
                from_address: String::new(),
                from_name: String::new(),
                enabled: false,
            });
        };

        let smtp_password = fs::read_to_string(&config.smtp_password_file)
            .await
            .context("Failed to read SMTP password file")?
            .trim()
            .to_string();

        let credentials = Credentials::new(config.smtp_username.clone(), smtp_password);

        let transport = if !config.enable_tls {
            SmtpTransport::builder_dangerous(&config.smtp_host)
                .credentials(credentials)
                .port(config.smtp_port)
                .build()
        } else {
            SmtpTransport::relay(&config.smtp_host)
                .context("Failed to create SMTP transport")?
                .credentials(credentials)
                .port(config.smtp_port)
                .build()
        };

        Ok(Self {
            transport: Some(transport),
            from_address: config.from_address.clone(),
            from_name: config.from_name.clone(),
            enabled: true,
        })
    }
}

#[async_trait]
impl EmailSender for EmailService {
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn send_verification_email(
        &self,
        to_email: &str,
        to_name: &str,
        verification_token: &str,
        base_url: &str,
    ) -> Result<()> {
        if !self.enabled {
            bail!("Email service is not enabled");
        }

        let transport = self
            .transport
            .as_ref()
            .context("SMTP transport not initialized")?;

        let verification_url = format!(
            "{}/api/v1/auth/verify-email?token={}",
            base_url, verification_token
        );

        let email_body = verification_email_html(to_name, &verification_url);

        let email = Message::builder()
            .from(
                format!("{} <{}>", self.from_name, self.from_address)
                    .parse()
                    .context("Invalid from address")?,
            )
            .to(format!("{} <{}>", to_name, to_email)
                .parse()
                .context("Invalid to address")?)
            .subject("Verify your email address - Gradient")
            .header(ContentType::TEXT_HTML)
            .body(email_body)
            .context("Failed to build email")?;

        transport.send(&email).context("Failed to send email")?;

        info!(to = to_email, "Verification email sent");
        Ok(())
    }

    async fn send_password_reset_email(
        &self,
        to_email: &str,
        to_name: &str,
        reset_token: &str,
        base_url: &str,
    ) -> Result<()> {
        if !self.enabled {
            bail!("Email service is not enabled");
        }

        let transport = self
            .transport
            .as_ref()
            .context("SMTP transport not initialized")?;

        let reset_url = format!("{}/reset-password?token={}", base_url, reset_token);

        let email_body = password_reset_email_html(to_name, &reset_url);

        let email = Message::builder()
            .from(
                format!("{} <{}>", self.from_name, self.from_address)
                    .parse()
                    .context("Invalid from address")?,
            )
            .to(format!("{} <{}>", to_name, to_email)
                .parse()
                .context("Invalid to address")?)
            .subject("Reset your password - Gradient")
            .header(ContentType::TEXT_HTML)
            .body(email_body)
            .context("Failed to build email")?;

        transport.send(&email).context("Failed to send email")?;

        info!(to = to_email, "Password reset email sent");
        Ok(())
    }

    async fn send_action_mail(
        &self,
        to: &[String],
        subject: &str,
        body: &str,
    ) -> Result<MailDeliveryResult> {
        if !self.enabled {
            bail!("SMTP is not configured on this server");
        }
        if to.is_empty() {
            bail!("send_action_mail: no recipients");
        }
        let transport = self
            .transport
            .as_ref()
            .context("SMTP is not configured on this server")?;
        let from = format!("{} <{}>", self.from_name, self.from_address);
        let mut builder = Message::builder()
            .from(from.parse().context("invalid from address")?)
            .subject(subject);
        for addr in to {
            builder = builder.to(addr.parse().context("invalid recipient")?);
        }
        let msg = builder
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .context("building email message")?;
        let response = transport.send(&msg).context("smtp send")?;
        Ok(MailDeliveryResult {
            status_code: u16::from(response.code()) as i32,
            server_response: response.message().collect::<Vec<_>>().join(" "),
        })
    }
    async fn send_invitation_email(
        &self,
        to_email: &str,
        to_name: &str,
        invite: &InvitationMail<'_>,
    ) -> Result<()> {
        if !self.enabled {
            bail!("Email service is not enabled");
        }

        let transport = self
            .transport
            .as_ref()
            .context("SMTP transport not initialized")?;

        let subject = format!(
            "You have been invited to the {} {} - Gradient",
            invite.scope_display_name,
            invite.scope.label()
        );

        let email = Message::builder()
            .from(
                format!("{} <{}>", self.from_name, self.from_address)
                    .parse()
                    .context("Invalid from address")?,
            )
            .to(format!("{} <{}>", to_name, to_email)
                .parse()
                .context("Invalid to address")?)
            .subject(subject)
            .header(ContentType::TEXT_HTML)
            .body(invitation_email_html(to_name, invite))
            .context("Failed to build email")?;

        transport.send(&email).context("Failed to send email")?;

        info!(to = to_email, "Invitation email sent");
        Ok(())
    }

    async fn send_subscription_mail(
        &self,
        to: &[String],
        mail: &SubscriptionMail<'_>,
    ) -> Result<()> {
        if !self.enabled {
            bail!("Email service is not enabled");
        }
        if to.is_empty() {
            return Ok(());
        }

        let transport = self
            .transport
            .as_ref()
            .context("SMTP transport not initialized")?;

        let subject = match mail.event {
            SubscriptionEvent::Requested => format!(
                "{} requests access to the {} cache - Gradient",
                mail.project_display_name, mail.cache_display_name
            ),
            SubscriptionEvent::Approved => format!(
                "{} may now use the {} cache - Gradient",
                mail.project_display_name, mail.cache_display_name
            ),
            SubscriptionEvent::Denied => format!(
                "Cache request for {} was declined - Gradient",
                mail.project_display_name
            ),
        };

        let mut builder = Message::builder()
            .from(
                format!("{} <{}>", self.from_name, self.from_address)
                    .parse()
                    .context("Invalid from address")?,
            )
            .subject(subject);
        for addr in to {
            builder = builder.to(addr.parse().context("invalid recipient")?);
        }

        let msg = builder
            .header(ContentType::TEXT_HTML)
            .body(subscription_email_html(mail))
            .context("Failed to build email")?;

        transport.send(&msg).context("Failed to send email")?;

        Ok(())
    }
}

fn verification_email_html(name: &str, verification_url: &str) -> String {
    format!(
        r#"
<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Verify your email address</title>
</head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
    <div style="max-width: 600px; margin: 0 auto; padding: 20px;">
        <h1 style="color: #2c3e50;">Welcome to Gradient!</h1>
        
        <p>Hello {name},</p>
        
        <p>Thank you for registering with Gradient. To complete your registration and activate your account, please verify your email address by clicking the button below:</p>
        
        <div style="text-align: center; margin: 30px 0;">
            <a href="{verification_url}" 
               style="background-color: #3498db; color: white; padding: 12px 30px; text-decoration: none; border-radius: 5px; display: inline-block; font-weight: bold;">
                Verify Email Address
            </a>
        </div>
        
        <p>If the button above doesn't work, you can also copy and paste the following link into your browser:</p>
        <p style="word-break: break-all; background-color: #f8f9fa; padding: 10px; border-radius: 3px;">
            {verification_url}
        </p>
        
        <p>This verification link will expire in 24 hours for security reasons.</p>
        
        <p>If you didn't create an account with Gradient, you can safely ignore this email.</p>
        
        <hr style="border: none; border-top: 1px solid #eee; margin: 30px 0;">
        
        <p style="font-size: 12px; color: #666;">
            This email was sent by Gradient. If you have any questions, please contact your system administrator.
        </p>
    </div>
</body>
</html>
"#
    )
}

fn password_reset_email_html(name: &str, reset_url: &str) -> String {
    format!(
        r#"
<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>Reset your password</title>
</head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
    <div style="max-width: 600px; margin: 0 auto; padding: 20px;">
        <h1 style="color: #2c3e50;">Password Reset Request</h1>
        
        <p>Hello {name},</p>
        
        <p>We received a request to reset your password for your Gradient account. If you made this request, click the button below to reset your password:</p>
        
        <div style="text-align: center; margin: 30px 0;">
            <a href="{reset_url}" 
               style="background-color: #e74c3c; color: white; padding: 12px 30px; text-decoration: none; border-radius: 5px; display: inline-block; font-weight: bold;">
                Reset Password
            </a>
        </div>
        
        <p>If the button above doesn't work, you can also copy and paste the following link into your browser:</p>
        <p style="word-break: break-all; background-color: #f8f9fa; padding: 10px; border-radius: 3px;">
            {reset_url}
        </p>
        
        <p>This password reset link will expire in 1 hour for security reasons.</p>
        
        <p><strong>If you didn't request a password reset, you can safely ignore this email.</strong> Your password will remain unchanged.</p>
        
        <hr style="border: none; border-top: 1px solid #eee; margin: 30px 0;">
        
        <p style="font-size: 12px; color: #666;">
            This email was sent by Gradient. If you have any questions, please contact your system administrator.
        </p>
    </div>
</body>
</html>
"#
    )
}

fn invitation_email_html(name: &str, invite: &InvitationMail<'_>) -> String {
    let scope = invite.scope.label();
    let scope_name = invite.scope_display_name;
    let role = invite.role;
    let inviter = invite.inviter;
    let accept_url = &invite.accept_url;
    format!(
        r#"
<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>You have been invited</title>
</head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
    <div style="max-width: 600px; margin: 0 auto; padding: 20px;">
        <h1 style="color: #2c3e50;">You have been invited</h1>

        <p>Hello {name},</p>

        <p>{inviter} invited you to the <strong>{scope_name}</strong> {scope} on Gradient as <strong>{role}</strong>.</p>

        <div style="text-align: center; margin: 30px 0;">
            <a href="{accept_url}"
               style="background-color: #3498db; color: white; padding: 12px 30px; text-decoration: none; border-radius: 5px; display: inline-block; font-weight: bold;">
                Review Invitation
            </a>
        </div>

        <p>If the button above doesn't work, you can also copy and paste the following link into your browser:</p>
        <p style="word-break: break-all; background-color: #f8f9fa; padding: 10px; border-radius: 3px;">
            {accept_url}
        </p>

        <p>You need to be signed in as the invited account to accept. The invitation expires in 7 days, and you can also find it under Settings, My Invites.</p>

        <p>If you were not expecting this invitation, you can safely ignore this email.</p>

        <hr style="border: none; border-top: 1px solid #eee; margin: 30px 0;">

        <p style="font-size: 12px; color: #666;">
            This email was sent by Gradient. If you have any questions, please contact your system administrator.
        </p>
    </div>
</body>
</html>
"#
    )
}

fn subscription_email_html(mail: &SubscriptionMail<'_>) -> String {
    let project = mail.project_display_name;
    let cache = mail.cache_display_name;
    let mode = mail.mode;
    let actor = mail.actor;
    let link = &mail.link;
    let (heading, body) = match mail.event {
        SubscriptionEvent::Requested => (
            "Cache subscription requested",
            format!(
                "{actor} asked to subscribe the <strong>{project}</strong> project to the <strong>{cache}</strong> cache in <strong>{mode}</strong> mode."
            ),
        ),
        SubscriptionEvent::Approved => (
            "Cache subscription approved",
            format!(
                "{actor} approved the <strong>{project}</strong> project's subscription to the <strong>{cache}</strong> cache in <strong>{mode}</strong> mode."
            ),
        ),
        SubscriptionEvent::Denied => (
            "Cache subscription declined",
            format!(
                "{actor} declined the <strong>{project}</strong> project's request to subscribe to the <strong>{cache}</strong> cache."
            ),
        ),
    };

    format!(
        r#"
<!DOCTYPE html>
<html>
<head>
    <meta charset="utf-8">
    <title>{heading}</title>
</head>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
    <div style="max-width: 600px; margin: 0 auto; padding: 20px;">
        <h1 style="color: #2c3e50;">{heading}</h1>

        <p>{body}</p>

        <div style="text-align: center; margin: 30px 0;">
            <a href="{link}"
               style="background-color: #3498db; color: white; padding: 12px 30px; text-decoration: none; border-radius: 5px; display: inline-block; font-weight: bold;">
                Open in Gradient
            </a>
        </div>

        <hr style="border: none; border-top: 1px solid #eee; margin: 30px 0;">

        <p style="font-size: 12px; color: #666;">
            This email was sent by Gradient. If you have any questions, please contact your system administrator.
        </p>
    </div>
</body>
</html>
"#
    )
}

/// 32 random bytes, hex encoded. Used for e-mail verification and invitations.
pub fn generate_token() -> String {
    let token: [u8; 32] = rand::random();
    hex::encode(token)
}

pub fn generate_verification_token() -> String {
    generate_token()
}

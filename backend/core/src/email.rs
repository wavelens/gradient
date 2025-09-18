/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::{Cli, ServerState};
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use std::sync::Arc;
use tokio::fs;
use tracing::info;

pub struct EmailService {
    transport: Option<SmtpTransport>,
    from_address: String,
    from_name: String,
    enabled: bool,
}

impl EmailService {
    pub async fn new(cli: &Cli) -> Result<Self, String> {
        if !cli.email_enabled {
            return Ok(Self {
                transport: None,
                from_address: String::new(),
                from_name: cli.email_from_name.clone(),
                enabled: false,
            });
        }

        let smtp_host = cli
            .email_smtp_host
            .as_ref()
            .ok_or("SMTP host is required when email is enabled")?;

        let smtp_username = cli
            .email_smtp_username
            .as_ref()
            .ok_or("SMTP username is required when email is enabled")?;

        let smtp_password_file = cli
            .email_smtp_password_file
            .as_ref()
            .ok_or("SMTP password file is required when email is enabled")?;

        let from_address = cli
            .email_from_address
            .as_ref()
            .ok_or("From address is required when email is enabled")?;

        let smtp_password = fs::read_to_string(smtp_password_file)
            .await
            .map_err(|e| format!("Failed to read SMTP password file: {}", e))?
            .trim()
            .to_string();

        let credentials = Credentials::new(smtp_username.clone(), smtp_password);

        let transport = SmtpTransport::relay(smtp_host)
            .map_err(|e| format!("Failed to create SMTP transport: {}", e))?
            .credentials(credentials)
            .port(cli.email_smtp_port)
            .build();

        Ok(Self {
            transport: Some(transport),
            from_address: from_address.clone(),
            from_name: cli.email_from_name.clone(),
            enabled: true,
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub async fn send_verification_email(
        &self,
        to_email: &str,
        to_name: &str,
        verification_token: &str,
        base_url: &str,
    ) -> Result<(), String> {
        if !self.enabled {
            return Err("Email service is not enabled".to_string());
        }

        let transport = self
            .transport
            .as_ref()
            .ok_or("SMTP transport not initialized")?;

        let verification_url = format!(
            "{}/api/v1/auth/verify-email?token={}",
            base_url, verification_token
        );

        let email_body = format!(
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
"#,
            name = to_name,
            verification_url = verification_url
        );

        let email = Message::builder()
            .from(
                format!("{} <{}>", self.from_name, self.from_address)
                    .parse()
                    .map_err(|e| format!("Invalid from address: {}", e))?,
            )
            .to(format!("{} <{}>", to_name, to_email)
                .parse()
                .map_err(|e| format!("Invalid to address: {}", e))?)
            .subject("Verify your email address - Gradient")
            .header(ContentType::TEXT_HTML)
            .body(email_body)
            .map_err(|e| format!("Failed to build email: {}", e))?;

        transport
            .send(&email)
            .map_err(|e| format!("Failed to send email: {}", e))?;

        info!("Verification email sent to {}", to_email);
        Ok(())
    }

    pub async fn send_password_reset_email(
        &self,
        to_email: &str,
        to_name: &str,
        reset_token: &str,
        base_url: &str,
    ) -> Result<(), String> {
        if !self.enabled {
            return Err("Email service is not enabled".to_string());
        }

        let transport = self
            .transport
            .as_ref()
            .ok_or("SMTP transport not initialized")?;

        let reset_url = format!("{}/reset-password?token={}", base_url, reset_token);

        let email_body = format!(
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
"#,
            name = to_name,
            reset_url = reset_url
        );

        let email = Message::builder()
            .from(
                format!("{} <{}>", self.from_name, self.from_address)
                    .parse()
                    .map_err(|e| format!("Invalid from address: {}", e))?,
            )
            .to(format!("{} <{}>", to_name, to_email)
                .parse()
                .map_err(|e| format!("Invalid to address: {}", e))?)
            .subject("Reset your password - Gradient")
            .header(ContentType::TEXT_HTML)
            .body(email_body)
            .map_err(|e| format!("Failed to build email: {}", e))?;

        transport
            .send(&email)
            .map_err(|e| format!("Failed to send email: {}", e))?;

        info!("Password reset email sent to {}", to_email);
        Ok(())
    }
}

pub fn generate_verification_token() -> String {
    let token: [u8; 32] = rand::random();
    hex::encode(token)
}

pub async fn create_email_service(state: Arc<ServerState>) -> Result<EmailService, String> {
    EmailService::new(&state.cli).await
}

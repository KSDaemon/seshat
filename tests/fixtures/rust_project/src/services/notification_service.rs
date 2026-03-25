// Demonstrates:
// - thiserror error types with #[from] attribute
// - tracing logging
// - Enum variants
// - Trait definitions and implementations

use std::fmt;

use thiserror::Error;
use tracing::{debug, error};

/// Notification delivery channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Email,
    Sms,
    Push,
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Email => write!(f, "email"),
            Self::Sms => write!(f, "sms"),
            Self::Push => write!(f, "push"),
        }
    }
}

/// Errors from the notification service.
#[derive(Debug, Error)]
pub enum NotificationError {
    #[error("Failed to send {channel} notification: {reason}")]
    SendFailed { channel: Channel, reason: String },

    #[error("Template rendering error")]
    TemplateError(#[from] TemplateError),

    #[error("Rate limit exceeded for channel {0}")]
    RateLimited(Channel),
}

/// Errors from template rendering.
#[derive(Debug, Error)]
#[error("Template '{name}' failed: {details}")]
pub struct TemplateError {
    pub name: String,
    pub details: String,
}

/// Trait for sending notifications.
pub trait NotificationSender: Send + Sync {
    fn send(&self, recipient: &str, message: &str) -> Result<(), NotificationError>;
    fn channel(&self) -> Channel;
}

/// Email notification sender.
pub struct EmailSender {
    smtp_host: String,
}

impl EmailSender {
    pub fn new(smtp_host: String) -> Self {
        Self { smtp_host }
    }
}

impl NotificationSender for EmailSender {
    fn send(&self, recipient: &str, message: &str) -> Result<(), NotificationError> {
        debug!(
            recipient,
            host = %self.smtp_host,
            "Sending email notification"
        );

        if recipient.is_empty() {
            error!("Empty recipient for email notification");
            return Err(NotificationError::SendFailed {
                channel: Channel::Email,
                reason: "Empty recipient".into(),
            });
        }

        // Simulate sending
        let _ = message;
        Ok(())
    }

    fn channel(&self) -> Channel {
        Channel::Email
    }
}

/// Dispatches notifications to the appropriate sender.
pub fn dispatch_notification(
    senders: &[Box<dyn NotificationSender>],
    channel: Channel,
    recipient: &str,
    message: &str,
) -> Result<(), NotificationError> {
    for sender in senders {
        if sender.channel() == channel {
            return sender.send(recipient, message);
        }
    }

    Err(NotificationError::SendFailed {
        channel,
        reason: "No sender registered for channel".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSender;

    impl NotificationSender for MockSender {
        fn send(&self, _recipient: &str, _message: &str) -> Result<(), NotificationError> {
            Ok(())
        }

        fn channel(&self) -> Channel {
            Channel::Push
        }
    }

    #[test]
    fn test_channel_display() {
        assert_eq!(Channel::Email.to_string(), "email");
        assert_eq!(Channel::Sms.to_string(), "sms");
        assert_eq!(Channel::Push.to_string(), "push");
    }

    #[test]
    fn test_email_sender_empty_recipient() {
        let sender = EmailSender::new("smtp.example.com".into());
        let result = sender.send("", "Hello");
        assert!(result.is_err());
    }

    #[test]
    fn test_dispatch_notification() {
        let senders: Vec<Box<dyn NotificationSender>> = vec![Box::new(MockSender)];
        let result = dispatch_notification(&senders, Channel::Push, "user@example.com", "Hello");
        assert!(result.is_ok());
    }

    #[test]
    fn test_dispatch_no_sender() {
        let senders: Vec<Box<dyn NotificationSender>> = vec![];
        let result = dispatch_notification(&senders, Channel::Email, "user@example.com", "Hello");
        assert!(result.is_err());
    }

    #[test]
    fn test_template_error_from() {
        let template_err = TemplateError {
            name: "welcome".into(),
            details: "Missing variable".into(),
        };
        let notif_err: NotificationError = template_err.into();
        assert!(matches!(notif_err, NotificationError::TemplateError(_)));
    }
}

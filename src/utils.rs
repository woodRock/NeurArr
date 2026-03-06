use notify_rust::Notification;
use tracing::error;

pub fn send_notification(title: &str, body: &str) {
    if let Err(e) = Notification::new()
        .summary(title)
        .body(body)
        .appname("NeurArr")
        .show() 
    {
        error!("Failed to send desktop notification: {}", e);
    }
}

pub mod auth {
    use argon2::{
        password_hash::{rand_core::OsRng, PasswordHasher, PasswordVerifier, SaltString},
        Argon2,
    };

    pub fn hash_password(password: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        argon2.hash_password(password.as_bytes(), &salt).unwrap().to_string()
    }

    pub fn verify_password(password: &str, hash: &str) -> bool {
        let argon2 = Argon2::default();
        if let Ok(parsed_hash) = argon2::PasswordHash::new(hash) {
            return argon2.verify_password(password.as_bytes(), &parsed_hash).is_ok();
        }
        false
    }
}

pub struct Renamer;

impl Renamer {
    pub fn format_movie(template: &str, title: &str, year: &str, quality: &str) -> String {
        template
            .replace("{title}", title)
            .replace("{year}", year)
            .replace("{quality}", quality)
    }

    pub fn format_tv(template: &str, title: &str, season: i64, episode: i64, quality: &str) -> String {
        template
            .replace("{title}", title)
            .replace("{season}", &format!("{:02}", season))
            .replace("{episode}", &format!("{:02}", episode))
            .replace("{quality}", quality)
    }
}

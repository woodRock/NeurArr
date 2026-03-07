use notify_rust::Notification;
use tracing::{error, info};

#[allow(dead_code)]
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

pub struct Renamer {
    pub library_dir: String,
}

impl Renamer {
    pub fn new(library_dir: String) -> Self {
        Self { library_dir }
    }

    pub fn sanitize_filename(name: &str) -> String {
        name.replace(|c: char| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'), "")
            .trim()
            .to_string()
    }

    #[allow(dead_code)]
    pub fn format_movie(template: &str, title: &str, year: &str, quality: &str) -> String {
        let title_clean = Self::sanitize_filename(title);
        template
            .replace("{title}", &title_clean)
            .replace("{year}", year)
            .replace("{quality}", quality)
    }

    #[allow(dead_code)]
    pub fn format_tv(template: &str, title: &str, season: i64, episode: i64, quality: &str) -> String {
        let title_clean = Self::sanitize_filename(title);
        template
            .replace("{title}", &title_clean)
            .replace("{season}", &format!("{:02}", season))
            .replace("{episode}", &format!("{:02}", episode))
            .replace("{quality}", quality)
    }

    pub async fn move_file(&self, path: &std::path::Path, metadata: &crate::parser::MediaMetadata, final_title: &str) -> anyhow::Result<()> {
        let mut dest = std::path::PathBuf::from(&self.library_dir);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("mkv");
        let sanitized_title = Self::sanitize_filename(final_title);
        
        if let Some(s) = metadata.season {
            dest.push("TV");
            dest.push(&sanitized_title);
            dest.push(format!("Season {}", s));
            tokio::fs::create_dir_all(&dest).await?;
            dest.push(format!("{} - S{:02}E{:02}.{}", sanitized_title, s, metadata.episode.unwrap_or(0), ext));
        } else {
            dest.push("Movies");
            dest.push(&sanitized_title);
            tokio::fs::create_dir_all(&dest).await?;
            dest.push(format!("{}.{}", sanitized_title, ext));
        }

        info!("Renamer: Moving {} to {}", path.display(), dest.display());

        // Try rename first (fast, same device)
        if let Err(_) = tokio::fs::rename(path, &dest).await {
            // Fallback to copy + delete (cross-device)
            tokio::fs::copy(path, &dest).await?;
            tokio::fs::remove_file(path).await?;
        }
        Ok(())
    }
}

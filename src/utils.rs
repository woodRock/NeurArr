use notify_rust::Notification;
use tracing::error;

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
    #[allow(dead_code)]
    pub fn format_movie(template: &str, title: &str, year: &str, quality: &str) -> String {
        template
            .replace("{title}", title)
            .replace("{year}", year)
            .replace("{quality}", quality)
    }

    #[allow(dead_code)]
    pub fn format_tv(template: &str, title: &str, season: i64, episode: i64, quality: &str) -> String {
        template
            .replace("{title}", title)
            .replace("{season}", &format!("{:02}", season))
            .replace("{episode}", &format!("{:02}", episode))
            .replace("{quality}", quality)
    }

    pub async fn move_file(&self, path: &std::path::Path, metadata: &crate::parser::MediaMetadata, final_title: &str) -> anyhow::Result<()> {
        let mut dest = std::path::PathBuf::from(&self.library_dir);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("mkv");
        
        if let Some(s) = metadata.season {
            dest.push("TV");
            dest.push(final_title);
            dest.push(format!("Season {}", s));
            tokio::fs::create_dir_all(&dest).await?;
            dest.push(format!("{} - S{:02}E{:02}.{}", final_title, s, metadata.episode.unwrap_or(0), ext));
        } else {
            dest.push("Movies");
            dest.push(final_title);
            tokio::fs::create_dir_all(&dest).await?;
            dest.push(format!("{}.{}", final_title, ext));
        }

        tokio::fs::rename(path, &dest).await?;
        Ok(())
    }
}

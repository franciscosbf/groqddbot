use std::path::Path;

use config::{Config, ConfigError};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to read config")]
    ReadedError(#[source] ConfigError),
    #[error("failed to to parse config")]
    ParserError(#[source] ConfigError),
    #[error("prompt_size must be between 255 and 4096 characters")]
    InvalidPromptSize,
    #[error("flush_days must be greater than zero")]
    InvalidFlushDays,
    #[error("history_size must be greater than zero")]
    InvalidHistorySize,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct Bot {
    pub discord_token: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct AiProvider {
    pub api_key: String,
    pub model: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct Chat {
    pub prompt_size: u16,
    pub flush_days: u8,
    pub history_size: u8,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct App {
    pub bot: Bot,
    pub chat: Chat,
    pub ai_provider: AiProvider,
}

impl App {
    pub fn parse(path: &Path) -> Result<Self, Error> {
        let file = config::File::from(path);

        let config = Config::builder()
            .add_source(file)
            .build()
            .map_err(Error::ReadedError)?
            .try_deserialize::<App>()
            .map_err(Error::ParserError)?;

        if !(255..=4096).contains(&config.chat.prompt_size) {
            return Err(Error::InvalidFlushDays);
        }

        if config.chat.flush_days == 0 {
            return Err(Error::InvalidFlushDays);
        }

        if config.chat.history_size == 0 {
            return Err(Error::InvalidHistorySize);
        }

        Ok(config)
    }
}

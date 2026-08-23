use std::io;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum HostError {
    #[error("{0}")]
    Parse(String),
    #[error("{0}")]
    Init(String),
    #[error("{0}")]
    Listen(String),
    #[error("{0} (win32={1})")]
    Win32(String, u32),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl HostError {
    pub fn parse_failure(message: &str) -> Self {
        Self::Parse(message.to_string())
    }

    pub fn init(message: impl Into<String>) -> Self {
        Self::Init(message.into())
    }

    pub fn listen(message: impl Into<String>) -> Self {
        Self::Listen(message.into())
    }

    pub fn win32(message: &str, code: u32) -> Self {
        Self::Win32(message.to_string(), code)
    }

    pub fn io(error: io::Error) -> Self {
        Self::Io(error)
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Parse(_) => 1,
            Self::Init(_) | Self::Win32(..) | Self::Io(_) => 2,
            Self::Listen(_) => 3,
        }
    }
}

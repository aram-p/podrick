//! Errors carry the exit code, so an agent branches on the code and never parses English.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Ok` documents the code space even though success never errors.
pub enum Code {
    Ok = 0,
    NotFound = 1,
    Usage = 2,
    Conflict = 3,
    Io = 4,
}

#[derive(Debug)]
pub struct AppError {
    pub code: Code,
    pub msg: String,
    /// What the caller should do about it. Always present when there is an obvious fix.
    pub hint: Option<String>,
}

impl AppError {
    pub fn new(code: Code, msg: impl Into<String>) -> Self {
        AppError {
            code,
            msg: msg.into(),
            hint: None,
        }
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(Code::NotFound, msg)
    }
    pub fn usage(msg: impl Into<String>) -> Self {
        Self::new(Code::Usage, msg)
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::new(Code::Conflict, msg)
    }
    pub fn io(msg: impl Into<String>) -> Self {
        Self::new(Code::Io, msg)
    }
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

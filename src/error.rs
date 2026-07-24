use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
    pub byte_offset: usize,
}

#[derive(Debug)]
pub struct ArgentError {
    pub path: Option<PathBuf>,
    pub location: Option<SourceLocation>,
    pub message: String,
}

impl ArgentError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { path: None, location: None, message: message.into() }
    }

    pub fn at(path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        Self { path: Some(path.into()), location: None, message: message.into() }
    }

    pub fn in_source(source: &str, byte_offset: usize, message: impl Into<String>) -> Self {
        Self { path: None, location: Some(source_location(source, byte_offset)), message: message.into() }
    }

    pub fn at_source(path: impl Into<PathBuf>, source: &str, byte_offset: usize, message: impl Into<String>) -> Self {
        Self { path: Some(path.into()), location: Some(source_location(source, byte_offset)), message: message.into() }
    }

    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }
}

impl fmt::Display for ArgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.path, self.location) {
            (Some(path), Some(location)) => {
                write!(f, "{}:{}:{}: {}", path.display(), location.line, location.column, self.message)
            }
            (Some(path), None) => write!(f, "{}: {}", path.display(), self.message),
            (None, Some(location)) => write!(f, "{}:{}: {}", location.line, location.column, self.message),
            (None, None) => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ArgentError {}

impl From<std::io::Error> for ArgentError {
    fn from(value: std::io::Error) -> Self {
        Self::new(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ArgentError>;

fn source_location(source: &str, byte_offset: usize) -> SourceLocation {
    let mut byte_offset = byte_offset.min(source.len());
    while !source.is_char_boundary(byte_offset) {
        byte_offset -= 1;
    }

    let prefix = &source[..byte_offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column = prefix.rsplit('\n').next().unwrap_or_default().chars().count() + 1;
    SourceLocation { line, column, byte_offset }
}

#[cfg(test)]
mod tests {
    use super::ArgentError;

    #[test]
    fn displays_path_line_and_character_column() {
        let err = ArgentError::at_source("demo.ag", "state Café {}\n/", "state Café {}\n".len(), "unexpected token");

        assert_eq!(err.to_string(), "demo.ag:2:1: unexpected token");
        let location = err.location.expect("source location");
        assert_eq!(location.byte_offset, 15);
    }
}

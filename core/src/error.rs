use std::{fmt, panic::Location};

pub type Result<T> = std::result::Result<T, Error>;

pub struct Error {
    message: Box<str>,
    location: &'static Location<'static>,
}

impl Error {
    #[track_caller]
    pub fn message(message: impl fmt::Display) -> Self {
        Self {
            message: message.to_string().into_boxed_str(),
            location: Location::caller(),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} [{}:{}:{}]",
            self.message,
            self.location.file(),
            self.location.line(),
            self.location.column()
        )
    }
}

impl<E: std::error::Error + 'static> From<E> for Error {
    #[track_caller]
    fn from(error: E) -> Self {
        Self::message(error)
    }
}

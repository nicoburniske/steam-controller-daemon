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

impl std::error::Error for Error {}

pub trait ResultExt<T> {
    fn whence(self) -> Result<T>;
}

impl<T, E: fmt::Display> ResultExt<T> for std::result::Result<T, E> {
    #[track_caller]
    fn whence(self) -> Result<T> {
        match self {
            Ok(value) => Ok(value),
            Err(error) => Err(Error::message(error)),
        }
    }
}

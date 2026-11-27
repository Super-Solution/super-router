use std::fmt;

/// What can go wrong that is not an ordinary answer (those are the outcome enums).
#[derive(Debug)]
pub enum Error {
    /// The caller asked for something the rules never allow (a rate over 100%, an empty name).
    Invalid(String),
    /// The database failed.
    Db(sqlx::Error),
    /// The sink refused or could not reach the ledger. Nothing was recorded.
    Grant(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(m) => write!(f, "invalid: {m}"),
            Error::Db(e) => write!(f, "database: {e}"),
            Error::Grant(m) => write!(f, "grant: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Error::Db(e)
    }
}

impl From<crate::sink::SinkError> for Error {
    fn from(e: crate::sink::SinkError) -> Self {
        Error::Grant(e.0)
    }
}

pub(crate) fn invalid<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

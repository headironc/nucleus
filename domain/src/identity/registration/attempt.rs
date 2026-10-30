use tokio_postgres::types::{FromSql, ToSql};
use uuid::Uuid;

/// The successful Receipt preserves the identity of its original Attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromSql, ToSql)]
#[postgres(transparent)]
pub(crate) struct Id(Uuid);

impl Id {
    pub(crate) fn allocate() -> Self {
        Self(Uuid::new_v4())
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::AsRefStr, strum::EnumString)]
#[strum(serialize_all = "snake_case")]
pub(crate) enum Status {
    Registered,
    MobileTaken,
    IdNumberTaken,
}

impl<'a> FromSql<'a> for Status {
    fn from_sql(
        kind: &tokio_postgres::types::Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(<&str>::from_sql(kind, raw)?.parse()?)
    }

    fn accepts(kind: &tokio_postgres::types::Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

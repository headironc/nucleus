//! Validated pagination shared by business list reads.

use serde::Deserialize;
use serde_with::{DisplayFromStr, PickFirst, serde_as};

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(try_from = "Input")]
pub struct Pagination {
    offset: i64,
    size: i64,
}

impl Pagination {
    pub fn new(page: i64, size: i64) -> Result<Self, Error> {
        if page < 1 || !(1..=100).contains(&size) {
            return Err(Error);
        }
        match (page - 1).checked_mul(size) {
            Some(offset) => Ok(Self { offset, size }),
            None => Err(Error),
        }
    }

    pub(crate) fn offset(self) -> i64 {
        self.offset
    }

    pub(crate) fn size(self) -> i64 {
        self.size
    }
}

#[serde_as]
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Input {
    #[serde_as(as = "PickFirst<(_, DisplayFromStr)>")]
    page: i64,
    #[serde_as(as = "PickFirst<(_, DisplayFromStr)>")]
    size: i64,
}

impl Default for Input {
    fn default() -> Self {
        Self { page: 1, size: 20 }
    }
}

impl TryFrom<Input> for Pagination {
    type Error = Error;
    fn try_from(input: Input) -> Result<Self, Error> {
        Self::new(input.page, input.size)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("page is positive, size is between 1 and 100, and the offset fits PostgreSQL bigint")]
pub struct Error;

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use crate::pagination::Pagination;

    #[test]
    fn pagination_defaults_only_missing_fields_and_rejects_overflow() {
        for (input, offset, size) in [
            (r#"{}"#, 0, 20),
            (r#"{"page":2}"#, 20, 20),
            (r#"{"size":100}"#, 0, 100),
            (r#"{"page":1,"size":1}"#, 0, 1),
        ] {
            let pagination: Pagination = serde_json::from_str(input).expect("valid pagination");
            assert_eq!((pagination.offset, pagination.size), (offset, size));
        }
        for input in [
            r#"{"page":0}"#,
            r#"{"page":-1}"#,
            r#"{"size":0}"#,
            r#"{"size":101}"#,
            r#"{"page":null}"#,
            r#"{"page":1.5}"#,
            r#"{"page":1,"page":2}"#,
            r#"{"entry":"institution"}"#,
            r#"{"page":9223372036854775807,"size":100}"#,
        ] {
            assert!(
                serde_json::from_str::<Pagination>(input).is_err(),
                "{input}"
            );
        }
        assert!(Pagination::new(i64::MAX, 1).is_ok());
        assert!(Pagination::new(i64::MAX, 2).is_err());
    }
}

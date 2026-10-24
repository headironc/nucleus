//! Current avatar metadata; a missing account differs from its missing required avatar.

use tokio_postgres::{Client, Row};

use crate::identity::{
    account,
    avatar::{Error, Revision, link},
};

pub(crate) struct Snapshot {
    pub(super) revision: Revision,
    pub(super) digest: link::Digest,
    pub(super) size: i64,
}

impl TryFrom<&Row> for Snapshot {
    type Error = Error;

    fn try_from(row: &Row) -> Result<Self, Error> {
        let width: i32 = row.try_get("width").map_err(Error::decode)?;
        let height: i32 = row.try_get("height").map_err(Error::decode)?;
        if width != 512 || height != 512 {
            return Err(Error::Facts {
                rule: "current default avatar dimensions are 512 by 512",
            });
        }
        Ok(Self {
            revision: row.try_get("avatar").map_err(Error::decode)?,
            digest: link::Digest::new(
                row.try_get::<_, Vec<u8>>("digest")
                    .map_err(Error::decode)?
                    .try_into()
                    .map_err(Error::invalid_digest)?,
            ),
            size: row.try_get("size").map_err(Error::decode)?,
        })
    }
}

pub(crate) async fn find_by_account(
    client: &Client,
    account: &crate::history::Verified<account::state::State>,
) -> Result<Option<Snapshot>, Error> {
    if account.current().is_none() {
        return Ok(None);
    }
    client
        .query_opt(
            "SELECT $2::uuid AS avatar, avatar.digest, avatar.size, avatar.width, avatar.height
                FROM identity.avatars avatar WHERE avatar.account_id=$1 AND avatar.released_at IS NULL",
            &[&account.id(), &account.avatar()],
        )
        .await?
        .as_ref()
        .map(Snapshot::try_from)
        .transpose()
}

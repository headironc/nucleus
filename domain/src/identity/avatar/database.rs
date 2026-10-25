//! Current avatar metadata; a missing account differs from its missing required avatar.

use tokio_postgres::{Client, Row};

use crate::identity::{
    account,
    avatar::{Error, Revision, link},
};

pub(crate) struct Snapshot {
    id: crate::identity::avatar::Id,
    revision: Revision,
    digest: link::Digest,
    size: i64,
    width: u16,
    height: u16,
}

const CONTENT_SEAL: &str = "identity.avatar.content";

impl Snapshot {
    pub(super) fn id(&self) -> super::Id {
        self.id
    }
    pub(super) fn digest(&self) -> link::Digest {
        self.digest
    }
    pub(super) fn size(&self) -> i64 {
        self.size
    }
    pub(super) fn width(&self) -> u16 {
        self.width
    }
    pub(super) fn height(&self) -> u16 {
        self.height
    }

    pub(crate) fn revision(&self) -> Revision {
        self.revision
    }

    pub(crate) fn read(
        row: &Row,
        seals: &crate::keys::seal::Keys,
        defaults: &super::Defaults,
    ) -> Result<Self, Error> {
        let width: i32 = row.try_get("width").map_err(Error::decode)?;
        let height: i32 = row.try_get("height").map_err(Error::decode)?;
        let size: i64 = row.try_get("size").map_err(Error::decode)?;
        if width != height || !(1..=1024).contains(&width) || size <= 0 || size > 8_388_608 {
            return Err(Error::Facts {
                rule: "current avatar metadata is one square within its byte and pixel limits",
            });
        }
        let snapshot = Self {
            id: row.try_get("id").map_err(Error::decode)?,
            revision: row.try_get("avatar").map_err(Error::decode)?,
            digest: link::Digest::new(
                row.try_get::<_, Vec<u8>>("digest")
                    .map_err(Error::decode)?
                    .try_into()
                    .map_err(Error::invalid_digest)?,
            ),
            size,
            width: u16::try_from(width).map_err(|_| Error::Facts {
                rule: "avatar width is a u16",
            })?,
            height: u16::try_from(height).map_err(|_| Error::Facts {
                rule: "avatar height is a u16",
            })?,
        };
        let tag: Option<Vec<u8>> = row.try_get("content_seal").map_err(Error::decode)?;
        let key: Option<crate::keys::Id> =
            row.try_get("content_seal_key_id").map_err(Error::decode)?;
        let version: Option<i16> = row.try_get("content_seal_version").map_err(Error::decode)?;
        match (tag, key, version) {
            (None, None, None) => {
                let content = defaults
                    .find(snapshot.digest.as_bytes())
                    .ok_or(Error::Mismatch)?;
                if snapshot.size != content.size()
                    || snapshot.width != 512
                    || snapshot.height != 512
                {
                    return Err(Error::Mismatch);
                }
            }
            (Some(tag), Some(key), Some(version)) => {
                let account = row.try_get("account_id").map_err(Error::decode)?;
                let fields = content_fields(
                    snapshot.id,
                    account,
                    snapshot.revision,
                    snapshot.digest.as_bytes(),
                    snapshot.size,
                    snapshot.width,
                    snapshot.height,
                );
                seals.check(
                    CONTENT_SEAL,
                    1,
                    &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                    &crate::keys::seal::Seal::stored(tag, key, version)?,
                )?;
                if defaults.find(snapshot.digest.as_bytes()).is_some() {
                    return Err(Error::Facts {
                        rule: "private avatar content is outside the frozen defaults",
                    });
                }
            }
            _ => {
                return Err(Error::Facts {
                    rule: "private avatar content retains its complete proof",
                });
            }
        }
        Ok(snapshot)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the seal binds each immutable avatar fact and its owner"
)]
pub(super) fn seal_content(
    seals: &crate::keys::seal::Keys,
    id: super::Id,
    account: account::Id,
    revision: Revision,
    digest: &[u8; 32],
    size: i64,
    width: u16,
    height: u16,
) -> Result<crate::keys::seal::Seal, Error> {
    let fields = content_fields(id, account, revision, digest, size, width, height);
    Ok(seals.sign(
        CONTENT_SEAL,
        1,
        &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
    )?)
}

fn content_fields(
    id: super::Id,
    account: account::Id,
    revision: Revision,
    digest: &[u8; 32],
    size: i64,
    width: u16,
    height: u16,
) -> Vec<Vec<u8>> {
    vec![
        id.as_bytes().to_vec(),
        account.as_bytes().to_vec(),
        revision.as_bytes().to_vec(),
        digest.to_vec(),
        size.to_be_bytes().to_vec(),
        width.to_be_bytes().to_vec(),
        height.to_be_bytes().to_vec(),
    ]
}

pub(crate) async fn find_by_account(
    client: &Client,
    account: &crate::history::Verified<account::state::State>,
    seals: &crate::keys::seal::Keys,
    defaults: &super::Defaults,
) -> Result<Option<Snapshot>, Error> {
    if account.current().is_none() {
        return Ok(None);
    }
    let snapshot = client
        .query_opt(
            "SELECT avatar.revision AS avatar, avatar.id, avatar.account_id, avatar.digest, avatar.size, avatar.width, avatar.height, avatar.content_seal, avatar.content_seal_key_id, avatar.content_seal_version
                FROM identity.avatars avatar WHERE avatar.account_id=$1 AND avatar.released_at IS NULL",
            &[&account.id()],
        )
        .await?
        .as_ref()
        .map(|row| Snapshot::read(row, seals, defaults))
        .transpose()?;
    if snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.revision != account.avatar())
    {
        return Err(Error::Facts {
            rule: "the current avatar revision matches its authenticated account",
        });
    }
    Ok(snapshot)
}

use std::{collections::BTreeMap, net::IpAddr};

use chrono::{DateTime, TimeDelta, Utc};
use tokio_postgres::Client;

use crate::identity::{authentication::Error, message::Limits, mobile::hash::Hash};

/// The caller holds the mobile, original source and deployment permit locks.
pub(super) async fn verify(
    client: &Client,
    limits: &Limits,
    mobile: &Hash,
    source: IpAddr,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let rows = client.query(
        "SELECT message.created_at, challenge.source, (challenge.mobile).hash AS hash
         FROM identity.messages message JOIN identity.challenges challenge ON challenge.id = message.challenge_id
         WHERE message.created_at > $1::timestamptz - interval '24 hours' ORDER BY message.created_at, message.id",
        &[&now],
    ).await?;
    let mut daily = Vec::new();
    let mut mobile_daily = Vec::new();
    let mut mobile_hourly = Vec::new();
    let mut source_minute = Vec::new();
    let mut source_hourly = BTreeMap::new();
    for row in rows {
        let created_at: DateTime<Utc> = row.try_get("created_at")?;
        let hash: Hash = row.try_get("hash")?;
        let origin: IpAddr = row.try_get("source")?;
        daily.push(created_at);
        if hash.as_ref() == mobile.as_ref() {
            mobile_daily.push(created_at);
            if created_at > now - TimeDelta::hours(1) {
                mobile_hourly.push(created_at);
            }
        }
        if origin == source {
            if created_at > now - TimeDelta::minutes(1) {
                source_minute.push(created_at);
            }
            if created_at > now - TimeDelta::hours(1) {
                // Ordered rows leave each mobile's latest permit, not its first one.
                source_hourly.insert(hash.as_ref().to_vec(), created_at);
            }
        }
    }
    let mut retry_at = now;
    if let Some(created_at) = mobile_daily.last() {
        retry_at = retry_at.max(*created_at + limits.mobile().interval());
    }
    capacity(&daily, limits.daily(), TimeDelta::hours(24), &mut retry_at);
    capacity(
        &mobile_daily,
        limits.mobile().daily(),
        TimeDelta::hours(24),
        &mut retry_at,
    );
    capacity(
        &mobile_hourly,
        limits.mobile().hourly(),
        TimeDelta::hours(1),
        &mut retry_at,
    );
    capacity(
        &source_minute,
        limits.source().minute(),
        TimeDelta::minutes(1),
        &mut retry_at,
    );
    if !source_hourly.contains_key(mobile.as_ref().as_slice()) {
        let mut latest: Vec<_> = source_hourly.into_values().collect();
        latest.sort_unstable();
        capacity(
            &latest,
            limits.source().hourly(),
            TimeDelta::hours(1),
            &mut retry_at,
        );
    }
    if retry_at > now {
        return Err(Error::Quota { retry_at });
    }
    Ok(())
}

fn capacity(
    permits: &[DateTime<Utc>],
    limit: usize,
    window: TimeDelta,
    retry_at: &mut DateTime<Utc>,
) {
    if permits.len() >= limit {
        *retry_at = (*retry_at).max(permits[permits.len() - limit] + window);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use tokio::runtime::Runtime;
    use uuid::Uuid;

    use super::*;
    use crate::identity::{
        authentication::{
            Input, database,
            verification::tests::{Database, keys, server},
        },
        message::{self, limits},
    };

    async fn permit(
        database: &Database,
        ordinal: i64,
        created_at: DateTime<Utc>,
        mobile: &str,
        source: &str,
    ) {
        let session = database
            .connections(&database.user)
            .recover()
            .await
            .expect("fixture runtime");
        let client = session.client();
        let keys = keys();
        let input = Input::new(
            Uuid::new_v4().to_string().parse().expect("operation"),
            mobile.parse().expect("mobile"),
            Uuid::new_v4().to_string().parse().expect("fictional token"),
            source.parse().expect("source"),
        );
        client.batch_execute("BEGIN").await.expect("begin fixture");
        let challenge = database::challenge::insert(client, &keys, &input, created_at)
            .await
            .expect("challenge fixture");
        client.execute("UPDATE identity.challenges SET status='passed',solved_at=$2,finished_at=$2 WHERE id=$1",&[&challenge.id,&created_at]).await.expect("passed fixture");
        let message_id = message::Id::new();
        let expires_at = created_at + TimeDelta::seconds(300);
        let worker_id = Uuid::new_v4();
        let key = keys.message.active();
        let proof = key
            .prove(message_id, &"00001234".parse().expect("fictional code"))
            .expect("proof");
        client.execute("INSERT INTO identity.messages(id,challenge_id,ordinal,worker_id,created_at,expires_at) VALUES($1,$2,$3,$4,$5,$6)",&[&message_id,&challenge.id,&ordinal,&worker_id,&created_at,&expires_at]).await.expect("permit fixture");
        client
            .execute(
                "INSERT INTO identity.checks(id,hash,key_id,expires_at) VALUES($1,$2,$3,$4)",
                &[&message_id, &proof, &key.id(), &expires_at],
            )
            .await
            .expect("check fixture");
        client
            .batch_execute("COMMIT")
            .await
            .expect("paired fixture");
        session.terminate().await.expect("end fixture");
    }

    #[test]
    fn rolling_windows_use_the_last_distinct_mobile_permit_and_latest_blocker() {
        let server = server();
        Runtime::new().expect("runtime").block_on(async {
            for case in ["mobile","source","minute"] {
                let database=Database::new(Arc::clone(&server)).await;
                let now=database.now().await;
                let limits=Limits::new(300,100,limits::Mobile::new(60,2,3).expect("mobile limits"),limits::Source::new(2,if case=="minute" {10} else {2}).expect("source limits")).expect("limits");
                let records=match case {
                    "mobile"=>vec![(3601,"13800000000"),(300,"13800000000"),(120,"13800000000")],
                    "source"=>vec![(3500,"13800000000"),(3000,"13900000000"),(120,"13800000000")],
                    _=>vec![(59,"13800000000"),(1,"13900000000")],
                };
                for (index,(age,mobile)) in records.into_iter().enumerate() {
                    permit(&database,i64::try_from(index+1).expect("ordinal"),now-TimeDelta::seconds(age),mobile,"192.0.2.1").await;
                }
                let reader=database.connections(&database.user).recover().await.expect("reader");
                let keys=keys();
                let mobile=if case=="mobile" {"13800000000"} else {"13700000000"};
                let hash=keys.mobile.hash(&mobile.parse().expect("mobile")).expect("hash");
                let expected=match case {"mobile"=>82799,"source"=>600,_=>1};
                assert!(matches!(verify(reader.client(),&limits,&hash,"192.0.2.1".parse().expect("source"),now).await,Err(Error::Quota{retry_at}) if retry_at==now+TimeDelta::seconds(expected)));
                if case=="source" {
                    let existing=keys.mobile.hash(&"13800000000".parse().expect("mobile")).expect("hash");
                    // Reusing an occupied distinct-mobile slot does not require another slot.
                    let limits=Limits::new(300,100,limits::Mobile::default(),limits::Source::new(3,2).expect("source limits")).expect("limits");
                    verify(reader.client(),&limits,&existing,"192.0.2.1".parse().expect("source"),now).await.expect("same slot");
                    let other=keys.mobile.hash(&"13700000000".parse().expect("mobile")).expect("hash");
                    verify(reader.client(),&limits,&other,"192.0.2.1".parse().expect("source"),now+TimeDelta::seconds(600)).await.expect("strict hourly edge");
                }
                reader.terminate().await.expect("end reader");
            }
        });
    }
}

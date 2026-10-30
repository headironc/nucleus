//! The current role definition and its permanent deletion tombstone.

use chrono::{DateTime, Utc};
use tokio_postgres::{Client, Row};
use zeroize::Zeroizing;

use crate::{
    access::{
        capability::Scope,
        role::{Code, Id, Kind, Role, change},
    },
    history::{Error, Keys, State as HistoryState, Token, Verified},
    keys::seal::Seal,
};

pub(crate) const TABLE: &str = "access.roles";

#[derive(Clone)]
pub(in crate::access) struct State {
    role: Role,
    scope: Scope,
    active: bool,
    changed_at: DateTime<Utc>,
    sequence: i64,
    version: i64,
}

pub(in crate::access) struct Data {
    pub(super) role: Role,
    pub(super) scope: Scope,
    pub(super) active: bool,
    pub(super) changed_at: DateTime<Utc>,
}

pub(super) fn check_collection(states: &[Verified<State>]) -> Result<(), super::Error> {
    let mut names = std::collections::HashSet::new();
    for state in states {
        if state.active() && !names.insert((state.scope(), state.role().name().0.as_str())) {
            return Err(super::Error::Facts {
                rule: "live role names are unique within their scope",
            });
        }
    }
    if !states.is_empty()
        && super::Builtin::ALL.iter().any(|system| {
            states
                .iter()
                .filter(|state| {
                    state.active()
                        && state.scope() == system.scope()
                        && state.role().level().0 == system.level()
                })
                .count()
                != 1
        })
    {
        return Err(super::Error::Facts {
            rule: "the four immutable system roles remain present and unique",
        });
    }
    Ok(())
}

impl State {
    pub(super) fn role(&self) -> &Role {
        &self.role
    }

    pub(super) fn scope(&self) -> Scope {
        self.scope
    }

    pub(super) fn active(&self) -> bool {
        self.active
    }

    pub(super) fn changed_at(&self) -> DateTime<Utc> {
        self.changed_at
    }
}

impl crate::history::Kind for State {
    type Error = crate::history::Error;
    const NAME: &'static str = "access.role";
    const SELECT: &'static str = "SELECT state.id AS role_id, state.scope, state.name, state.level, state.permissions, state.active, state.change_id, state.changed_at, state.sequence AS role_sequence, state.version AS role_version, state.seal, state.seal_key_id, state.seal_version FROM access.roles state";

    fn decode(_: Token, row: &Row, _: Keys<'_>, now: DateTime<Utc>) -> Result<Self, Error> {
        let scope = row.try_get("scope").map_err(Error::decode)?;
        let name = row.try_get("name").map_err(Error::decode)?;
        let level = row.try_get("level").map_err(Error::decode)?;
        let permissions: Vec<Code> = row.try_get("permissions").map_err(Error::decode)?;
        let kind = Kind::new(scope, &name, level, &permissions).map_err(|_| {
            Error::Integrity("role scope, level, system definition and permissions agree")
        })?;
        if permissions
            .windows(2)
            .any(|pair| pair[0].as_ref() >= pair[1].as_ref())
        {
            return Err(Error::Integrity(
                "role permissions have canonical code order",
            ));
        }
        let active: bool = row.try_get("active").map_err(Error::decode)?;
        let changed_at = row.try_get("changed_at").map_err(Error::decode)?;
        if changed_at > now || (!active && (kind != Kind::Custom || !permissions.is_empty())) {
            return Err(Error::Integrity(
                "a role has a non-future definition and only a custom role can become a tombstone",
            ));
        }
        Ok(Self {
            role: Role {
                id: row.try_get("role_id").map_err(Error::decode)?,
                name,
                level,
                kind,
                permissions,
                change_id: row.try_get("change_id").map_err(Error::decode)?,
            },
            scope,
            active,
            changed_at,
            sequence: row.try_get("role_sequence").map_err(Error::decode)?,
            version: row.try_get("role_version").map_err(Error::decode)?,
        })
    }

    fn fields(&self) -> Zeroizing<Vec<Vec<u8>>> {
        let mut fields = vec![
            self.role.id.as_bytes().to_vec(),
            self.scope.as_ref().as_bytes().to_vec(),
            self.role.name.0.as_bytes().to_vec(),
            self.role.level.0.to_string().into_bytes(),
            if self.active {
                b"active".to_vec()
            } else {
                b"deleted".to_vec()
            },
            self.role.change_id.as_bytes().to_vec(),
            self.changed_at.timestamp_micros().to_string().into_bytes(),
            self.sequence.to_string().into_bytes(),
            self.version.to_string().into_bytes(),
            self.role.permissions.len().to_string().into_bytes(),
        ];
        fields.extend(
            self.role
                .permissions
                .iter()
                .map(|code| code.as_ref().as_bytes().to_vec()),
        );
        Zeroizing::new(fields)
    }
}

impl HistoryState for State {
    type Id = Id;
    type Data = Data;
    type Record = change::Record;

    fn build(_: Token, data: Data, sequence: i64, version: i64) -> Self {
        Self {
            role: data.role,
            scope: data.scope,
            active: data.active,
            changed_at: data.changed_at,
            sequence,
            version,
        }
    }

    fn id(&self) -> Id {
        self.role.id
    }
    fn bytes(id: Id) -> [u8; 16] {
        *id.as_bytes()
    }
    fn sequence(&self) -> i64 {
        self.sequence
    }
    fn version(&self) -> i64 {
        self.version
    }

    fn has_head(&self, record: &change::Record) -> bool {
        record.matches(&self.role, self.scope, self.active, self.changed_at)
    }

    async fn write(
        &self,
        _: Token,
        client: &Client,
        seal: &Seal,
        original: Option<&Verified<Self>>,
    ) -> Result<u64, Error> {
        let permissions: Vec<&str> = self.role.permissions.iter().map(AsRef::as_ref).collect();
        Ok(match original {
            None => client.execute("INSERT INTO access.roles (id,scope,name,level,permissions,active,change_id,changed_at,sequence,version,seal,seal_key_id,seal_version) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)", &[&self.role.id, &self.scope.as_ref(), &self.role.name, &self.role.level, &permissions, &self.active, &self.role.change_id, &self.changed_at, &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version()]).await?,
            Some(original) => client.execute("UPDATE access.roles SET name=$3,level=$4,permissions=$5,active=$6,change_id=$7,changed_at=$8,sequence=$9,version=$10,seal=$11,seal_key_id=$12,seal_version=$13 WHERE id=$1 AND scope=$2 AND version=$14 AND seal=$15 AND seal_key_id=$16 AND seal_version=$17", &[&self.role.id, &self.scope.as_ref(), &self.role.name, &self.role.level, &permissions, &self.active, &self.role.change_id, &self.changed_at, &self.sequence, &self.version, &seal.tag(), &seal.key_id(), &seal.version(), &original.version(), &original.seal().tag(), &original.seal().key_id(), &original.seal().version()]).await?,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{sync::Arc, time::Duration};

    use chrono::DateTime;
    use platform::cryptography::sha256;
    use serde_json::json;
    use tokio::{runtime::Runtime, time::timeout};
    use uuid::Uuid;
    use zeroize::Zeroizing;

    use super::{Data, State};
    use crate::{
        access::{
            capability::Scope,
            role::{
                self, Code, Kind, Level, Role,
                tests::{actor, start},
            },
        },
        history::{self, Kind as _, Seed},
        identity::authentication::{self, Database, Supplier, server},
        kernel::{self, FaultKind, Stage},
        keys::{self, Derived, Entry, Material, seal},
        pagination::Pagination,
    };

    const FACTS: &str = "SELECT (SELECT jsonb_agg(to_jsonb(s) ORDER BY id) FROM access.roles s)::text, (SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM access.changes r)::text, (SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY family,operation_id),'[]'::jsonb) FROM access.completions c)::text";

    #[test]
    fn current_definitions_reject_authentic_old_states_and_missing_states() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for scope in [Scope::Institution, Scope::Workforce] {
                for deleted in [false, true] {
                    for missing in [false, true] {
                        timeout(Duration::from_secs(30), Box::pin(async {
                            let database = Database::initialized(Arc::clone(&server)).await;
                            let mut supplier = Supplier::new().await;
                            let (context, services) = start(database.connections(&database.user), &supplier).await;
                            let actor = actor(&database, &services, &mut supplier).await;
                            let original = Uuid::new_v4().to_string().parse().expect("operation");
                            let created = match scope {
                                Scope::Institution => services.role().create(actor.clone(), original, serde_json::from_value(json!({"name":"Original role","level":200,"permissions":[]})).expect("institution input")).await,
                                Scope::Workforce => services.role().create_workforce(actor.clone(), original, serde_json::from_value(json!({"entry":"institution","name":"Original role","level":200,"permissions":[]})).expect("workforce input")).await,
                            }.expect("real sealed creation");
                            let listed = services.role().fetch_list(&actor, Pagination::new(1,100).expect("page"), scope).await.expect("original state authenticates");
                            assert!(listed.roles().iter().any(|role| role.id() == created.id() && role.change_id() == created.change_id()));
                            let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                            owner.client().batch_execute("CREATE TEMP TABLE original_role AS TABLE access.roles").await.expect("complete authentic earlier state");
                            let old: String = owner.client().query_one("SELECT to_jsonb(s)::text FROM original_role s WHERE id=$1", &[&created.id()]).await.expect("original row").get(0);
                            let next = Uuid::new_v4().to_string().parse().expect("next operation");
                            if deleted {
                                match scope {
                                    Scope::Institution => services.role().delete(actor.clone(),next,created.id(),role::Delete::new(created.change_id())).await,
                                    Scope::Workforce => services.role().delete_workforce(actor.clone(),next,created.id(),serde_json::from_value(json!({"entry":"institution","changeId":created.change_id()})).expect("workforce deletion")).await,
                                }.expect("real deletion retains a sealed tombstone");
                            } else {
                                match scope {
                                    Scope::Institution => services.role().update(actor.clone(),next,created.id(),role::Update::new(created.change_id(),serde_json::from_value(json!({"name":"Updated role","level":201,"permissions":[]})).expect("updated definition"))).await,
                                    Scope::Workforce => services.role().update_workforce(actor.clone(),next,created.id(),serde_json::from_value(json!({"entry":"institution","changeId":created.change_id(),"name":"Updated role","level":201,"permissions":[]})).expect("workforce update")).await,
                                }.expect("real modification advances the anchor");
                            }
                            let family = if scope == Scope::Institution {"institution_role"} else {"workforce_role"};
                            let current = owner.client().query_one("SELECT active,sequence,version,(SELECT count(*) FROM access.changes WHERE role_id=$1),(SELECT count(*) FROM access.completions WHERE operation_id IN ($2,$3) AND family=$4),(SELECT bool_and(expires_at IS NULL) FROM access.completions WHERE operation_id IN ($2,$3) AND family=$4) FROM access.roles WHERE id=$1", &[&created.id(),&original,&next,&family]).await.expect("independent state, record and permanent completion facts");
                            assert_eq!(current.get::<_,bool>(0), !deleted);
                            for index in 1_usize..=4 { assert_eq!(current.get::<_,i64>(index),2); }
                            assert!(current.get::<_,bool>(5));
                            let row = owner.client().query_one(FACTS,&[]).await.expect("healthy facts");
                            let healthy: [String;3] = std::array::from_fn(|index| row.get(index));
                            owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("damage only this disposable database");
                            let changed = if missing {
                                owner.client().execute("DELETE FROM access.roles WHERE id=$1", &[&created.id()]).await
                            } else {
                                // Restore every column with its authentic old seal; never sign the rollback again.
                                owner.client().execute("UPDATE access.roles current SET scope=old.scope,name=old.name,level=old.level,permissions=old.permissions,active=old.active,change_id=old.change_id,changed_at=old.changed_at,sequence=old.sequence,version=old.version,seal=old.seal,seal_key_id=old.seal_key_id,seal_version=old.seal_version FROM original_role old WHERE current.id=old.id AND current.id=$1", &[&created.id()]).await
                            };
                            assert_eq!(changed.expect("selected state damage"),1);
                            owner.client().batch_execute("COMMIT").await.expect("damage visible online");
                            if missing {
                                assert!(owner.client().query_opt("SELECT id FROM access.roles WHERE id=$1", &[&created.id()]).await.expect("state absence").is_none());
                            } else {
                                assert_eq!(owner.client().query_one("SELECT to_jsonb(s)::text FROM access.roles s WHERE id=$1", &[&created.id()]).await.expect("restored authentic state").get::<_,String>(0),old);
                            }
                            let row = owner.client().query_one(FACTS,&[]).await.expect("damaged facts");
                            let damaged: [String;3] = std::array::from_fn(|index| row.get(index));
                            assert_ne!(damaged[0],healthy[0]);
                            assert_eq!(damaged[1],healthy[1],"state damage leaves every Record intact");
                            assert_eq!(damaged[2],healthy[2],"state damage leaves every Completion intact");
                            let error = services.role().fetch_list(&actor, Pagination::new(1,100).expect("page"), scope).await.err().expect("anchored definitions reject old or missing state");
                            assert!(matches!(&error,kernel::Error::Corrupt(fault) if fault.stage()==Stage::Read && matches!(fault.kind(),FaultKind::History(history::Error::Integrity("snapshot state is not older than its anchor view")))),"{scope:?}, deleted={deleted}, missing={missing}: {error:?}");
                            assert!(services.ready().await.is_err(),"anchor damage closes admission");
                            let closed = services.role().fetch_list(&actor, Pagination::new(1,100).expect("page"), scope).await.err().expect("later admission is closed");
                            assert!(closed.is_closed(),"{closed:?}");
                            let row = owner.client().query_one(FACTS,&[]).await.expect("rejection writes no facts");
                            let after: [String;3] = std::array::from_fn(|index| row.get(index));
                            assert_eq!(after,damaged);
                            services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                            owner.terminate().await.expect("owner ended"); supplier.stop().await;
                        })).await.unwrap_or_else(|error| panic!("{scope:?}, deleted={deleted}, missing={missing}: {error}"));
                    }
                }
            }
        }));
    }

    #[test]
    fn role_completion_damage_rejects_replay_and_missing_completions_reject_startup() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for scope in [Scope::Institution, Scope::Workforce] {
                for damage in ["seal", "input", "missing"] {
                    timeout(Duration::from_secs(30), Box::pin(async {
                        let database = Database::initialized(Arc::clone(&server)).await;
                        let mut supplier = Supplier::new().await;
                        let (context, services) = start(database.connections(&database.user), &supplier).await;
                        let actor = actor(&database, &services, &mut supplier).await;
                        let original = Uuid::new_v4().to_string().parse().expect("operation");
                        let input: role::Input = serde_json::from_value(json!({"name":"Permanent completion","level":200,"permissions":[]})).expect("institution input");
                        let workforce: role::workforce::Input = serde_json::from_value(json!({"entry":"institution","name":"Permanent completion","level":200,"permissions":[]})).expect("workforce input");
                        let created = match scope {
                            Scope::Institution => services.role().create(actor.clone(),original,input.clone()).await,
                            Scope::Workforce => services.role().create_workforce(actor.clone(),original,workforce.clone()).await,
                        }.expect("real sealed role and permanent completion");
                        let replay = match scope {
                            Scope::Institution => services.role().create(actor.clone(),original,input.clone()).await,
                            Scope::Workforce => services.role().create_workforce(actor.clone(),original,workforce.clone()).await,
                        }.expect("healthy original request replays");
                        assert_eq!(replay.id(),created.id()); assert_eq!(replay.change_id(),created.change_id());
                        let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                        let family = if scope == Scope::Institution {"institution_role"} else {"workforce_role"};
                        let completion = owner.client().query_one("SELECT expires_at IS NULL,(SELECT count(*) FROM access.changes WHERE role_id=$2),(SELECT count(*) FROM access.completions WHERE family=$3 AND operation_id=$1) FROM access.completions WHERE family=$3 AND operation_id=$1", &[&original,&created.id(),&family]).await.expect("independent retained completion");
                        assert!(completion.get::<_,bool>(0));
                        assert_eq!(completion.get::<_,i64>(1),1); assert_eq!(completion.get::<_,i64>(2),1);
                        let row = owner.client().query_one(FACTS,&[]).await.expect("healthy facts");
                        let healthy: [String;3] = std::array::from_fn(|index| row.get(index));
                        owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("isolated replica damage");
                        let statement = match damage {
                            "seal" => "UPDATE access.completions SET seal=set_byte(seal,0,get_byte(seal,0) # 1) WHERE family=$2 AND operation_id=$1",
                            "input" => "UPDATE access.completions SET input=set_byte(input,38,get_byte(input,38) # 1) WHERE family=$2 AND operation_id=$1",
                            "missing" => "DELETE FROM access.completions WHERE family=$2 AND operation_id=$1",
                            _ => unreachable!("closed damage cases"),
                        };
                        assert_eq!(owner.client().execute(statement,&[&original,&family]).await.expect("completion damage without resigning"),1);
                        owner.client().batch_execute("COMMIT").await.expect("damage committed");
                        let row = owner.client().query_one(FACTS,&[]).await.expect("damaged facts");
                        let damaged: [String;3] = std::array::from_fn(|index| row.get(index));
                        assert_eq!(damaged[0],healthy[0],"completion damage leaves State intact");
                        assert_eq!(damaged[1],healthy[1],"completion damage leaves Record intact");
                        assert_ne!(damaged[2],healthy[2]);
                        let listed = services.role().fetch_list(&actor,Pagination::new(1,100).expect("page"),scope).await.expect("current definitions independently authenticate intact State");
                        assert!(listed.roles().iter().any(|role| role.id()==created.id() && role.change_id()==created.change_id()));
                        services.ready().await.expect("an unread completion fault has not closed admission");
                        if damage != "missing" {
                            let error = match scope {
                                Scope::Institution => services.role().create(actor.clone(),original,input).await,
                                Scope::Workforce => services.role().create_workforce(actor.clone(),original,workforce).await,
                            }.err().expect("same original request authenticates its completion");
                            assert!(matches!(&error,kernel::Error::Corrupt(fault) if fault.stage()==Stage::Completion && match damage {
                                "seal" => matches!(fault.kind(),FaultKind::History(history::Error::Seal(seal::Error::Mismatch))),
                                "input" => matches!(fault.kind(),FaultKind::History(history::Error::Encryption(kernel::keys::Error::Envelope(platform::cryptography::envelope::Error::Authentication(_))))),
                                _ => false,
                            }),"{scope:?}, {damage}: {error:?}");
                            assert!(services.ready().await.is_err(),"completion corruption closes admission");
                            let closed = services.role().fetch_list(&actor,Pagination::new(1,100).expect("page"),scope).await.err().expect("later admission closed");
                            assert!(closed.is_closed(),"{closed:?}");
                        }
                        services.close(); services.drain().await.expect("drained"); context.stop().await.expect("stopped");
                        if damage == "missing" {
                            let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                            let result = Box::pin(crate::startup::Services::start(
                                crate::database::Barrier::begin(context.clone()).await.expect("restart barrier"),
                                authentication::inventory(authentication::keys()),
                                crate::startup::Settings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("settings"),
                                supplier.client(),authentication::unused_client().await,
                            )).await;
                            let error = match result {
                                Ok(services) => {
                                    services.close(); services.drain().await.expect("unexpected startup drained");
                                    context.stop().await.expect("unexpected startup stopped");
                                    panic!("{scope:?}: startup accepted a missing permanent completion");
                                }
                                Err(error) => error,
                            };
                            assert!(matches!(&error,crate::startup::Error::Role(kernel::Error::Corrupt(fault)) if fault.stage()==Stage::Command && matches!(fault.kind(),FaultKind::Integrity("every permanent online role change retains its completion"))),"{scope:?}: {error:?}");
                            context.stop().await.expect("failed startup stopped");
                        }
                        let row = owner.client().query_one(FACTS,&[]).await.expect("replay or startup rejection writes no facts");
                        let after: [String;3] = std::array::from_fn(|index| row.get(index));
                        assert_eq!(after,damaged);
                        owner.terminate().await.expect("owner ended"); supplier.stop().await;
                    })).await.unwrap_or_else(|error| panic!("{scope:?}, {damage}: {error}"));
                }
            }
        }));
    }

    #[test]
    fn startup_rejects_damaged_system_role_definitions_and_original_records() {
        let server = server();
        Runtime::new().expect("runtime").block_on(Box::pin(async {
            for damage in ["system_permissions", "missing_default", "record_time"] {
                timeout(Duration::from_secs(30), Box::pin(async {
                    let database = Database::initialized(Arc::clone(&server)).await;
                    let supplier = Supplier::new().await;
                    let (context, services) = start(database.connections(&database.user), &supplier).await;
                    services.ready().await.expect("complete initialization starts normally");
                    services.close(); services.drain().await.expect("original services drained");
                    context.stop().await.expect("original context stopped before damage");
                    let owner = database.connections("postgres").recover().await.expect("isolated damage owner");
                    let counts = owner.client().query_one("SELECT (SELECT count(*) FROM access.roles),(SELECT count(*) FROM access.changes),(SELECT count(*) FROM access.completions)", &[]).await.expect("complete initialization facts");
                    assert_eq!(counts.get::<_,i64>(0),4); assert_eq!(counts.get::<_,i64>(1),4); assert_eq!(counts.get::<_,i64>(2),1);
                    let row = owner.client().query_one(FACTS,&[]).await.expect("healthy retained facts");
                    let healthy: [String;3] = std::array::from_fn(|index| row.get(index));
                    owner.client().batch_execute("BEGIN; SET LOCAL session_replication_role=replica").await.expect("damage only this disposable database");
                    match damage {
                        "system_permissions" => {
                            assert_eq!(owner.client().execute("UPDATE access.roles SET permissions=array_remove(permissions,'institution.roles') WHERE 'institution.roles'=ANY(permissions)", &[]).await.expect("remove actual State permission"),1);
                            assert_eq!(owner.client().execute("UPDATE access.changes SET permissions=array_remove(permissions,'institution.roles') WHERE 'institution.roles'=ANY(permissions)", &[]).await.expect("remove actual Record permission"),1);
                        }
                        "missing_default" => {
                            assert_eq!(owner.client().execute("DELETE FROM access.roles WHERE scope='workforce' AND level=1", &[]).await.expect("remove default State but retain its Record"),1);
                        }
                        "record_time" => {
                            assert_eq!(owner.client().execute("UPDATE access.changes SET changed_at=changed_at+interval '1 hour'", &[]).await.expect("rewrite original Record times without resigning"),4);
                            assert!(owner.client().query_one("SELECT bool_and(changed_at <= clock_timestamp()) FROM access.changes", &[]).await.expect("historical initialization remains non-future").get::<_,bool>(0),"the rewritten valid timestamp reaches seal verification");
                        }
                        _ => unreachable!("closed startup damage cases"),
                    }
                    owner.client().batch_execute("COMMIT").await.expect("independent damage committed");
                    let row = owner.client().query_one(FACTS,&[]).await.expect("damaged retained facts");
                    let damaged: [String;3] = std::array::from_fn(|index| row.get(index));
                    assert_eq!(damaged[2],healthy[2],"none of these cases changes Completion");
                    match damage {
                        "system_permissions" => {
                            assert_ne!(damaged[0],healthy[0]); assert_ne!(damaged[1],healthy[1]);
                        }
                        "missing_default" => {
                            assert_ne!(damaged[0],healthy[0]); assert_eq!(damaged[1],healthy[1],"original Record remains retained");
                        }
                        "record_time" => {
                            assert_eq!(damaged[0],healthy[0],"State stays untouched"); assert_ne!(damaged[1],healthy[1]);
                        }
                        _ => unreachable!("closed startup damage cases"),
                    }
                    let context = crate::runtime::Context::new(database.connections(&database.user)).await.expect("restart context");
                    let result = Box::pin(crate::startup::Services::start(
                        crate::database::Barrier::begin(context.clone()).await.expect("restart barrier"),
                        authentication::inventory(authentication::keys()),
                        crate::startup::Settings::new(4,crate::identity::message::Limits::default(),authentication::session_settings(),crate::registration::Limits::default(),authentication::avatar_settings()).expect("settings"),
                        supplier.client(),authentication::unused_client().await,
                    )).await;
                    let error = match result {
                        Ok(services) => {
                            services.close(); services.drain().await.expect("unexpected startup drained");
                            context.stop().await.expect("unexpected startup stopped");
                            panic!("{damage}: startup opened services with damaged Role facts");
                        }
                        Err(error) => error,
                    };
                    assert!(matches!(&error,crate::startup::Error::Role(kernel::Error::Corrupt(fault)) if fault.stage()==Stage::Command && match damage {
                        "system_permissions" => matches!(fault.kind(),FaultKind::History(history::Error::Integrity("role scope, level, system definition and permissions agree"))),
                        "missing_default" => matches!(fault.kind(),FaultKind::History(history::Error::Integrity("every record belongs to a retained state"))),
                        "record_time" => matches!(fault.kind(),FaultKind::History(history::Error::Seal(seal::Error::Mismatch))),
                        _ => false,
                    }),"{damage}: {error:?}");
                    context.stop().await.expect("failed startup context stopped");
                    let row = owner.client().query_one(FACTS,&[]).await.expect("failed startup never supplements retained facts");
                    let after: [String;3] = std::array::from_fn(|index| row.get(index));
                    assert_eq!(after,damaged,"{damage}: State, Record and Completion remain unchanged");
                    owner.terminate().await.expect("fixture connection ended"); supplier.stop().await;
                })).await.unwrap_or_else(|error| panic!("startup Role case {damage}: {error}"));
            }
        }));
    }

    #[test]
    fn role_encoding_matches_independent_python_vectors() {
        let key_id = keys::Id::new(0).expect("key");
        let seals = seal::Keys::new(Derived::fixed(
            key_id,
            vec![Entry::new(
                key_id,
                Material(Zeroizing::new(std::array::from_fn(|index| {
                    u8::try_from(index).expect("32 bytes")
                }))),
            )],
        ));
        let inventory = authentication::inventory(authentication::keys());
        let keys = history::Keys {
            data: &inventory.encryption,
            seals: &seals,
        };
        let active = Seed::state::<State>(
            Data {
                role: Role {
                    id: "00000000-0000-4000-8000-000000000001"
                        .parse()
                        .expect("role"),
                    name: "审阅 Café".parse().expect("NFC name"),
                    level: Level(237),
                    kind: Kind::Custom,
                    permissions: vec![Code::WorkforceDirectory, Code::WorkforceNumbers],
                    change_id: serde_json::from_str(r#""00000000-0000-4000-8000-000000000002""#)
                        .expect("change"),
                },
                scope: Scope::Workforce,
                active: true,
                changed_at: DateTime::from_timestamp_micros(1_700_000_000_123_456)
                    .expect("microseconds"),
            },
            12,
            14,
            keys,
        )
        .expect("active state");
        let tombstone = Seed::state::<State>(
            Data {
                role: Role {
                    id: "00000000-0000-4000-8000-000000000001"
                        .parse()
                        .expect("role"),
                    name: "已删除 Café".parse().expect("NFC name"),
                    level: Level(456),
                    kind: Kind::Custom,
                    permissions: Vec::new(),
                    change_id: serde_json::from_str(r#""00000000-0000-4000-8000-000000000002""#)
                        .expect("change"),
                },
                scope: Scope::Institution,
                active: false,
                changed_at: DateTime::from_timestamp_micros(1_700_000_000_123_456)
                    .expect("microseconds"),
            },
            13,
            15,
            keys,
        )
        .expect("tombstone");
        // Python struct.pack(">I"), hashlib.sha256 and hmac with the fixed seal-use key 0..31.
        for (state, count, length, digest, tag) in [
            (
                active,
                12,
                206,
                "9575a8596586d47037b667afd34ac70590d1cfc2c8dc30817e2460d305cce4b7",
                "ebbb7777d59160af9ade496fecc5461407612ae1a53528ecff6089acbd5cef29",
            ),
            (
                tombstone,
                10,
                168,
                "dcc88878eaad7c58e44e4dc3a5653bb9018882e9fc34492a1b4535e66d9af436",
                "d5e2a5872fb8f61c6cf4621b397e26e14fa1aca8f2ee8f352f928bad21562b9c",
            ),
        ] {
            let fields = state.fields();
            assert_eq!(fields.len(), count);
            let encoded = seal::encode_fields(
                State::NAME,
                State::VERSION,
                &fields.iter().map(Vec::as_slice).collect::<Vec<_>>(),
            )
            .expect("encoding");
            assert_eq!(encoded.as_ref().len(), length);
            assert_eq!(hex::encode(sha256::Digest::new(&encoded).as_ref()), digest);
            assert_eq!(hex::encode(state.seal().tag()), tag);
        }
    }
}

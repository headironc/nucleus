//! The actual printable content and explicit self-history projections.

use chrono::{DateTime, Utc};
use postgres_types::{FromSql, ToSql};
use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{
        RefOr, Schema,
        schema::{AllOfBuilder, ObjectBuilder, OneOfBuilder, Type},
    },
};
use uuid::Uuid;

use crate::{
    identity::person,
    pagination::Pagination,
    workforce::{
        directory::{department, roster, title},
        number::Number,
    },
};

macro_rules! identifier {
    ($name:ident, $schema:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, FromSql, ToSql, ToSchema,
        )]
        #[serde(transparent)]
        #[postgres(transparent)]
        #[schema(as = application::$schema)]
        pub struct $name(Uuid);
        impl $name {
            pub(super) fn allocate() -> Self {
                Self(Uuid::new_v4())
            }
        }
        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

identifier!(Id, Id);
identifier!(PhotoId, PhotoId);
identifier!(ResourceId, ResourceId);
identifier!(BadgeId, BadgeId);

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = application::Badge)]
pub struct Badge {
    pub(super) id: BadgeId,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = application::RenderError)]
pub enum RenderError {
    MissingGlyph,
    TextOverflow,
    DependencyUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = application::Kind)]
pub enum Kind {
    Issuance,
    Duplication,
    Replacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = application::Status)]
pub enum Status {
    Submitted,
    Approved,
    Ready,
    Rejected,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = application::Approval)]
pub struct Approval {
    pub(super) reviewer_id: person::Id,
    pub(super) reviewer_name: String,
    pub(super) reviewed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
#[schema(as = application::Decision)]
pub enum Decision {
    Submitted,
    Approved {
        #[serde(flatten)]
        approval: Approval,
        #[serde(rename = "renderError", skip_serializing_if = "Option::is_none")]
        #[schema(value_type = RenderError, required = false, nullable = false)]
        render_error: Option<RenderError>,
    },
    Ready {
        #[serde(flatten)]
        approval: Approval,
        badge: Badge,
    },
    Rejected {
        #[serde(rename = "reviewerId")]
        reviewer_id: person::Id,
        #[serde(rename = "reviewerName")]
        reviewer_name: String,
        #[serde(rename = "reviewedAt")]
        reviewed_at: DateTime<Utc>,
        #[serde(rename = "rejectionReason")]
        rejection_reason: Reason,
    },
    Cancelled {
        #[serde(flatten)]
        #[schema(schema_with = cancelled_approval)]
        approval: Option<Approval>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[schema(value_type = Badge, required = false, nullable = false)]
        badge: Option<Badge>,
    },
}

fn cancelled_approval() -> RefOr<Schema> {
    // A cancelled application has either the complete prior approval or none of its fields.
    // Utoipa's automatic optional flatten would require the approval even when Serde omits it.
    // Incompatible types forbid any supplied value in the branch without approval fields.
    let absent: RefOr<Schema> = AllOfBuilder::new()
        .item(ObjectBuilder::new().schema_type(Type::String))
        .item(ObjectBuilder::new().schema_type(Type::Null))
        .into();
    OneOfBuilder::new()
        .item(
            ObjectBuilder::new()
                .property("reviewerId", absent.clone())
                .property("reviewerName", absent.clone())
                .property("reviewedAt", absent),
        )
        .item(Approval::schema())
        .into()
}

#[derive(Debug, Clone, Serialize, ToSchema, ToSql)]
#[serde(transparent)]
#[postgres(transparent)]
#[schema(as = application::Reason, value_type = String, min_length = 1)]
pub struct Reason(String);

impl<'de> Deserialize<'de> for Reason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for Reason {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let value = value.trim();
        if value.is_empty() || value.contains('\0') {
            return Err("a nonempty reason without NUL is required");
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = application::ReviewInput)]
pub enum ReviewInput {
    Approve {},
    Reject { reason: Reason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = application::CancellableStatus)]
pub enum CancellableStatus {
    Submitted,
    Approved,
    Ready,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = application::CancellationInput)]
pub enum CancellationInput {
    #[serde(rename = "self")]
    #[schema(max_properties = 2)]
    SelfService {
        #[serde(rename = "expectedStatus")]
        expected_status: CancellableStatus,
    },
    #[schema(max_properties = 3)]
    Manager {
        #[serde(rename = "expectedStatus")]
        expected_status: CancellableStatus,
        reason: Reason,
    },
}

#[derive(Clone, Copy, Deserialize)]
#[serde(try_from = "ManagementInput")]
pub struct ManagementQuery {
    pub(super) pagination: Pagination,
    pub(super) status: Status,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ManagementInput {
    page: Option<String>,
    size: Option<String>,
    status: Option<Status>,
}

impl TryFrom<ManagementInput> for ManagementQuery {
    type Error = &'static str;
    fn try_from(input: ManagementInput) -> Result<Self, Self::Error> {
        let page = match input.page {
            Some(value) => match value.parse() {
                Ok(page) => page,
                Err(_) => return Err("invalid page"),
            },
            None => 1,
        };
        let size = match input.size {
            Some(value) => match value.parse() {
                Ok(size) => size,
                Err(_) => return Err("invalid size"),
            },
            None => 20,
        };
        let pagination = match Pagination::new(page, size) {
            Ok(pagination) => pagination,
            Err(_) => return Err("invalid pagination"),
        };
        Ok(Self {
            pagination,
            status: input.status.unwrap_or(Status::Submitted),
        })
    }
}

#[cfg(test)]
mod review_input_tests {
    use super::*;

    #[test]
    fn decisions_require_their_exact_fields_and_a_real_reason() {
        for value in [
            r#"{"decision":"approve"}"#,
            r#"{"decision":"reject","reason":" Correct the name "}"#,
        ] {
            assert!(serde_json::from_str::<ReviewInput>(value).is_ok());
        }
        for value in [
            r#"{"decision":"approve","reason":"unexpected"}"#,
            r#"{"decision":"reject"}"#,
            r#"{"decision":"reject","reason":null}"#,
            r#"{"decision":"reject","reason":"\u3000\n"}"#,
            r#"{"decision":"reject","reason":"\u0000"}"#,
            r#"{"decision":"rejected","reason":"text"}"#,
        ] {
            assert!(
                serde_json::from_str::<ReviewInput>(value).is_err(),
                "{value}"
            );
        }
    }
}

#[cfg(test)]
mod cancellation_tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn inputs_require_an_explicit_mode_exact_expected_state_and_manager_reason() {
        for value in [
            r#"{"mode":"self","expectedStatus":"submitted"}"#,
            r#"{"mode":"self","expectedStatus":"approved"}"#,
            r#"{"mode":"self","expectedStatus":"ready"}"#,
            r#"{"mode":"manager","expectedStatus":"submitted","reason":" Correct the application "}"#,
            r#"{"mode":"manager","expectedStatus":"approved","reason":"Duplicate"}"#,
        ] {
            assert!(
                serde_json::from_str::<CancellationInput>(value).is_ok(),
                "{value}"
            );
        }
        for value in [
            r#"{"expectedStatus":"submitted"}"#,
            r#"{"mode":"self"}"#,
            r#"{"mode":"self","expectedStatus":null}"#,
            r#"{"mode":"self","expectedStatus":"rejected"}"#,
            r#"{"mode":"self","expectedStatus":"issued"}"#,
            r#"{"mode":"self","expectedStatus":"cancelled"}"#,
            r#"{"mode":"self","expectedStatus":"submitted","reason":"Not accepted"}"#,
            r#"{"mode":"self_service","expectedStatus":"submitted"}"#,
            r#"{"mode":"manager","expectedStatus":"submitted"}"#,
            r#"{"mode":"manager","expectedStatus":"approved","reason":null}"#,
            r#"{"mode":"manager","expectedStatus":"approved","reason":"\u3000\n"}"#,
            r#"{"mode":"manager","expectedStatus":"approved","reason":"\u0000"}"#,
            r#"{"mode":"manager","expectedStatus":"approved","reason":"Duplicate","personId":"supplied"}"#,
        ] {
            assert!(
                serde_json::from_str::<CancellationInput>(value).is_err(),
                "{value}"
            );
        }
        let input = serde_json::from_str::<CancellationInput>(
            r#"{"mode":"manager","expectedStatus":"approved","reason":"  Duplicate\n "}"#,
        )
        .expect("valid manager cancellation");
        let CancellationInput::Manager { reason, .. } = input else {
            panic!("manager mode");
        };
        assert_eq!(reason.0, "Duplicate");
    }

    #[test]
    fn cancellation_omits_or_preserves_the_whole_prior_approval() {
        use serde_json::json;

        let reviewer_id: person::Id = "11111111-1111-4111-8111-111111111111"
            .parse()
            .expect("fixture person id");
        let reviewed_at = "2026-10-31T01:15:00Z"
            .parse::<DateTime<Utc>>()
            .expect("fixture approval time");
        let approval = Approval {
            reviewer_id,
            reviewer_name: "林*".to_owned(),
            reviewed_at,
        };
        let fields = json!({
            "status":"cancelled", "reviewerId":reviewer_id,
            "reviewerName":"林*", "reviewedAt":"2026-10-31T01:15:00Z"
        });
        assert_eq!(
            serde_json::to_value(Decision::Cancelled {
                approval: None,
                badge: None
            })
            .expect("unreviewed cancellation"),
            json!({"status":"cancelled"})
        );
        assert_eq!(
            serde_json::to_value(Decision::Cancelled {
                approval: Some(approval.clone()),
                badge: None
            })
            .expect("approved cancellation"),
            fields
        );
        assert_eq!(
            serde_json::to_value(Decision::Approved {
                approval,
                render_error: None
            })
            .expect("unchanged approval shape"),
            json!({
                "status":"approved", "reviewerId":reviewer_id,
                "reviewerName":"林*", "reviewedAt":"2026-10-31T01:15:00Z"
            })
        );
    }
}

/// Coordinates and dimensions are integer micrometres, relative to the top-left canvas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Rectangle)]
pub struct Rectangle {
    #[serde(rename = "xUm")]
    pub(super) x: u32,
    #[serde(rename = "yUm")]
    pub(super) y: u32,
    #[serde(rename = "widthUm")]
    pub(super) width: u32,
    #[serde(rename = "heightUm")]
    pub(super) height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::TextArea)]
pub struct TextArea {
    pub(super) area: Rectangle,
    #[serde(rename = "fontSizeUm")]
    pub(super) font_size: u32,
    #[serde(rename = "lineHeightUm")]
    pub(super) line_height: u32,
    pub(super) wrap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Layout)]
pub struct Layout {
    #[serde(rename = "widthUm")]
    pub(super) width: u32,
    #[serde(rename = "heightUm")]
    pub(super) height: u32,
    pub(super) photo: Rectangle,
    pub(super) name: TextArea,
    pub(super) number: TextArea,
    pub(super) department: TextArea,
    pub(super) title: TextArea,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Template)]
pub struct Template {
    pub(super) background_id: ResourceId,
    pub(super) font_id: ResourceId,
    pub(super) layout: Layout,
}

impl<'de> Deserialize<'de> for Template {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Input {
            background_id: ResourceId,
            font_id: ResourceId,
            layout: Layout,
        }
        let input = Input::deserialize(deserializer)?;
        let template = Self {
            background_id: input.background_id,
            font_id: input.font_id,
            layout: input.layout,
        };
        super::settings::validate_layout(&template).map_err(serde::de::Error::custom)?;
        if template.background_id == template.font_id {
            return Err(serde::de::Error::custom(
                "background and font have distinct resource identities",
            ));
        }
        Ok(template)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Department)]
pub struct Department {
    pub(super) id: department::Id,
    pub(super) name: String,
    pub(super) printed: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Position)]
pub struct Position {
    pub(super) roster_id: roster::Id,
    pub(super) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Title)]
pub struct Title {
    pub(super) id: title::Id,
    pub(super) name: String,
}

/// Deserializing this content does not establish acceptance; submit compares every field
/// with a newly constructed trusted snapshot inside the decisive transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Snapshot)]
pub struct Snapshot {
    pub(super) name: String,
    pub(super) number: Number,
    pub(super) printed_number: String,
    pub(super) department: Department,
    #[schema(required)]
    #[serde(deserialize_with = "required_nullable")]
    pub(super) position: Option<Position>,
    #[schema(required)]
    #[serde(deserialize_with = "required_nullable")]
    pub(super) title: Option<Title>,
    pub(super) printed_title: String,
    pub(super) photo_id: PhotoId,
    pub(super) template: Template,
    pub(super) pickup: String,
    pub(super) text: TextLayout,
}

/// UTF-8 byte ranges address the original complete printable field, without duplicated text.
/// Coordinates use micrometres from the canvas top-left; x is the line's left edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Line)]
pub struct Line {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) x_um: f64,
    pub(super) baseline_um: f64,
    pub(super) width_um: f64,
    pub(super) direction: Direction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = application::Direction)]
pub enum Direction {
    Ltr,
    Rtl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = application::TextLayout)]
pub struct TextLayout {
    pub(super) name: Vec<Line>,
    pub(super) number: Vec<Line>,
    pub(super) department: Vec<Line>,
    pub(super) title: Vec<Line>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Selections)]
pub struct Selections {
    pub(super) department_id: department::Id,
    #[schema(required)]
    #[serde(deserialize_with = "required_nullable")]
    pub(super) roster_id: Option<roster::Id>,
    #[schema(required)]
    #[serde(deserialize_with = "required_nullable")]
    pub(super) title_id: Option<title::Id>,
    pub(super) photo_id: PhotoId,
    pub(super) pickup: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::PreviewInput)]
pub struct PreviewInput {
    pub(super) kind: Kind,
    pub(super) selections: Selections,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schema(as = application::Input)]
pub struct Input {
    pub(super) kind: Kind,
    pub(super) selections: Selections,
    pub(super) expected: Snapshot,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = application::Preview)]
pub struct Preview {
    pub(super) kind: Kind,
    pub(super) snapshot: Snapshot,
    pub(super) photo: Photo,
}

#[derive(Serialize, ToSchema)]
#[schema(as = application::TemplateChoices)]
pub struct TemplateChoices {
    pub(super) template: Template,
    pub(super) pickups: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = application::Photo)]
pub struct Photo {
    pub(super) photo_id: PhotoId,
    pub(super) resource_id: ResourceId,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) media_type: &'static str,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = application::Summary)]
pub struct Summary {
    pub(super) id: Id,
    pub(super) kind: Kind,
    pub(super) status: Status,
    pub(super) submitted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = application::Application)]
pub struct Application {
    pub(super) id: Id,
    pub(super) kind: Kind,
    #[serde(rename = "submittedAt")]
    pub(super) submitted_at: DateTime<Utc>,
    #[serde(flatten)]
    pub(super) decision: Decision,
    pub(super) snapshot: Snapshot,
    pub(super) photo: Photo,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = application::ManagedSummary)]
pub struct ManagedSummary {
    #[serde(flatten)]
    pub(super) summary: Summary,
    pub(super) name: String,
    pub(super) printed_number: String,
    pub(super) department: Department,
}

#[derive(Serialize, ToSchema)]
#[schema(as = application::ManagedList)]
pub struct ManagedList {
    pub(super) count: i64,
    pub(super) applications: Vec<ManagedSummary>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = application::DirectoryValue)]
pub struct DirectoryValue {
    pub(super) name: String,
    pub(super) enabled: bool,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = application::Current)]
pub struct Current {
    pub(super) name: String,
    #[schema(required)]
    pub(super) number: Option<Number>,
    pub(super) account_active: bool,
    pub(super) participation_active: bool,
    pub(super) department: DirectoryValue,
    #[schema(required)]
    pub(super) position: Option<DirectoryValue>,
    #[schema(required)]
    pub(super) title: Option<DirectoryValue>,
}

#[derive(Serialize, ToSchema)]
#[schema(as = application::ManagedDetail)]
pub struct ManagedDetail {
    pub(super) application: Application,
    pub(super) current: Current,
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "access", rename_all = "snake_case")]
#[schema(as = application::List)]
pub enum List {
    Full {
        count: i64,
        applications: Vec<Application>,
    },
    Restricted {
        count: i64,
        applications: Vec<Summary>,
    },
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "access", rename_all = "snake_case")]
#[schema(as = application::Detail)]
pub enum Detail {
    Full { application: Box<Application> },
    Restricted { application: Summary },
}

fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use serde_json::json;

    use super::Selections;
    #[test]
    fn optional_selections_are_explicitly_nullable_and_never_filled_by_defaults() {
        let body = json!({"departmentId":"00000000-0000-4000-8000-000000000001","rosterId":null,"titleId":null,"photoId":"00000000-0000-4000-8000-000000000002","pickup":"Fixture"});
        serde_json::from_value::<Selections>(body.clone()).expect("explicit null selections");
        for key in ["departmentId", "rosterId", "titleId", "photoId", "pickup"] {
            let mut missing = body.clone();
            missing.as_object_mut().expect("object").remove(key);
            assert!(
                serde_json::from_value::<Selections>(missing).is_err(),
                "missing {key}"
            );
        }
    }
}

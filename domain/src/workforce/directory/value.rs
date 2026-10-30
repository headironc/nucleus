use std::str::FromStr;

use serde::{Deserialize, Serialize};
use tokio_postgres::types::{FromSql, ToSql, Type};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::identity::operation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
#[schema(as = directory::Collection)]
pub enum Collection {
    Groups,
    Departments,
    Positions,
    Categories,
    Disciplines,
    Titles,
}

impl Collection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Groups => "groups",
            Self::Departments => "departments",
            Self::Positions => "positions",
            Self::Categories => "categories",
            Self::Disciplines => "disciplines",
            Self::Titles => "titles",
        }
    }
}

impl FromStr for Collection {
    type Err = InputError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "groups" => Ok(Self::Groups),
            "departments" => Ok(Self::Departments),
            "positions" => Ok(Self::Positions),
            "categories" => Ok(Self::Categories),
            "disciplines" => Ok(Self::Disciplines),
            "titles" => Ok(Self::Titles),
            _ => Err(InputError::Collection),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema, ToSql)]
#[serde(transparent)]
#[schema(as = directory::Name, value_type = String, min_length = 1, max_length = 64)]
#[postgres(transparent)]
pub struct Name(String);

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl Name {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Name {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for Name {
    type Err = InputError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.chars().any(char::is_control) {
            return Err(InputError::Name);
        }
        let value: String = value.nfc().collect::<String>().trim().to_owned();
        if !(1..=64).contains(&value.chars().count()) || value.chars().any(char::is_control) {
            return Err(InputError::Name);
        }
        Ok(Self(value))
    }
}

impl<'a> FromSql<'a> for Name {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let value = <&str>::from_sql(kind, raw)?;
        let name: Self = value.parse()?;
        if name.0 != value {
            return Err(Box::new(InputError::Name));
        }
        Ok(name)
    }

    fn accepts(kind: &Type) -> bool {
        <&str as FromSql>::accepts(kind)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema, ToSql,
)]
#[serde(try_from = "i32", into = "i32")]
#[schema(as = directory::Index, value_type = i32, minimum = 0)]
#[postgres(transparent)]
pub struct Index(i32);

impl From<Index> for i32 {
    fn from(index: Index) -> Self {
        index.0
    }
}

impl TryFrom<i32> for Index {
    type Error = InputError;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if value < 0 {
            return Err(InputError::Index);
        }
        Ok(Self(value))
    }
}

impl<'a> FromSql<'a> for Index {
    fn from_sql(
        kind: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self::try_from(i32::from_sql(kind, raw)?)?)
    }

    fn accepts(kind: &Type) -> bool {
        <i32 as FromSql>::accepts(kind)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("unknown Workforce directory collection")]
    Collection,
    #[error("directory name must contain 1 to 64 normalized scalars without controls")]
    Name,
    #[error("directory index must be non-negative")]
    Index,
    #[error("directory operation input does not match its collection, path or immutable identity")]
    Shape,
}

pub mod group {
    use super::{Deserialize, FromSql, Index, Name, Serialize, ToSchema, ToSql, Uuid, operation};

    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, FromSql, ToSql,
    )]
    #[serde(transparent)]
    #[schema(as = directory::group::Id, value_type = Uuid)]
    #[postgres(transparent)]
    pub struct Id(Uuid);

    impl Id {
        pub(crate) fn allocate() -> Self {
            Self(Uuid::new_v4())
        }
    }

    impl From<Uuid> for Id {
        fn from(id: Uuid) -> Self {
            Self(id)
        }
    }

    impl std::fmt::Display for Id {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(formatter)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::group::Creation)]
    pub struct Creation {
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::group::Item)]
    pub struct Item {
        pub(in crate::workforce::directory) id: Id,
        pub(in crate::workforce::directory) operation_id: operation::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
    }

    impl Item {
        pub fn id(&self) -> Id {
            self.id
        }
        pub fn operation_id(&self) -> operation::Id {
            self.operation_id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn enabled(&self) -> bool {
            self.enabled
        }
        pub fn index(&self) -> Index {
            self.index
        }
    }
}

pub mod position {
    use super::{Deserialize, FromSql, Index, Name, Serialize, ToSchema, ToSql, Uuid, operation};

    /// Position identifiers cannot be used as group identifiers.
    ///
    /// ```compile_fail
    /// use domain::workforce::directory::{group, position};
    /// fn group_id(id: position::Id) -> group::Id { id.into() }
    /// ```
    ///
    /// Group identifiers cannot be used as position identifiers either.
    ///
    /// ```compile_fail
    /// use domain::workforce::directory::{group, position};
    /// fn position_id(id: group::Id) -> position::Id { id.into() }
    /// ```
    #[derive(
        Debug,
        Clone,
        Copy,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        Hash,
        Serialize,
        Deserialize,
        ToSchema,
        FromSql,
        ToSql,
    )]
    #[serde(transparent)]
    #[schema(as = directory::position::Id, value_type = Uuid, description = "The identifier of a Workforce position.")]
    #[postgres(transparent)]
    pub struct Id(Uuid);

    impl Id {
        pub(crate) fn allocate() -> Self {
            Self(Uuid::new_v4())
        }
    }

    impl From<Uuid> for Id {
        fn from(id: Uuid) -> Self {
            Self(id)
        }
    }

    impl std::fmt::Display for Id {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(formatter)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::position::Creation)]
    pub struct Creation {
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
    }

    impl Creation {
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn enabled(&self) -> bool {
            self.enabled
        }
        pub fn index(&self) -> Index {
            self.index
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::position::Item)]
    pub struct Item {
        pub(in crate::workforce::directory) id: Id,
        pub(in crate::workforce::directory) operation_id: operation::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
    }

    impl Item {
        pub fn id(&self) -> Id {
            self.id
        }
        pub fn operation_id(&self) -> operation::Id {
            self.operation_id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn enabled(&self) -> bool {
            self.enabled
        }
        pub fn index(&self) -> Index {
            self.index
        }
    }
}

pub mod department {
    use super::{
        Deserialize, FromSql, Index, Name, Serialize, ToSchema, ToSql, Uuid, group, operation,
    };

    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, FromSql, ToSql,
    )]
    #[serde(transparent)]
    #[schema(as = directory::department::Id, value_type = Uuid)]
    #[postgres(transparent)]
    pub struct Id(Uuid);

    impl Id {
        pub(crate) fn allocate() -> Self {
            Self(Uuid::new_v4())
        }
    }

    impl From<Uuid> for Id {
        fn from(id: Uuid) -> Self {
            Self(id)
        }
    }

    impl std::fmt::Display for Id {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(formatter)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::department::Creation)]
    pub struct Creation {
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
        pub(in crate::workforce::directory) group_id: group::Id,
        pub(in crate::workforce::directory) parent_id: Option<Id>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::department::Item)]
    pub struct Item {
        pub(in crate::workforce::directory) id: Id,
        pub(in crate::workforce::directory) operation_id: operation::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
        pub(in crate::workforce::directory) group_id: group::Id,
        #[schema(required)]
        #[serde(deserialize_with = "Option::deserialize")]
        pub(in crate::workforce::directory) parent_id: Option<Id>,
    }

    impl Item {
        pub fn id(&self) -> Id {
            self.id
        }
        pub fn operation_id(&self) -> operation::Id {
            self.operation_id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn enabled(&self) -> bool {
            self.enabled
        }
        pub fn index(&self) -> Index {
            self.index
        }
        pub fn group_id(&self) -> group::Id {
            self.group_id
        }
        pub fn parent_id(&self) -> Option<Id> {
            self.parent_id
        }
    }
}

pub mod category {
    use super::{Deserialize, FromSql, Index, Name, Serialize, ToSchema, ToSql, Uuid, operation};

    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, FromSql, ToSql,
    )]
    #[serde(transparent)]
    #[schema(as = directory::category::Id, value_type = Uuid, description = "The identifier of a title category.")]
    #[postgres(transparent)]
    pub struct Id(Uuid);

    impl Id {
        pub(crate) fn allocate() -> Self {
            Self(Uuid::new_v4())
        }
    }

    impl From<Uuid> for Id {
        fn from(id: Uuid) -> Self {
            Self(id)
        }
    }

    impl std::fmt::Display for Id {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(formatter)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::category::Creation)]
    pub struct Creation {
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::category::Item)]
    pub struct Item {
        pub(in crate::workforce::directory) id: Id,
        pub(in crate::workforce::directory) operation_id: operation::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
    }

    impl Item {
        pub fn id(&self) -> Id {
            self.id
        }
        pub fn operation_id(&self) -> operation::Id {
            self.operation_id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn enabled(&self) -> bool {
            self.enabled
        }
        pub fn index(&self) -> Index {
            self.index
        }
    }
}

pub mod discipline {
    use super::{
        Deserialize, FromSql, Index, Name, Serialize, ToSchema, ToSql, Uuid, category, operation,
    };

    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, FromSql, ToSql,
    )]
    #[serde(transparent)]
    #[schema(as = directory::discipline::Id, value_type = Uuid, description = "The identifier of a title discipline.")]
    #[postgres(transparent)]
    pub struct Id(Uuid);

    impl Id {
        pub(crate) fn allocate() -> Self {
            Self(Uuid::new_v4())
        }
    }

    impl From<Uuid> for Id {
        fn from(id: Uuid) -> Self {
            Self(id)
        }
    }

    impl std::fmt::Display for Id {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(formatter)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::discipline::Creation)]
    pub struct Creation {
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
        pub(in crate::workforce::directory) category_id: category::Id,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::discipline::Item)]
    pub struct Item {
        pub(in crate::workforce::directory) id: Id,
        pub(in crate::workforce::directory) operation_id: operation::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
        pub(in crate::workforce::directory) category_id: category::Id,
    }

    impl Item {
        pub fn id(&self) -> Id {
            self.id
        }
        pub fn operation_id(&self) -> operation::Id {
            self.operation_id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn enabled(&self) -> bool {
            self.enabled
        }
        pub fn index(&self) -> Index {
            self.index
        }
        pub fn category_id(&self) -> category::Id {
            self.category_id
        }
    }
}

pub mod title {
    use super::{
        Deserialize, FromSql, Index, Name, Serialize, ToSchema, ToSql, Uuid, discipline, operation,
    };

    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, FromSql, ToSql,
    )]
    #[serde(transparent)]
    #[schema(as = directory::title::Id, value_type = Uuid, description = "The identifier of a complete title.")]
    #[postgres(transparent)]
    pub struct Id(Uuid);

    impl Id {
        pub(crate) fn allocate() -> Self {
            Self(Uuid::new_v4())
        }
    }

    impl From<Uuid> for Id {
        fn from(id: Uuid) -> Self {
            Self(id)
        }
    }

    impl std::fmt::Display for Id {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(formatter)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::title::Creation)]
    pub struct Creation {
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
        pub(in crate::workforce::directory) discipline_id: discipline::Id,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::title::Item)]
    pub struct Item {
        pub(in crate::workforce::directory) id: Id,
        pub(in crate::workforce::directory) operation_id: operation::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
        pub(in crate::workforce::directory) discipline_id: discipline::Id,
    }

    impl Item {
        pub fn id(&self) -> Id {
            self.id
        }
        pub fn operation_id(&self) -> operation::Id {
            self.operation_id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn enabled(&self) -> bool {
            self.enabled
        }
        pub fn index(&self) -> Index {
            self.index
        }
        pub fn discipline_id(&self) -> discipline::Id {
            self.discipline_id
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Creation {
    Group(group::Creation),
    Department(department::Creation),
    Position(position::Creation),
    Category(category::Creation),
    Discipline(discipline::Creation),
    Title(title::Creation),
}

impl Creation {
    pub fn collection(&self) -> Collection {
        match self {
            Self::Group(_) => Collection::Groups,
            Self::Department(_) => Collection::Departments,
            Self::Position(_) => Collection::Positions,
            Self::Category(_) => Collection::Categories,
            Self::Discipline(_) => Collection::Disciplines,
            Self::Title(_) => Collection::Titles,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Item {
    Group(group::Item),
    Department(department::Item),
    Position(position::Item),
    Category(category::Item),
    Discipline(discipline::Item),
    Title(title::Item),
}

impl Item {
    pub fn collection(&self) -> Collection {
        match self {
            Self::Group(_) => Collection::Groups,
            Self::Department(_) => Collection::Departments,
            Self::Position(_) => Collection::Positions,
            Self::Category(_) => Collection::Categories,
            Self::Discipline(_) => Collection::Disciplines,
            Self::Title(_) => Collection::Titles,
        }
    }

    pub fn target(&self) -> Target {
        match self {
            Self::Group(item) => Target::Group(item.id),
            Self::Department(item) => Target::Department(item.id),
            Self::Position(item) => Target::Position(item.id),
            Self::Category(item) => Target::Category(item.id),
            Self::Discipline(item) => Target::Discipline(item.id),
            Self::Title(item) => Target::Title(item.id),
        }
    }

    pub fn operation_id(&self) -> operation::Id {
        match self {
            Self::Group(item) => item.operation_id,
            Self::Department(item) => item.operation_id,
            Self::Position(item) => item.operation_id,
            Self::Category(item) => item.operation_id,
            Self::Discipline(item) => item.operation_id,
            Self::Title(item) => item.operation_id,
        }
    }
}

impl From<group::Item> for Item {
    fn from(item: group::Item) -> Self {
        Self::Group(item)
    }
}

impl From<department::Item> for Item {
    fn from(item: department::Item) -> Self {
        Self::Department(item)
    }
}

impl From<position::Item> for Item {
    fn from(item: position::Item) -> Self {
        Self::Position(item)
    }
}

impl From<category::Item> for Item {
    fn from(item: category::Item) -> Self {
        Self::Category(item)
    }
}

impl From<discipline::Item> for Item {
    fn from(item: discipline::Item) -> Self {
        Self::Discipline(item)
    }
}

impl From<title::Item> for Item {
    fn from(item: title::Item) -> Self {
        Self::Title(item)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "(Collection, Uuid)")]
pub enum Target {
    Group(group::Id),
    Department(department::Id),
    Position(position::Id),
    Category(category::Id),
    Discipline(discipline::Id),
    Title(title::Id),
}

impl From<(Collection, Uuid)> for Target {
    fn from((collection, id): (Collection, Uuid)) -> Self {
        match collection {
            Collection::Groups => Self::Group(id.into()),
            Collection::Departments => Self::Department(id.into()),
            Collection::Positions => Self::Position(id.into()),
            Collection::Categories => Self::Category(id.into()),
            Collection::Disciplines => Self::Discipline(id.into()),
            Collection::Titles => Self::Title(id.into()),
        }
    }
}

impl Target {
    pub fn collection(self) -> Collection {
        match self {
            Self::Group(_) => Collection::Groups,
            Self::Department(_) => Collection::Departments,
            Self::Position(_) => Collection::Positions,
            Self::Category(_) => Collection::Categories,
            Self::Discipline(_) => Collection::Disciplines,
            Self::Title(_) => Collection::Titles,
        }
    }

    pub fn canonical(self) -> String {
        match self {
            Self::Group(id) => id.to_string(),
            Self::Department(id) => id.to_string(),
            Self::Position(id) => id.to_string(),
            Self::Category(id) => id.to_string(),
            Self::Discipline(id) => id.to_string(),
            Self::Title(id) => id.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = directory::Update)]
pub struct Update<T = Item> {
    #[schema(inline)]
    pub(in crate::workforce::directory) expected: T,
    #[schema(inline)]
    pub(in crate::workforce::directory) replacement: T,
}

impl<T: Into<Item>> Update<T> {
    pub fn into_items(self) -> Update {
        Update {
            expected: self.expected.into(),
            replacement: self.replacement.into(),
        }
    }
}

impl Update {
    pub(super) fn validate(&self, target: Target) -> Result<(), InputError> {
        let expected = self.expected.target();
        let replacement = self.replacement.target();
        if expected != target
            || replacement != target
            || self.expected.operation_id() != self.replacement.operation_id()
        {
            return Err(InputError::Shape);
        }
        Ok(())
    }

    pub(in crate::workforce::directory) fn expected(&self) -> &Item {
        &self.expected
    }

    pub(in crate::workforce::directory) fn replacement(&self) -> &Item {
        &self.replacement
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = directory::Delete)]
pub struct Delete<T = Item> {
    #[schema(inline)]
    pub(in crate::workforce::directory) expected: T,
}

impl<T: Into<Item>> Delete<T> {
    pub fn into_items(self) -> Delete {
        Delete {
            expected: self.expected.into(),
        }
    }
}

impl Delete {
    pub(super) fn validate(&self, target: Target) -> Result<(), InputError> {
        if self.expected.target() != target {
            return Err(InputError::Shape);
        }
        Ok(())
    }

    pub(in crate::workforce::directory) fn expected(&self) -> &Item {
        &self.expected
    }
}

pub mod rule {
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    use crate::workforce::{
        directory::{InputError, department},
        number::Subtype,
    };

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::rule::Creation)]
    pub struct Creation {
        pub(in crate::workforce::directory) department_id: department::Id,
        pub(in crate::workforce::directory) position: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::rule::Rule)]
    pub struct Rule {
        pub(in crate::workforce::directory) id: Subtype,
        pub(in crate::workforce::directory) department_id: department::Id,
        pub(in crate::workforce::directory) position: bool,
    }

    impl Rule {
        pub fn id(&self) -> Subtype {
            self.id
        }
        pub fn department_id(&self) -> department::Id {
            self.department_id
        }
        pub fn position(&self) -> bool {
            self.position
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields)]
    #[schema(as = directory::rule::Update)]
    pub struct Update {
        pub(in crate::workforce::directory) expected: Rule,
        pub(in crate::workforce::directory) replacement: Rule,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(untagged)]
    #[schema(as = directory::rule::Input)]
    pub enum Input {
        Create(Creation),
        Update(Update),
    }

    impl Input {
        pub(in crate::workforce::directory) fn validate(
            &self,
            subtype: Subtype,
        ) -> Result<(), InputError> {
            match self {
                Self::Create(_) => Ok(()),
                Self::Update(input)
                    if input.expected.id == subtype && input.replacement.id == subtype =>
                {
                    Ok(())
                }
                Self::Update(_) => Err(InputError::Shape),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[schema(as = directory::rule::Configuration)]
    pub struct Configuration {
        pub(in crate::workforce::directory) rules: Vec<Rule>,
        pub(in crate::workforce::directory) missing: Vec<Subtype>,
    }

    impl Configuration {
        pub(in crate::workforce::directory) fn new(rules: Vec<Rule>) -> Self {
            let missing = Subtype::ALL
                .into_iter()
                .filter(|subtype| rules.iter().all(|rule| rule.id != *subtype))
                .collect();
            Self { rules, missing }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Receipt {
        pub(in crate::workforce::directory) rule: Rule,
        pub(in crate::workforce::directory) created: bool,
    }

    impl Receipt {
        pub fn rule(&self) -> &Rule {
            &self.rule
        }
        pub fn created(&self) -> bool {
            self.created
        }
    }
}

pub mod roster {
    use std::collections::HashSet;

    use super::{
        Deserialize, FromSql, Index, InputError, Serialize, ToSchema, ToSql, Uuid, position,
    };

    /// Department position entries keep their own identity.
    ///
    /// ```compile_fail
    /// use domain::workforce::directory::{position, roster};
    /// fn entry_id(id: position::Id) -> roster::Id { id.into() }
    /// ```
    #[derive(
        Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, FromSql, ToSql,
    )]
    #[serde(transparent)]
    #[schema(as = directory::roster::Id, value_type = Uuid, description = "The identifier of one configured department position.")]
    #[postgres(transparent)]
    pub struct Id(Uuid);

    impl Id {
        pub(crate) fn allocate() -> Self {
            Self(Uuid::new_v4())
        }
    }

    /// One position configured for the department named by the path.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::roster::Entry)]
    pub struct Entry {
        pub(in crate::workforce::directory) id: Id,
        pub(in crate::workforce::directory) position_id: position::Id,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
    }

    impl Entry {
        pub fn id(&self) -> Id {
            self.id
        }
        pub fn position_id(&self) -> position::Id {
            self.position_id
        }
        pub fn enabled(&self) -> bool {
            self.enabled
        }
        pub fn index(&self) -> Index {
            self.index
        }

        pub(in crate::workforce::directory) fn configures(&self, choice: &Choice) -> bool {
            self.position_id == choice.position_id
                && self.enabled == choice.enabled
                && self.index == choice.index
        }
    }

    /// The department's complete position list ordered by index then entry ID, or the
    /// deletion of the department after the original replacement.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Receipt {
        Present(Vec<Entry>),
        Deleted,
    }

    /// A replacement entry; retained positions keep their entry ID and new ones receive one.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[schema(as = directory::roster::Choice)]
    pub struct Choice {
        pub(in crate::workforce::directory) position_id: position::Id,
        pub(in crate::workforce::directory) enabled: bool,
        pub(in crate::workforce::directory) index: Index,
    }

    /// The complete current list and its complete replacement for one department.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
    #[serde(deny_unknown_fields)]
    #[schema(as = directory::roster::Replace)]
    pub struct Replace {
        pub(in crate::workforce::directory) expected: Vec<Entry>,
        pub(in crate::workforce::directory) replacement: Vec<Choice>,
    }

    impl Replace {
        /// Orders both lists by position, so list order never changes the canonical input,
        /// and rejects a position or entry that appears twice.
        pub(in crate::workforce::directory) fn canonical(mut self) -> Result<Self, InputError> {
            self.expected.sort_by_key(|entry| entry.position_id);
            self.replacement.sort_by_key(|choice| choice.position_id);
            let mut ids = HashSet::new();
            let repeated = self
                .expected
                .windows(2)
                .any(|pair| pair[0].position_id == pair[1].position_id)
                || !self.expected.iter().all(|entry| ids.insert(entry.id))
                || self
                    .replacement
                    .windows(2)
                    .any(|pair| pair[0].position_id == pair[1].position_id);
            if repeated {
                return Err(InputError::Shape);
            }
            Ok(self)
        }
    }
}

pub mod selection {
    use serde::Serialize;
    use utoipa::ToSchema;

    use crate::workforce::{
        directory::{
            Index, Name, category, department, discipline, group, position, roster, title,
        },
        number::Subtype,
    };

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[schema(as = directory::selection::Group)]
    pub struct Group {
        pub(in crate::workforce::directory) id: group::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) index: Index,
    }

    impl Group {
        pub fn id(&self) -> group::Id {
            self.id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn index(&self) -> Index {
            self.index
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[schema(as = directory::selection::Department)]
    pub struct Department {
        pub(in crate::workforce::directory) id: department::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) index: Index,
        pub(in crate::workforce::directory) group_id: group::Id,
        #[schema(required)]
        pub(in crate::workforce::directory) parent_id: Option<department::Id>,
    }

    impl Department {
        pub fn id(&self) -> department::Id {
            self.id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn index(&self) -> Index {
            self.index
        }
        pub fn group_id(&self) -> group::Id {
            self.group_id
        }
        pub fn parent_id(&self) -> Option<department::Id> {
            self.parent_id
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[schema(as = directory::selection::Position)]
    pub struct Position {
        pub(in crate::workforce::directory) department_id: department::Id,
        pub(in crate::workforce::directory) entry_id: roster::Id,
        pub(in crate::workforce::directory) position_id: position::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) index: Index,
    }

    impl Position {
        pub fn department_id(&self) -> department::Id {
            self.department_id
        }
        pub fn entry_id(&self) -> roster::Id {
            self.entry_id
        }
        pub fn position_id(&self) -> position::Id {
            self.position_id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn index(&self) -> Index {
            self.index
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[schema(as = directory::selection::Category)]
    pub struct Category {
        pub(in crate::workforce::directory) id: category::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) index: Index,
    }

    impl Category {
        pub fn id(&self) -> category::Id {
            self.id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn index(&self) -> Index {
            self.index
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[schema(as = directory::selection::Discipline)]
    pub struct Discipline {
        pub(in crate::workforce::directory) id: discipline::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) index: Index,
        pub(in crate::workforce::directory) category_id: category::Id,
    }

    impl Discipline {
        pub fn id(&self) -> discipline::Id {
            self.id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn index(&self) -> Index {
            self.index
        }
        pub fn category_id(&self) -> category::Id {
            self.category_id
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[schema(as = directory::selection::Title)]
    pub struct Title {
        pub(in crate::workforce::directory) id: title::Id,
        pub(in crate::workforce::directory) name: Name,
        pub(in crate::workforce::directory) index: Index,
        pub(in crate::workforce::directory) discipline_id: discipline::Id,
    }

    impl Title {
        pub fn id(&self) -> title::Id {
            self.id
        }
        pub fn name(&self) -> &Name {
            &self.name
        }
        pub fn index(&self) -> Index {
            self.index
        }
        pub fn discipline_id(&self) -> discipline::Id {
            self.discipline_id
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[schema(as = directory::selection::Rule)]
    pub struct Rule {
        pub(in crate::workforce::directory) id: Subtype,
        pub(in crate::workforce::directory) department_id: department::Id,
        pub(in crate::workforce::directory) position: bool,
        pub(in crate::workforce::directory) selectable: bool,
    }

    impl Rule {
        pub fn id(&self) -> Subtype {
            self.id
        }
        pub fn department_id(&self) -> department::Id {
            self.department_id
        }
        pub fn position(&self) -> bool {
            self.position
        }
        pub fn selectable(&self) -> bool {
            self.selectable
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
    #[serde(rename_all = "camelCase")]
    #[schema(as = directory::selection::Selection)]
    pub struct Selection {
        pub(in crate::workforce::directory) groups: Vec<Group>,
        pub(in crate::workforce::directory) departments: Vec<Department>,
        pub(in crate::workforce::directory) positions: Vec<Position>,
        pub(in crate::workforce::directory) categories: Vec<Category>,
        pub(in crate::workforce::directory) disciplines: Vec<Discipline>,
        pub(in crate::workforce::directory) titles: Vec<Title>,
        pub(in crate::workforce::directory) rules: Vec<Rule>,
        pub(in crate::workforce::directory) missing: Vec<Subtype>,
    }

    impl Selection {
        pub fn groups(&self) -> &[Group] {
            &self.groups
        }
        pub fn departments(&self) -> &[Department] {
            &self.departments
        }
        pub fn positions(&self) -> &[Position] {
            &self.positions
        }
        pub fn categories(&self) -> &[Category] {
            &self.categories
        }
        pub fn disciplines(&self) -> &[Discipline] {
            &self.disciplines
        }
        pub fn titles(&self) -> &[Title] {
            &self.titles
        }
        pub fn rules(&self) -> &[Rule] {
            &self.rules
        }
        pub fn missing(&self) -> &[Subtype] {
            &self.missing
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use serde_json::json;

    use super::{Collection, Delete, Index, Item, Name, Target, Update, group, position};

    #[test]
    fn names_indices_and_collections_are_controlled() {
        assert_eq!(
            "  Cafe\u{301}  ".parse::<Name>().expect("NFC").as_str(),
            "Café"
        );
        assert_eq!(
            serde_json::to_value("字".repeat(64).parse::<Name>().expect("64 scalars"))
                .expect("JSON"),
            "字".repeat(64)
        );
        for invalid in [
            "",
            " ",
            "a\u{0000}b",
            "\tname",
            "name\n",
            "a\u{0085}b",
            &"字".repeat(65),
        ] {
            assert!(invalid.parse::<Name>().is_err());
            assert!(serde_json::from_value::<Name>(json!(invalid)).is_err());
        }
        assert_ne!(
            "Alpha".parse::<Name>().expect("uppercase"),
            "alpha".parse::<Name>().expect("lowercase")
        );
        assert_eq!(
            serde_json::from_value::<Name>(json!("  Cafe\u{301}  "))
                .expect("canonical JSON")
                .as_str(),
            "Café"
        );
        for value in ["main", "Café", &"字".repeat(64)] {
            assert!(
                <Name as tokio_postgres::types::FromSql>::from_sql(
                    &tokio_postgres::types::Type::TEXT,
                    value.as_bytes()
                )
                .is_ok()
            );
        }
        for value in [" north ", "A\u{301}", &"字".repeat(65)] {
            assert!(
                <Name as tokio_postgres::types::FromSql>::from_sql(
                    &tokio_postgres::types::Type::TEXT,
                    value.as_bytes()
                )
                .is_err()
            );
        }
        assert_eq!(
            serde_json::from_value::<Index>(json!(0)).expect("zero"),
            0.try_into().expect("index")
        );
        assert!(serde_json::from_value::<Index>(json!(-1)).is_err());
        assert_eq!(
            "groups".parse::<Collection>().expect("groups"),
            Collection::Groups
        );
        assert_eq!(
            "positions".parse::<Collection>().expect("positions"),
            Collection::Positions
        );
        for (path, collection) in [
            ("categories", Collection::Categories),
            ("disciplines", Collection::Disciplines),
            ("titles", Collection::Titles),
        ] {
            assert_eq!(path.parse::<Collection>().expect("title tree"), collection);
            assert_eq!(collection.as_str(), path);
        }
        assert!("rosters".parse::<Collection>().is_err());
        assert!("unknown".parse::<Collection>().is_err());
    }

    #[test]
    fn complete_conditional_input_binds_path_and_immutable_identity() {
        let item = Item::Group(serde_json::from_value(json!({"id":uuid::Uuid::new_v4(),"operationId":uuid::Uuid::new_v4(),"name":"Group","enabled":true,"index":0})).expect("group item"));
        let target = item.target();
        let update = Update {
            expected: item.clone(),
            replacement: item.clone(),
        };
        update.validate(target).expect("matching conditional input");
        let delete = Delete {
            expected: item.clone(),
        };
        delete.validate(target).expect("matching deletion");
        let other = Item::Group(serde_json::from_value(json!({"id":uuid::Uuid::new_v4(),"operationId":uuid::Uuid::new_v4(),"name":"Other","enabled":true,"index":0})).expect("other item"));
        assert!(
            Update {
                expected: item.clone(),
                replacement: other.clone()
            }
            .validate(target)
            .is_err()
        );
        assert!(delete.validate(other.target()).is_err());
    }

    #[test]
    fn conditional_inputs_preserve_the_collection_even_with_identical_uuid_values() {
        let id = uuid::Uuid::new_v4();
        let item = json!({"id":id,"operationId":uuid::Uuid::new_v4(),"name":"Shared shape","enabled":true,"index":0});
        let update = json!({"expected":item,"replacement":item});
        let delete = json!({"expected":item});
        let group_update = serde_json::from_value::<Update<group::Item>>(update.clone())
            .expect("group update")
            .into_items();
        let position_update = serde_json::from_value::<Update<position::Item>>(update)
            .expect("position update")
            .into_items();
        let group_delete = serde_json::from_value::<Delete<group::Item>>(delete.clone())
            .expect("group deletion")
            .into_items();
        let position_delete = serde_json::from_value::<Delete<position::Item>>(delete)
            .expect("position deletion")
            .into_items();
        let group_target = Target::Group(id.into());
        let position_target = Target::Position(id.into());
        assert_ne!(group_target, position_target);
        for (update, delete, target, other) in [
            (group_update, group_delete, group_target, position_target),
            (
                position_update,
                position_delete,
                position_target,
                group_target,
            ),
        ] {
            update.validate(target).expect("own collection");
            delete.validate(target).expect("own collection");
            assert!(update.validate(other).is_err());
            assert!(delete.validate(other).is_err());
        }
    }

    #[test]
    fn rule_input_requires_a_real_subtype_and_binds_the_complete_path_value() {
        use crate::workforce::{directory::rule, number::Subtype};

        let department_id = uuid::Uuid::new_v4();
        let input: rule::Input =
            serde_json::from_value(json!({"departmentId":department_id,"position":false}))
                .expect("explicit rule creation");
        input.validate(Subtype::Internship).expect("creation path");
        for invalid in [
            json!({"position":false}),
            json!({"departmentId":null,"position":false}),
            json!({"departmentId":department_id}),
            json!({"departmentId":department_id,"position":false,"extra":0}),
        ] {
            assert!(serde_json::from_value::<rule::Input>(invalid).is_err());
        }
        let expected: rule::Rule =
            serde_json::from_value(json!({"id":902,"departmentId":department_id,"position":false}))
                .expect("fixed subtype rule");
        let replacement = rule::Rule {
            position: true,
            ..expected.clone()
        };
        let update = rule::Input::Update(rule::Update {
            expected: expected.clone(),
            replacement: replacement.clone(),
        });
        update.validate(Subtype::Internship).expect("matching path");
        assert!(update.validate(Subtype::Clinical).is_err());
        assert!(
            serde_json::from_value::<rule::Rule>(
                json!({"id":999,"departmentId":department_id,"position":false})
            )
            .is_err()
        );
        let other_department = rule::Rule {
            department_id: serde_json::from_value(json!(uuid::Uuid::new_v4()))
                .expect("other department"),
            ..replacement
        };
        assert_ne!(expected, other_department);
        let configuration = rule::Configuration::new(vec![expected]);
        assert_eq!(configuration.rules.len(), 1);
        assert_eq!(configuration.missing.len(), 15);
        assert!(!configuration.missing.contains(&Subtype::Internship));
        assert!(configuration.missing.contains(&Subtype::Clinical));
    }
}

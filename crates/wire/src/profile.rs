//! Directory profile: the display `name` the owner sets and the
//! self-declared `gender_form`, `age_band`, `role` and `intro` (API_V1
//! "Directory profile 필드 규칙"; `IF-WIRE-006`).
//!
//! Requests carry every field as a string, so a misspelt enum value is a
//! validation error ([`ProfileFieldError`]) rather than a deserialization
//! error. That lets the server answer an agent that sends `name` with 403
//! before any value rule runs. A partial update tells three states apart
//! with [`FieldPatch`]: key omitted, `null`, or a value.
//!
//! The hub stores and returns these values as opaque self-declared
//! metadata; nothing here interprets them.

use std::fmt;
use std::str::FromStr;

use serde::de::value::MapAccessDeserializer;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::AgentId;

/// Largest `name`, in Unicode scalar values after trimming. A contract
/// constant (`IF-WIRE-006`), not a tuning value.
pub const MAX_NAME_CHARS: usize = 32;

/// Largest `role`, in Unicode scalar values after trimming.
pub const MAX_ROLE_CHARS: usize = 80;

/// Largest `intro`, in Unicode scalar values after trimming.
pub const MAX_INTRO_CHARS: usize = 280;

// ---- enums -----------------------------------------------------------------------

/// Declared gender form. `None` declares that there is none; an undeclared
/// value is `null` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenderForm {
    #[serde(rename = "female")]
    Female,
    #[serde(rename = "male")]
    Male,
    #[serde(rename = "none")]
    None,
}

impl GenderForm {
    pub const ALL: [GenderForm; 3] = [GenderForm::Female, GenderForm::Male, GenderForm::None];

    /// The exact wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            GenderForm::Female => "female",
            GenderForm::Male => "male",
            GenderForm::None => "none",
        }
    }
}

/// Declared age band. `None` declares that there is none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgeBand {
    #[serde(rename = "child")]
    Child,
    #[serde(rename = "teen")]
    Teen,
    #[serde(rename = "20s")]
    Twenties,
    #[serde(rename = "30s_40s")]
    ThirtiesForties,
    #[serde(rename = "50s_plus")]
    FiftiesPlus,
    #[serde(rename = "none")]
    None,
}

impl AgeBand {
    pub const ALL: [AgeBand; 6] = [
        AgeBand::Child,
        AgeBand::Teen,
        AgeBand::Twenties,
        AgeBand::ThirtiesForties,
        AgeBand::FiftiesPlus,
        AgeBand::None,
    ];

    /// The exact wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            AgeBand::Child => "child",
            AgeBand::Teen => "teen",
            AgeBand::Twenties => "20s",
            AgeBand::ThirtiesForties => "30s_40s",
            AgeBand::FiftiesPlus => "50s_plus",
            AgeBand::None => "none",
        }
    }
}

/// Error from parsing an enum spelling that is not exactly a wire value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown value")]
pub struct UnknownValue;

impl FromStr for GenderForm {
    type Err = UnknownValue;

    /// Case-sensitive, untrimmed match (`IF-WIRE-006`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        GenderForm::ALL
            .into_iter()
            .find(|value| value.as_str() == s)
            .ok_or(UnknownValue)
    }
}

impl FromStr for AgeBand {
    type Err = UnknownValue;

    /// Case-sensitive, untrimmed match (`IF-WIRE-006`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AgeBand::ALL
            .into_iter()
            .find(|value| value.as_str() == s)
            .ok_or(UnknownValue)
    }
}

impl fmt::Display for GenderForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Display for AgeBand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---- fields ----------------------------------------------------------------------

/// The writable profile fields, in the order `IF-WIRE-006` checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileField {
    Name,
    GenderForm,
    AgeBand,
    Role,
    Intro,
}

impl ProfileField {
    /// The fields an agent may write about itself (everything but `name`).
    pub const SELF_DECLARED: [ProfileField; 4] = [
        ProfileField::GenderForm,
        ProfileField::AgeBand,
        ProfileField::Role,
        ProfileField::Intro,
    ];

    /// The JSON key.
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileField::Name => "name",
            ProfileField::GenderForm => "gender_form",
            ProfileField::AgeBand => "age_band",
            ProfileField::Role => "role",
            ProfileField::Intro => "intro",
        }
    }
}

impl FromStr for ProfileField {
    type Err = UnknownValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        std::iter::once(ProfileField::Name)
            .chain(ProfileField::SELF_DECLARED)
            .find(|field| field.as_str() == s)
            .ok_or(UnknownValue)
    }
}

impl fmt::Display for ProfileField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a profile field value was rejected. Messages never repeat the value:
/// it is free text another agent reads, handled like a message body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileFieldProblem {
    /// Empty after trimming. Clearing a field is `null`.
    Empty,
    /// More scalar values than the field allows after trimming.
    TooLong { max_chars: usize },
    /// A Unicode `Cc` character (`intro` allows only LF).
    ControlCharacter,
    /// Not exactly one of the enum spellings.
    UnknownValue,
    /// `name: null`; the name can be changed but not removed.
    CannotClear,
}

/// A profile field that failed validation (`IF-WIRE-006`), for 422
/// `invalid_field` with `details.field`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.describe())]
pub struct ProfileFieldError {
    pub field: ProfileField,
    pub problem: ProfileFieldProblem,
}

impl ProfileFieldError {
    /// The JSON key, for `details.field`.
    pub fn field(&self) -> &'static str {
        self.field.as_str()
    }

    fn describe(&self) -> String {
        let field = self.field.as_str();
        match self.problem {
            ProfileFieldProblem::Empty => {
                format!("{field} must not be empty after trimming; send null to clear it")
            }
            ProfileFieldProblem::TooLong { max_chars } => {
                format!("{field} must be at most {max_chars} characters after trimming")
            }
            ProfileFieldProblem::ControlCharacter if self.field == ProfileField::Intro => {
                format!("{field} must not contain control characters other than a line feed")
            }
            ProfileFieldProblem::ControlCharacter => {
                format!("{field} must not contain control characters")
            }
            ProfileFieldProblem::UnknownValue => {
                format!("{field} must be one of {}", enum_spellings(self.field))
            }
            ProfileFieldProblem::CannotClear => format!("{field} cannot be cleared"),
        }
    }
}

fn enum_spellings(field: ProfileField) -> String {
    let names: Vec<&str> = match field {
        ProfileField::GenderForm => GenderForm::ALL.iter().map(|v| v.as_str()).collect(),
        ProfileField::AgeBand => AgeBand::ALL.iter().map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    };
    names.join(", ")
}

/// The text rules of one string field.
struct TextRule {
    field: ProfileField,
    max_chars: usize,
    allows_line_feed: bool,
}

const NAME_RULE: TextRule = TextRule {
    field: ProfileField::Name,
    max_chars: MAX_NAME_CHARS,
    allows_line_feed: false,
};
const ROLE_RULE: TextRule = TextRule {
    field: ProfileField::Role,
    max_chars: MAX_ROLE_CHARS,
    allows_line_feed: false,
};
const INTRO_RULE: TextRule = TextRule {
    field: ProfileField::Intro,
    max_chars: MAX_INTRO_CHARS,
    allows_line_feed: true,
};

impl TextRule {
    /// Trims (`str::trim`), then checks length in scalar values and `Cc`
    /// characters. Returns the trimmed value.
    fn apply(&self, raw: &str) -> Result<String, ProfileFieldError> {
        let fail = |problem| ProfileFieldError {
            field: self.field,
            problem,
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(fail(ProfileFieldProblem::Empty));
        }
        if trimmed.chars().count() > self.max_chars {
            return Err(fail(ProfileFieldProblem::TooLong {
                max_chars: self.max_chars,
            }));
        }
        let forbidden = |c: char| c.is_control() && !(self.allows_line_feed && c == '\n');
        if trimmed.chars().any(forbidden) {
            return Err(fail(ProfileFieldProblem::ControlCharacter));
        }
        Ok(trimmed.to_owned())
    }
}

/// Validates a display name for `POST /v1/agents` and returns it trimmed.
pub fn validate_name(raw: &str) -> Result<String, ProfileFieldError> {
    NAME_RULE.apply(raw)
}

fn parse_enum<T: FromStr>(field: ProfileField, raw: &str) -> Result<T, ProfileFieldError> {
    raw.parse().map_err(|_| ProfileFieldError {
        field,
        problem: ProfileFieldProblem::UnknownValue,
    })
}

// ---- partial update ----------------------------------------------------------------

/// One field of a partial update: key omitted (`Keep`), `null` (`Clear`) or
/// a value (`Set`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FieldPatch<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

impl<T> FieldPatch<T> {
    pub fn is_keep(&self) -> bool {
        matches!(self, FieldPatch::Keep)
    }

    /// Converts a `Set` value; `Keep` and `Clear` pass through.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> FieldPatch<U> {
        match self {
            FieldPatch::Keep => FieldPatch::Keep,
            FieldPatch::Clear => FieldPatch::Clear,
            FieldPatch::Set(value) => FieldPatch::Set(f(value)),
        }
    }

    /// Validates or converts a `Set` value; `Keep` and `Clear` pass through.
    pub fn try_map<U, E>(self, f: impl FnOnce(T) -> Result<U, E>) -> Result<FieldPatch<U>, E> {
        Ok(match self {
            FieldPatch::Keep => FieldPatch::Keep,
            FieldPatch::Clear => FieldPatch::Clear,
            FieldPatch::Set(value) => FieldPatch::Set(f(value)?),
        })
    }

    /// The stored value after applying this patch to `current`.
    pub fn apply_to(self, current: Option<T>) -> Option<T> {
        match self {
            FieldPatch::Keep => current,
            FieldPatch::Clear => None,
            FieldPatch::Set(value) => Some(value),
        }
    }
}

/// `Clear` serializes as `null`; `Keep` is skipped by the containing struct.
impl<T: Serialize> Serialize for FieldPatch<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            FieldPatch::Keep | FieldPatch::Clear => serializer.serialize_none(),
            FieldPatch::Set(value) => value.serialize(serializer),
        }
    }
}

/// Used with `#[serde(default)]`: only called when the key is present, so a
/// missing key stays `Keep`. Any JSON type but `T` and `null` is an error.
pub(crate) fn present_patch<'de, D, T>(deserializer: D) -> Result<FieldPatch<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(match Option::<T>::deserialize(deserializer)? {
        None => FieldPatch::Clear,
        Some(value) => FieldPatch::Set(value),
    })
}

/// `PUT /v1/agents/{agent_id}/profile` body. Every field is optional; the
/// enums arrive as strings and are checked by [`UpdateProfileRequest::validate`].
///
/// Only a JSON object is accepted. A derived deserializer would also take
/// an array as positional fields, and with every field optional `[]` or
/// `["x"]` would parse; `remote = "Self"` keeps the derived field handling
/// behind an object-only entry point.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct UpdateProfileRequest {
    #[serde(
        default,
        deserialize_with = "present_patch",
        skip_serializing_if = "FieldPatch::is_keep"
    )]
    pub name: FieldPatch<String>,
    #[serde(
        default,
        deserialize_with = "present_patch",
        skip_serializing_if = "FieldPatch::is_keep"
    )]
    pub gender_form: FieldPatch<String>,
    #[serde(
        default,
        deserialize_with = "present_patch",
        skip_serializing_if = "FieldPatch::is_keep"
    )]
    pub age_band: FieldPatch<String>,
    #[serde(
        default,
        deserialize_with = "present_patch",
        skip_serializing_if = "FieldPatch::is_keep"
    )]
    pub role: FieldPatch<String>,
    #[serde(
        default,
        deserialize_with = "present_patch",
        skip_serializing_if = "FieldPatch::is_keep"
    )]
    pub intro: FieldPatch<String>,
}

impl Serialize for UpdateProfileRequest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The inherent function generated by `remote = "Self"`.
        UpdateProfileRequest::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for UpdateProfileRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ObjectOnly)
    }
}

/// Accepts a map and hands it to the derived field handling.
struct ObjectOnly;

impl<'de> Visitor<'de> for ObjectOnly {
    type Value = UpdateProfileRequest;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        UpdateProfileRequest::deserialize(MapAccessDeserializer::new(map))
    }
}

impl UpdateProfileRequest {
    /// The body carries a `name` key, whatever its value (`null` included).
    /// Only the owner may send one.
    pub fn has_name(&self) -> bool {
        !self.name.is_keep()
    }

    /// No key at all: `{}`.
    pub fn is_empty(&self) -> bool {
        [
            &self.name,
            &self.gender_form,
            &self.age_band,
            &self.role,
            &self.intro,
        ]
        .iter()
        .all(|patch| patch.is_keep())
    }

    /// The patch slot of `field`, for building a request field by field.
    pub fn patch_mut(&mut self, field: ProfileField) -> &mut FieldPatch<String> {
        match field {
            ProfileField::Name => &mut self.name,
            ProfileField::GenderForm => &mut self.gender_form,
            ProfileField::AgeBand => &mut self.age_band,
            ProfileField::Role => &mut self.role,
            ProfileField::Intro => &mut self.intro,
        }
    }

    /// Applies the value rules in field order `name` → `gender_form` →
    /// `age_band` → `role` → `intro` and reports the first violation.
    pub fn validate(self) -> Result<ProfileUpdate, ProfileFieldError> {
        let name = match self.name {
            FieldPatch::Keep => None,
            FieldPatch::Clear => {
                return Err(ProfileFieldError {
                    field: ProfileField::Name,
                    problem: ProfileFieldProblem::CannotClear,
                });
            }
            FieldPatch::Set(raw) => Some(validate_name(&raw)?),
        };
        let gender_form = self
            .gender_form
            .try_map(|raw| parse_enum(ProfileField::GenderForm, &raw))?;
        let age_band = self
            .age_band
            .try_map(|raw| parse_enum(ProfileField::AgeBand, &raw))?;
        let role = self.role.try_map(|raw| ROLE_RULE.apply(&raw))?;
        let intro = self.intro.try_map(|raw| INTRO_RULE.apply(&raw))?;
        Ok(ProfileUpdate {
            name,
            gender_form,
            age_band,
            role,
            intro,
        })
    }
}

/// A validated partial update. Strings are trimmed. `name` can only be set
/// (`Some`) or left alone (`None`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileUpdate {
    pub name: Option<String>,
    pub gender_form: FieldPatch<GenderForm>,
    pub age_band: FieldPatch<AgeBand>,
    pub role: FieldPatch<String>,
    pub intro: FieldPatch<String>,
}

impl ProfileUpdate {
    pub fn has_name(&self) -> bool {
        self.name.is_some()
    }
}

// ---- responses ---------------------------------------------------------------------

/// The `profile` object of directory items, registration and profile
/// responses. Undeclared values are `null`, never omitted (`IF-WIRE-001`).
/// `updated_at` is `null` only for an agent registered before S9 whose
/// profile has never changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub gender_form: Option<GenderForm>,
    pub age_band: Option<AgeBand>,
    pub role: Option<String>,
    pub intro: Option<String>,
    pub updated_at: Option<String>,
}

/// `PUT /v1/agents/{agent_id}/profile` 200 body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileResponse {
    pub agent_id: AgentId,
    pub name: Option<String>,
    pub profile: AgentProfile,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn request(value: Value) -> Result<UpdateProfileRequest, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn validated(value: Value) -> Result<ProfileUpdate, ProfileFieldError> {
        request(value).expect("parses").validate()
    }

    fn problem(value: Value) -> (&'static str, ProfileFieldProblem) {
        let err = validated(value).unwrap_err();
        (err.field(), err.problem)
    }

    /// `count` copies of `c`.
    fn chars(c: char, count: usize) -> String {
        std::iter::repeat_n(c, count).collect()
    }

    // ---- name ----------------------------------------------------------------

    #[test]
    fn name_must_not_be_empty_after_trimming() {
        for raw in ["", " ", "   ", "\u{3000}", " \u{a0} "] {
            assert_eq!(
                validate_name(raw).unwrap_err().problem,
                ProfileFieldProblem::Empty,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn name_length_bounds_use_the_contract_constant() {
        assert_eq!(validate_name("a").unwrap(), "a");
        let longest = chars('a', MAX_NAME_CHARS);
        assert_eq!(validate_name(&longest).unwrap(), longest);
        assert_eq!(
            validate_name(&chars('a', MAX_NAME_CHARS + 1))
                .unwrap_err()
                .problem,
            ProfileFieldProblem::TooLong {
                max_chars: MAX_NAME_CHARS
            }
        );
    }

    #[test]
    fn name_length_counts_scalar_values_not_bytes() {
        // 3 bytes per Hangul syllable, 4 per emoji: the byte length is far
        // above the limit while the scalar count is exactly at it.
        for c in ['한', '🦀'] {
            let at_limit = chars(c, MAX_NAME_CHARS);
            assert!(at_limit.len() > MAX_NAME_CHARS);
            assert_eq!(validate_name(&at_limit).unwrap(), at_limit);
            assert!(validate_name(&chars(c, MAX_NAME_CHARS + 1)).is_err());
        }
    }

    #[test]
    fn name_length_is_measured_after_trimming() {
        let padded = format!("  {}  ", chars('a', MAX_NAME_CHARS));
        assert_eq!(validate_name(&padded).unwrap(), chars('a', MAX_NAME_CHARS));
    }

    #[test]
    fn name_is_trimmed_and_keeps_inner_spaces() {
        assert_eq!(validate_name("  에이전트  A \n").unwrap(), "에이전트  A");
    }

    #[test]
    fn name_rejects_control_characters() {
        for raw in ["a\nb", "a\tb", "a\u{0}b", "a\u{7f}b", "a\rb", "\u{0}"] {
            assert_eq!(
                validate_name(raw).unwrap_err().problem,
                ProfileFieldProblem::ControlCharacter,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn non_control_format_characters_are_kept() {
        // Zero-width joiner and a bidi mark are `Cf`, not `Cc`.
        assert!(validate_name("a\u{200d}b\u{200f}").is_ok());
    }

    // ---- role and intro -------------------------------------------------------

    #[test]
    fn role_length_bounds_and_single_line() {
        let longest = chars('역', MAX_ROLE_CHARS);
        let ok = validated(json!({"role": longest})).unwrap();
        assert_eq!(ok.role, FieldPatch::Set(longest));
        assert_eq!(
            problem(json!({"role": chars('a', MAX_ROLE_CHARS + 1)})),
            (
                "role",
                ProfileFieldProblem::TooLong {
                    max_chars: MAX_ROLE_CHARS
                }
            )
        );
        assert_eq!(
            problem(json!({"role": "two\nlines"})),
            ("role", ProfileFieldProblem::ControlCharacter)
        );
        assert_eq!(
            problem(json!({"role": " "})),
            ("role", ProfileFieldProblem::Empty)
        );
    }

    #[test]
    fn intro_length_bounds() {
        let longest = chars('소', MAX_INTRO_CHARS);
        assert_eq!(
            validated(json!({"intro": longest})).unwrap().intro,
            FieldPatch::Set(longest)
        );
        assert_eq!(
            problem(json!({"intro": chars('a', MAX_INTRO_CHARS + 1)})),
            (
                "intro",
                ProfileFieldProblem::TooLong {
                    max_chars: MAX_INTRO_CHARS
                }
            )
        );
    }

    #[test]
    fn intro_allows_line_feed_only() {
        let update = validated(json!({"intro": "\n첫 줄\n둘째 줄\n"})).unwrap();
        assert_eq!(update.intro, FieldPatch::Set("첫 줄\n둘째 줄".to_owned()));
        for raw in ["a\r\nb", "a\tb", "a\u{0}b", "a\u{7f}b"] {
            assert_eq!(
                problem(json!({"intro": raw})),
                ("intro", ProfileFieldProblem::ControlCharacter),
                "{raw:?}"
            );
        }
    }

    // ---- enums ------------------------------------------------------------------

    #[test]
    fn gender_form_accepts_exact_spellings() {
        for value in GenderForm::ALL {
            let update = validated(json!({"gender_form": value.as_str()})).unwrap();
            assert_eq!(update.gender_form, FieldPatch::Set(value));
        }
        assert_eq!(
            validated(json!({"gender_form": "none"}))
                .unwrap()
                .gender_form,
            FieldPatch::Set(GenderForm::None)
        );
    }

    #[test]
    fn age_band_accepts_exact_spellings() {
        let spellings: Vec<&str> = AgeBand::ALL.iter().map(|v| v.as_str()).collect();
        assert_eq!(
            spellings,
            ["child", "teen", "20s", "30s_40s", "50s_plus", "none"]
        );
        for value in AgeBand::ALL {
            let update = validated(json!({"age_band": value.as_str()})).unwrap();
            assert_eq!(update.age_band, FieldPatch::Set(value));
        }
    }

    #[test]
    fn enums_reject_case_spacing_and_spelling_variants() {
        for raw in ["Female", "FEMALE", " none", "none ", "", "x"] {
            assert_eq!(
                problem(json!({"gender_form": raw})),
                ("gender_form", ProfileFieldProblem::UnknownValue),
                "{raw:?}"
            );
        }
        for raw in ["30s-40s", "20S", " teen", "", "50s+"] {
            assert_eq!(
                problem(json!({"age_band": raw})),
                ("age_band", ProfileFieldProblem::UnknownValue),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn enums_serialize_with_wire_spellings() {
        assert_eq!(
            serde_json::to_value(AgeBand::ThirtiesForties).unwrap(),
            "30s_40s"
        );
        assert_eq!(serde_json::to_value(GenderForm::None).unwrap(), "none");
        let back: AgeBand = serde_json::from_value(json!("50s_plus")).unwrap();
        assert_eq!(back, AgeBand::FiftiesPlus);
    }

    // ---- three-state patch and request shape -------------------------------------

    #[test]
    fn omitted_null_and_value_are_three_states() {
        let parsed = request(json!({"role": null, "intro": "hi"})).unwrap();
        assert_eq!(parsed.gender_form, FieldPatch::Keep);
        assert_eq!(parsed.role, FieldPatch::Clear);
        assert_eq!(parsed.intro, FieldPatch::Set("hi".to_owned()));
        assert!(!parsed.has_name());

        let update = parsed.validate().unwrap();
        assert_eq!(update.gender_form, FieldPatch::Keep);
        assert_eq!(update.role, FieldPatch::Clear);
        assert!(!update.has_name());
    }

    #[test]
    fn empty_object_is_an_empty_request() {
        let parsed = request(json!({})).unwrap();
        assert!(parsed.is_empty());
        assert_eq!(parsed.validate().unwrap(), ProfileUpdate::default());
    }

    #[test]
    fn name_null_is_rejected_on_validate() {
        let parsed = request(json!({"name": null})).unwrap();
        assert!(parsed.has_name(), "a null name still counts as sent");
        let err = parsed.validate().unwrap_err();
        assert_eq!(err.field(), "name");
        assert_eq!(err.problem, ProfileFieldProblem::CannotClear);
    }

    #[test]
    fn name_value_is_trimmed() {
        let update = validated(json!({"name": " 에이전트 A "})).unwrap();
        assert_eq!(update.name.as_deref(), Some("에이전트 A"));
        assert!(update.has_name());
    }

    #[test]
    fn first_violation_follows_field_order() {
        let body = json!({
            "intro": "a\tb",
            "role": "",
            "age_band": "old",
            "gender_form": "Female",
            "name": ""
        });
        assert_eq!(problem(body.clone()).0, "name");
        let mut without_name = body.as_object().unwrap().clone();
        without_name.remove("name");
        assert_eq!(
            problem(Value::Object(without_name.clone())).0,
            "gender_form"
        );
        without_name.remove("gender_form");
        assert_eq!(problem(Value::Object(without_name.clone())).0, "age_band");
        without_name.remove("age_band");
        assert_eq!(problem(Value::Object(without_name.clone())).0, "role");
        without_name.remove("role");
        assert_eq!(problem(Value::Object(without_name)).0, "intro");
    }

    #[test]
    fn non_string_types_are_parse_errors() {
        for (field, value) in [
            ("gender_form", json!(1)),
            ("age_band", json!({"x": 1})),
            ("role", json!(true)),
            ("intro", json!(["a"])),
            ("name", json!(42)),
        ] {
            let err = request(json!({ field: value })).unwrap_err();
            assert!(err.to_string().contains("invalid type"), "{field}: {err}");
        }
    }

    #[test]
    fn only_an_object_is_accepted() {
        for body in [json!([]), json!(["x"]), json!("x"), json!(null), json!(1)] {
            let err = request(body.clone()).unwrap_err();
            assert!(err.to_string().contains("a JSON object"), "{body}: {err}");
        }
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = request(json!({"pronouns": "x"})).unwrap_err();
        assert!(
            err.to_string().contains("unknown field `pronouns`"),
            "{err}"
        );
        let err = request(json!({"updated_at": "x"})).unwrap_err();
        assert!(
            err.to_string().contains("unknown field `updated_at`"),
            "{err}"
        );
    }

    #[test]
    fn serialization_skips_kept_fields_and_writes_null_for_cleared() {
        let mut body = UpdateProfileRequest::default();
        *body.patch_mut(ProfileField::Role) = FieldPatch::Clear;
        *body.patch_mut(ProfileField::GenderForm) = FieldPatch::Set("none".to_owned());
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            json!({"gender_form": "none", "role": null})
        );
        assert_eq!(
            serde_json::to_value(UpdateProfileRequest::default()).unwrap(),
            json!({})
        );
    }

    #[test]
    fn profile_serializes_every_key_with_nulls() {
        let profile = AgentProfile {
            gender_form: Some(GenderForm::None),
            ..AgentProfile::default()
        };
        assert_eq!(
            serde_json::to_value(&profile).unwrap(),
            json!({
                "gender_form": "none",
                "age_band": null,
                "role": null,
                "intro": null,
                "updated_at": null
            })
        );
    }

    #[test]
    fn profile_field_names_round_trip() {
        for field in std::iter::once(ProfileField::Name).chain(ProfileField::SELF_DECLARED) {
            assert_eq!(field.as_str().parse::<ProfileField>(), Ok(field));
        }
        assert!("Name".parse::<ProfileField>().is_err());
    }

    #[test]
    fn error_messages_do_not_repeat_the_value() {
        let secret = "VALUE-MARKER";
        for body in [
            json!({"name": format!("{secret}\tx")}),
            json!({"role": format!("{secret}{}", chars('a', MAX_ROLE_CHARS))}),
            json!({"gender_form": secret}),
        ] {
            let err = validated(body).unwrap_err();
            assert!(!err.to_string().contains(secret), "{err}");
        }
    }
}

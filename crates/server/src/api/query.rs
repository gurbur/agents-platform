//! Strict query-string parsing shared by the list endpoints (`IF-WIRE-005`):
//! an unknown parameter name is 422 `unexpected_field`, a repeated one is
//! 422 `invalid_field`, and values are checked only after the caller's own
//! permission step.

use std::collections::BTreeMap;

use platform_wire::messages::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use platform_wire::{Seq, codes};
use url::form_urlencoded;

use super::ApiError;

/// The recognised parameters of one request, still as decoded text.
#[derive(Debug, Default)]
pub struct QueryParams {
    values: BTreeMap<&'static str, String>,
}

impl QueryParams {
    /// Splits `query` (form-urlencoded, so `+` is a space) and keeps only the
    /// names in `allowed`.
    pub fn parse(query: Option<&str>, allowed: &[&'static str]) -> Result<Self, ApiError> {
        let mut values = BTreeMap::new();
        for (name, value) in form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
            let Some(&known) = allowed.iter().find(|known| **known == name) else {
                return Err(ApiError::unprocessable(
                    codes::UNEXPECTED_FIELD,
                    format!("unexpected query parameter `{name}`"),
                )
                .with_detail("field", name.as_ref()));
            };
            if values.insert(known, value.into_owned()).is_some() {
                return Err(ApiError::invalid_field(
                    known,
                    format!("query parameter `{known}` is repeated"),
                ));
            }
        }
        Ok(Self { values })
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn take(&mut self, name: &str) -> Option<String> {
        self.values.remove(name)
    }

    /// The optional sequence parameter `name` (`IF-ID-001` format).
    pub fn seq(&self, name: &str) -> Result<Option<Seq>, ApiError> {
        self.get(name).map(|raw| parse_seq(name, raw)).transpose()
    }

    /// `limit`: ASCII digits in `1..=MAX_PAGE_SIZE`, default `DEFAULT_PAGE_SIZE`.
    pub fn limit(&self) -> Result<u32, ApiError> {
        parse_limit(self.get("limit"))
    }
}

pub fn parse_seq(field: &str, raw: &str) -> Result<Seq, ApiError> {
    raw.parse()
        .map_err(|err: platform_wire::SeqError| ApiError::invalid_field(field, err.to_string()))
}

fn parse_limit(raw: Option<&str>) -> Result<u32, ApiError> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_PAGE_SIZE);
    };
    let invalid = || {
        ApiError::invalid_field(
            "limit",
            format!("limit must be an integer in 1..={MAX_PAGE_SIZE}"),
        )
    };
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let limit: u32 = raw.parse().map_err(|_| invalid())?;
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(invalid());
    }
    Ok(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: &[&str] = &["room_id", "q", "limit"];

    #[test]
    fn limit_defaults_and_bounds() {
        assert_eq!(parse_limit(None).unwrap(), DEFAULT_PAGE_SIZE);
        assert_eq!(parse_limit(Some("1")).unwrap(), 1);
        assert_eq!(
            parse_limit(Some(&MAX_PAGE_SIZE.to_string())).unwrap(),
            MAX_PAGE_SIZE
        );
        let over = (MAX_PAGE_SIZE + 1).to_string();
        for invalid in [
            "0",
            over.as_str(),
            "",
            "-1",
            "+5",
            "1.5",
            "ten",
            "99999999999",
        ] {
            let err = parse_limit(Some(invalid)).unwrap_err();
            assert_eq!(err.code(), codes::INVALID_FIELD, "{invalid:?}");
        }
    }

    #[test]
    fn unknown_and_repeated_parameters_are_rejected() {
        let unknown = QueryParams::parse(Some("room_id=all&offset=3"), NAMES).unwrap_err();
        assert_eq!(unknown.code(), codes::UNEXPECTED_FIELD);
        assert_eq!(unknown.details()["field"], "offset");
        let repeated = QueryParams::parse(Some("room_id=all&room_id=all"), NAMES).unwrap_err();
        assert_eq!(repeated.code(), codes::INVALID_FIELD);
        assert_eq!(repeated.details()["field"], "room_id");
        let mut decoded = QueryParams::parse(Some("room_id=all&q=a%26b+c"), NAMES).unwrap();
        assert_eq!(decoded.take("q").as_deref(), Some("a&b c"));
        assert_eq!(decoded.get("q"), None);
        assert!(
            QueryParams::parse(None, NAMES)
                .unwrap()
                .get("room_id")
                .is_none()
        );
    }

    #[test]
    fn seq_values_follow_the_wire_format() {
        let params = QueryParams::parse(Some("room_id=007&q=42"), NAMES).unwrap();
        let err = params.seq("room_id").unwrap_err();
        assert_eq!(err.code(), codes::INVALID_FIELD);
        assert_eq!(err.details()["field"], "room_id");
        assert_eq!(params.seq("q").unwrap().map(Seq::get), Some(42));
        assert_eq!(params.seq("limit").unwrap(), None);
    }
}

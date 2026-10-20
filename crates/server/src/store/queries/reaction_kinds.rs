//! The reaction kind registry (`IF-DB-001` v8 `reaction_kinds`;
//! `IF-WIRE-017`, ADR-0014).
//!
//! The owner's upsert is the only write and there is no DELETE: a kind is
//! turned off with `enabled = 0`, which refuses new reactions only.

use platform_wire::{ReactionKind, ReactionKindEntry, ReactionKindUpdate};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::store::StoreError;

/// How far after the largest `sort` a new kind without one goes.
const SORT_STEP: i64 = 10;

const KIND_COLUMNS: &str = "slug, glyph, label, sort, enabled";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// A new slug (201).
    Created(ReactionKindEntry),
    /// An existing slug, changed or not (200).
    Updated(ReactionKindEntry),
    /// Another slug already uses the glyph (409 `glyph_taken`).
    GlyphTaken,
}

/// Every kind, or only the enabled ones, in `sort` then `slug` order.
pub fn list(
    conn: &Connection,
    include_disabled: bool,
) -> Result<Vec<ReactionKindEntry>, StoreError> {
    let mut statement = conn.prepare(&format!(
        "SELECT {KIND_COLUMNS} FROM reaction_kinds
         WHERE ?1 OR enabled = 1
         ORDER BY sort, slug"
    ))?;
    let rows = statement.query_map(params![include_disabled], StoredKind::from_row)?;
    let mut kinds = Vec::new();
    for row in rows {
        kinds.push(row?.into_entry()?);
    }
    Ok(kinds)
}

/// Whether `slug` is registered and enabled.
pub fn is_enabled(conn: &Connection, slug: &ReactionKind) -> Result<bool, StoreError> {
    let enabled: Option<bool> = conn
        .query_row(
            "SELECT enabled FROM reaction_kinds WHERE slug = ?1",
            params![slug.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(enabled == Some(true))
}

/// `PUT /v1/reaction-kinds/{slug}`: creates the kind (an omitted `sort`
/// goes [`SORT_STEP`] after the largest, an omitted `enabled` is `true`) or
/// updates it (`glyph` and `label` replaced, an omitted `sort` or `enabled`
/// kept; nothing written when nothing changes). The glyph check excludes
/// the kind itself.
pub fn upsert(
    tx: &Transaction,
    slug: &ReactionKind,
    update: &ReactionKindUpdate,
    now: &str,
) -> Result<UpsertOutcome, StoreError> {
    let taken: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM reaction_kinds WHERE glyph = ?1 AND slug <> ?2",
            params![update.glyph, slug.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if taken.is_some() {
        return Ok(UpsertOutcome::GlyphTaken);
    }
    let Some(current) = find(tx, slug)? else {
        let sort = match update.sort {
            Some(sort) => sort,
            None => next_sort(tx)?,
        };
        let entry = ReactionKindEntry {
            slug: slug.clone(),
            glyph: update.glyph.clone(),
            label: update.label.clone(),
            sort,
            enabled: update.enabled.unwrap_or(true),
        };
        tx.execute(
            "INSERT INTO reaction_kinds (slug, glyph, label, sort, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.slug.as_str(),
                entry.glyph,
                entry.label,
                entry.sort,
                entry.enabled,
                now
            ],
        )?;
        return Ok(UpsertOutcome::Created(entry));
    };
    let changed = ReactionKindEntry {
        slug: slug.clone(),
        glyph: update.glyph.clone(),
        label: update.label.clone(),
        sort: update.sort.unwrap_or(current.sort),
        enabled: update.enabled.unwrap_or(current.enabled),
    };
    if changed != current {
        tx.execute(
            "UPDATE reaction_kinds SET glyph = ?2, label = ?3, sort = ?4, enabled = ?5
             WHERE slug = ?1",
            params![
                changed.slug.as_str(),
                changed.glyph,
                changed.label,
                changed.sort,
                changed.enabled
            ],
        )?;
    }
    Ok(UpsertOutcome::Updated(changed))
}

fn find(conn: &Connection, slug: &ReactionKind) -> Result<Option<ReactionKindEntry>, StoreError> {
    let stored = conn
        .query_row(
            &format!("SELECT {KIND_COLUMNS} FROM reaction_kinds WHERE slug = ?1"),
            params![slug.as_str()],
            StoredKind::from_row,
        )
        .optional()?;
    stored.map(StoredKind::into_entry).transpose()
}

/// [`SORT_STEP`] after the largest `sort` (the step alone when there is no
/// kind at all).
fn next_sort(conn: &Connection) -> Result<i64, StoreError> {
    let largest: Option<i64> =
        conn.query_row("SELECT MAX(sort) FROM reaction_kinds", [], |row| row.get(0))?;
    Ok(largest.unwrap_or(0).saturating_add(SORT_STEP))
}

pub(crate) fn stored_kind(raw: String) -> Result<ReactionKind, StoreError> {
    ReactionKind::try_from(raw)
        .map_err(|err| StoreError::Invariant(format!("stored reaction kind is invalid: {err}")))
}

/// A [`KIND_COLUMNS`] row before validation.
struct StoredKind {
    slug: String,
    glyph: String,
    label: String,
    sort: i64,
    enabled: bool,
}

impl StoredKind {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            slug: row.get(0)?,
            glyph: row.get(1)?,
            label: row.get(2)?,
            sort: row.get(3)?,
            enabled: row.get(4)?,
        })
    }

    fn into_entry(self) -> Result<ReactionKindEntry, StoreError> {
        Ok(ReactionKindEntry {
            slug: stored_kind(self.slug)?,
            glyph: self.glyph,
            label: self.label,
            sort: self.sort,
            enabled: self.enabled,
        })
    }
}

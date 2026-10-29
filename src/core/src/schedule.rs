//! Turns recurring series into issues a few weeks ahead of their occurrences.
//!
//! There is no background job: requests call `generate_pending`, which only does work
//! when a series has not been generated up to the horizon yet.

use crate::db::{DbError, Runtime};
use crate::domain::Series;
use crate::recurrence::{format_date, horizon, local_date};
use crate::store;

/// Bounds the extra work one request may pick up.
const SERIES_PER_RUN: i64 = 5;

pub async fn generate_pending<R: Runtime>(rt: &R, site_tz: chrono_tz::Tz) -> Result<(), DbError> {
    let now = rt.now_ms();
    let due = format_date(horizon(now, site_tz));
    for series in store::pending_series(rt, &due, SERIES_PER_RUN).await? {
        generate(rt, &series, now).await?;
    }
    Ok(())
}

/// Generates the series' occurrences from today (or its last edit) up to the horizon.
pub async fn generate<R: Runtime>(rt: &R, series: &Series, now: i64) -> Result<(), DbError> {
    let tz = series.timezone;
    let until = horizon(now, tz);
    // Days nobody visited the site are skipped rather than filled in afterwards.
    let mut from = local_date(now, tz)
        .max(local_date(series.effective_from, tz))
        .max(series.rule.start_date);
    if let Some(done) = series.generated_until.and_then(|d| d.succ_opt()) {
        from = from.max(done);
    }
    let existing = store::existing_occurrences(rt, &series.id, &format_date(from)).await?;
    let occurrences: Vec<_> = series
        .rule
        .occurrences(from, until, tz)
        .into_iter()
        .filter(|(date, start)| {
            *start > series.effective_from && !existing.contains(&format_date(*date))
        })
        .collect();
    match store::generate_occurrences(rt, series, &occurrences, until).await {
        // Another request generated them at the same moment.
        Ok(()) | Err(DbError::UniqueViolation) => Ok(()),
        Err(e) => Err(e),
    }
}

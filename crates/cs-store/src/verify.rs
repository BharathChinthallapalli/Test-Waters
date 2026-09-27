//! Chain and global-order verification (R4.5, R4.6, R6.4).
//!
//! Owned by unit `verify`: scans `events` by `global_pos` in chunks of
//! [`CHUNK_EVENTS`], each a short read under the shared read gate
//! ([`Store::read`]), and reports the first problem as a
//! `cs_core::control::VerifyProblem`. Erased content is reported, not a failure.
//!
//! # What is checked
//! Every event, in global order, against the events before it:
//! 1. its `global_pos` is the previous one plus 1, from 1
//!    ([`VerifyProblemKind::GlobalPositionGap`], reported at the first stored
//!    event after the gap or repeat, since a removed event has no row to name);
//! 2. its `seq` is its run's previous `seq` plus 1, from 1
//!    ([`VerifyProblemKind::SequenceGap`]);
//! 3. its `prev_hash` is its run's previous `event_hash`, or [`ZERO_HASH`] for
//!    `seq = 1` ([`VerifyProblemKind::PrevHashMismatch`]);
//! 4. its body is canonical JSON (RFC 8785, as the writer stores it) and
//!    [`cs_core::event::event_hash`] over the stored row gives the stored
//!    `event_hash` ([`VerifyProblemKind::EventHashMismatch`]); a body that
//!    isn't JSON, or a row whose columns don't hold their declared types, is
//!    reported the same way. The canonical check matters because a JSON
//!    parser may read rewritten text, such as a duplicated key, as the value
//!    that was hashed while other readers see something else;
//! 5. its content references: the addresses in the hashed body's `content`
//!    array and its `event_content` rows name the same set, and every address
//!    has a blob or is explained by an erasure (below); otherwise
//!    [`VerifyProblemKind::ContentMissing`].
//!
//! Only once every event passed, the `runs` table is compared with the last
//! event of each run, in the same read transaction as the last chunk: a
//! `last_seq`/`last_hash` that differs, a `runs` row with no events (reported
//! at `global_pos` 0, as there is no event to name) and events whose run has no
//! `runs` row are [`VerifyProblemKind::RunHeadMismatch`], the one with the
//! smallest `global_pos` first. A run head only summarises its chain, so a
//! broken chain is the more precise report.
//!
//! The scan goes on after the first problem, so `eventsChecked` counts every
//! event and `erasedEvents` every event whose content was erased.
//!
//! # Erased content (R6.4, ADR 0011)
//! Erasure (unit `erase`) deletes blobs and appends one event of kind
//! [`CONTENT_ERASED_KIND`] to the erased run; its body lists the erased
//! addresses under `"addresses"` and the other runs that shared that content
//! under `"affectedRuns"`. A missing blob is erased, not missing, if an erasure
//! event with a **later** `global_pos` than the referencing event lists its
//! address **and** either belongs to the referencing event's run or names that
//! run in `"affectedRuns"`. (The design's "in the same run" alone would flag
//! the runs that shared the content; this is a deliberate deviation.) Asking
//! for the run narrows what a forged erasure explains: an unrelated run's
//! event rewritten into an erasure (see the limits below) explains only the
//! runs it names. An erasure before the reference explains nothing: that
//! content was stored again after it. `erasedEvents` counts the events with at
//! least one such erased address.
//!
//! Erasure events are gathered first, in their own chunked pass, into a map
//! from each erased address to the erasure events listing it, each kept once
//! with its position and runs, so memory grows with the erased addresses and
//! erasures, not with the events. An erasure committed while the
//! verification runs is picked up when a later chunk first meets a blob it
//! removed (the blob's absence and the erasure event commit together): that
//! chunk reads the erasure events committed since, once.
//!
//! Verification can't tell an erasure event appended by erasure from one
//! appended by any other caller of `Store::append` with the same kind;
//! reserving the kind for erasure is up to the writer.
//!
//! # Concurrency and memory
//! Each chunk is its own read transaction, so a long verification holds the
//! read gate, and so delays an erasure, for one chunk at a time. Events are
//! append-only, so later chunks only see more events, never changed ones.
//! Appends that land during the scan are verified too: the scan ends with the
//! first chunk that reaches the end of its snapshot, and checks `runs` in that
//! same transaction, so heads and events are compared at one point in time.
//! Verifying is far faster than appending (every append is a durable commit on
//! one writer thread), so that normally happens within a chunk or two of the
//! position that was newest when verification started. So that the scan ends
//! even if appends kept up, after [`MAX_TAIL_CHUNKS`] full chunks past that
//! position the next chunk reads to the end of its snapshot without a limit.
//! State carried between chunks is one head per run and the erased addresses.
//!
//! # Limits (R4.6)
//! Until feature 04's signed checkpoints anchor the chains outside the
//! database, two changes leave no trace, because nothing later commits to what
//! they change:
//! - events removed from the end of the global order, with the run heads set
//!   back to match;
//! - an edit of the newest event of any run, with its `event_hash` and the
//!   run's `runs.last_hash` recomputed.
//!
//! SECURITY.md and PRIVACY.md say so. The fix is feature 04's anchors, not
//! more checks here.
//!
//! An erasure event is trusted before its own hash is checked, since the
//! first pass reads it before the scan reaches it. Tampering with an erasure
//! body is therefore reported at the erasure event
//! ([`VerifyProblemKind::EventHashMismatch`]) rather than at the event whose
//! missing content it explains; the result is still `ok: false`.

use std::collections::{BTreeSet, HashMap, HashSet};

use cs_core::control::{VerifyProblem, VerifyProblemKind, VerifyResult};
use cs_core::event::{self, HashedEvent, ZERO_HASH};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, Row, params};
use serde_json::Value;

use crate::writer::{CONTENT_FIELD, Store, StoreError};

/// Events per chunk, each chunk one read transaction.
pub const CHUNK_EVENTS: usize = 10_000;

/// The kind of the event erasure appends (contract with unit `erase`).
pub const CONTENT_ERASED_KIND: &str = "content.erased";

/// The member of a [`CONTENT_ERASED_KIND`] body that lists the erased
/// addresses (contract with unit `erase`).
pub const ERASED_ADDRESSES_FIELD: &str = "addresses";

/// The member of a [`CONTENT_ERASED_KIND`] body that lists the other runs
/// whose content the erasure removed (contract with unit `erase`).
pub const AFFECTED_RUNS_FIELD: &str = "affectedRuns";

/// Full chunks read past the position that was newest when verification
/// started before the next chunk reads without a limit; see the module docs.
pub const MAX_TAIL_CHUNKS: u32 = 8;

/// `LIMIT -1` is no limit in SQLite.
const NO_LIMIT: i64 = -1;

/// Verifies every chain, the global order, the run heads and the content
/// references. See the module docs.
///
/// Fails only if the database can't be read; everything it finds wrong in the
/// data is reported in the result.
pub async fn verify(store: &Store) -> Result<VerifyResult, StoreError> {
    let mut erasures = Erasures::default();
    while !erasures.gathered {
        erasures = store
            .read(move |conn| {
                erasures.gather_chunk(conn)?;
                Ok(erasures)
            })
            .await?;
    }
    let mut scan = Scan::new(erasures);
    while !scan.finished {
        scan = store
            .read(move |conn| {
                scan.chunk(conn)?;
                Ok(scan)
            })
            .await?;
    }
    Ok(scan.into_result())
}

/// One erasure event: its position and the runs whose references it explains
/// (its own run and the ones its body names under `"affectedRuns"`).
#[derive(Debug)]
struct Erasure {
    pos: i64,
    runs: HashSet<String>,
}

/// The erasure events read so far, by the addresses they list.
#[derive(Debug)]
struct Erasures {
    events: Vec<Erasure>,
    /// For each erased address, the indexes into `events` of the erasures
    /// listing it.
    by_address: HashMap<String, Vec<usize>>,
    /// First position not looked at yet; `None` once an event at `i64::MAX`
    /// was read, as none can follow it.
    next_from: Option<i64>,
    /// The newest position when gathering started; `None` before.
    end: Option<i64>,
    /// The first pass is done.
    gathered: bool,
    /// [`Erasures::catch_up`] ran in the current read transaction.
    caught_up: bool,
}

impl Default for Erasures {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            by_address: HashMap::new(),
            next_from: Some(i64::MIN),
            end: None,
            gathered: false,
            caught_up: false,
        }
    }
}

impl Erasures {
    /// Reads the erasure events among up to [`CHUNK_EVENTS`] events, up to
    /// the position that was the newest when gathering started.
    fn gather_chunk(&mut self, conn: &Connection) -> rusqlite::Result<()> {
        let end = match self.end {
            Some(end) => end,
            None => match newest_position(conn)? {
                Some(end) => *self.end.insert(end),
                None => {
                    self.gathered = true;
                    return Ok(());
                }
            },
        };
        let count = self.read_range(conn, end, chunk_limit())?;
        self.gathered =
            count < CHUNK_EVENTS || self.next_from.is_none_or(|next_from| next_from > end);
        Ok(())
    }

    /// Reads the erasure events committed since the first pass, up to the end
    /// of the current snapshot. Runs at most once per read transaction (see
    /// [`Erasures::new_transaction`]): within one, the snapshot doesn't change.
    fn catch_up(&mut self, conn: &Connection) -> rusqlite::Result<()> {
        if self.caught_up {
            return Ok(());
        }
        self.caught_up = true;
        if let Some(newest) = newest_position(conn)? {
            self.read_range(conn, newest, NO_LIMIT)?;
        }
        Ok(())
    }

    /// A new read transaction started; [`Erasures::catch_up`] may run again.
    fn new_transaction(&mut self) {
        self.caught_up = false;
    }

    /// Records the erasure events among up to `limit` events from
    /// `next_from` to `end`, moves `next_from` past them, and returns how many
    /// events it looked at.
    fn read_range(&mut self, conn: &Connection, end: i64, limit: i64) -> rusqlite::Result<usize> {
        let Some(from) = self.next_from else {
            return Ok(0);
        };
        let mut statement = conn.prepare_cached(
            "SELECT global_pos, CASE WHEN kind = ?1 THEN run_id END, CASE WHEN kind = ?1 THEN body END
             FROM events WHERE global_pos >= ?2 AND global_pos <= ?3 ORDER BY global_pos LIMIT ?4",
        )?;
        let mut rows = statement.query(params![CONTENT_ERASED_KIND, from, end, limit])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            let pos: i64 = row.get(0)?;
            if let Some(body) = text(row, 2)? {
                self.record(pos, text(row, 1)?, &body);
            }
            self.next_from = pos.checked_add(1);
        }
        Ok(count)
    }

    /// Notes the addresses an erasure event at `pos` in `run_id` lists, and
    /// the runs it explains them for. A body that isn't an erasure body
    /// explains nothing.
    fn record(&mut self, pos: i64, run_id: Option<String>, body: &str) {
        let Ok(Value::Object(members)) = serde_json::from_str::<Value>(body) else {
            return;
        };
        let Some(Value::Array(addresses)) = members.get(ERASED_ADDRESSES_FIELD) else {
            return;
        };
        let mut runs: HashSet<String> = match members.get(AFFECTED_RUNS_FIELD) {
            Some(Value::Array(affected)) => affected
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => HashSet::new(),
        };
        runs.extend(run_id);
        let index = self.events.len();
        let mut listed = false;
        for address in addresses.iter().filter_map(Value::as_str) {
            let erasures = self.by_address.entry(address.to_owned()).or_default();
            if erasures.last() != Some(&index) {
                erasures.push(index);
            }
            listed = true;
        }
        if listed {
            self.events.push(Erasure { pos, runs });
        }
    }

    /// An erasure after `pos` lists `address` and explains it for `run_id`.
    fn explains(&self, address: &str, pos: i64, run_id: Option<&str>) -> bool {
        let Some(run_id) = run_id else {
            return false;
        };
        self.by_address.get(address).is_some_and(|indexes| {
            indexes
                .iter()
                .filter_map(|&index| self.events.get(index))
                .any(|erasure| erasure.pos > pos && erasure.runs.contains(run_id))
        })
    }
}

/// The last event of a run seen so far.
#[derive(Debug)]
struct Head {
    seq: i64,
    hash: String,
    pos: i64,
}

/// An event row as stored. A column that doesn't hold the type the schema
/// declares reads as `None`.
struct StoredEvent {
    pos: i64,
    run_id: Option<String>,
    seq: Option<i64>,
    kind: Option<String>,
    ts_ms: Option<i64>,
    body: Option<String>,
    prev_hash: Option<String>,
    event_hash: Option<String>,
}

impl StoredEvent {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            pos: row.get(0)?,
            run_id: text(row, 1)?,
            seq: integer(row, 2)?,
            kind: text(row, 3)?,
            ts_ms: integer(row, 4)?,
            body: text(row, 5)?,
            prev_hash: text(row, 6)?,
            event_hash: text(row, 7)?,
        })
    }
}

/// An event whose every column holds its declared type.
struct Event<'a> {
    pos: i64,
    run_id: &'a str,
    seq: i64,
    kind: &'a str,
    ts_ms: i64,
    body: &'a str,
    prev_hash: &'a str,
    event_hash: &'a str,
}

impl<'a> Event<'a> {
    fn from_stored(stored: &'a StoredEvent) -> Option<Self> {
        Some(Self {
            pos: stored.pos,
            run_id: stored.run_id.as_deref()?,
            seq: stored.seq?,
            kind: stored.kind.as_deref()?,
            ts_ms: stored.ts_ms?,
            body: stored.body.as_deref()?,
            prev_hash: stored.prev_hash.as_deref()?,
            event_hash: stored.event_hash.as_deref()?,
        })
    }
}

/// What checking one event's content references found.
#[derive(Debug, Default)]
struct ContentCheck {
    /// A reference is missing, unreadable or not explained by an erasure.
    problem: bool,
    /// At least one referenced blob was erased.
    erased: bool,
}

/// The scan's state, carried from one chunk (read transaction) to the next.
#[derive(Debug)]
struct Scan {
    erasures: Erasures,
    /// First position the next chunk reads from; `None` once an event at
    /// `i64::MAX` was read, as none can follow it.
    next_from: Option<i64>,
    /// The first position that was not there when verification started;
    /// `None` if none can follow.
    tail_from: Option<i64>,
    /// Chunks started at or past `tail_from`.
    tail_chunks: u32,
    /// The position the next event should have.
    expected_pos: i64,
    heads: HashMap<String, Head>,
    events_checked: u64,
    erased_events: u64,
    first_problem: Option<VerifyProblem>,
    finished: bool,
}

impl Scan {
    fn new(erasures: Erasures) -> Self {
        Self {
            tail_from: erasures.next_from,
            tail_chunks: 0,
            erasures,
            next_from: Some(i64::MIN),
            expected_pos: 1,
            heads: HashMap::new(),
            events_checked: 0,
            erased_events: 0,
            first_problem: None,
            finished: false,
        }
    }

    /// Checks the next chunk; once it reaches the end of the log, checks the
    /// run heads in the same transaction.
    fn chunk(&mut self, conn: &Connection) -> rusqlite::Result<()> {
        self.erasures.new_transaction();
        let Some(from) = self.next_from else {
            return self.finish(conn);
        };
        if self.tail_from.is_some_and(|tail_from| from >= tail_from) {
            self.tail_chunks += 1;
        }
        let limit = if self.tail_chunks > MAX_TAIL_CHUNKS {
            NO_LIMIT
        } else {
            chunk_limit()
        };
        let mut statement = conn.prepare_cached(
            "SELECT global_pos, run_id, seq, kind, ts_ms, body, prev_hash, event_hash
             FROM events WHERE global_pos >= ?1 ORDER BY global_pos LIMIT ?2",
        )?;
        let mut rows = statement.query(params![from, limit])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            let stored = StoredEvent::read(row)?;
            self.next_from = stored.pos.checked_add(1);
            self.check_event(conn, &stored)?;
        }
        if limit == NO_LIMIT || count < CHUNK_EVENTS || self.next_from.is_none() {
            self.finish(conn)?;
        }
        Ok(())
    }

    /// The last event was read: checks the run heads in the same transaction.
    fn finish(&mut self, conn: &Connection) -> rusqlite::Result<()> {
        if self.first_problem.is_none() {
            self.first_problem = self.run_head_problem(conn)?;
        }
        self.finished = true;
        Ok(())
    }

    fn check_event(&mut self, conn: &Connection, stored: &StoredEvent) -> rusqlite::Result<()> {
        self.events_checked += 1;
        let run_id = stored.run_id.as_deref().unwrap_or_default();
        if stored.pos != self.expected_pos {
            self.report(VerifyProblemKind::GlobalPositionGap, stored.pos, run_id);
        }
        self.expected_pos = stored.pos.saturating_add(1);

        let body = stored
            .body
            .as_deref()
            .and_then(|body| serde_json::from_str::<Value>(body).ok());
        let chain_problem = match Event::from_stored(stored) {
            Some(event) => self.chain_problem(&event, body.as_ref()),
            // A column that doesn't hold its declared type wasn't hashed so.
            None => Some(VerifyProblemKind::EventHashMismatch),
        };
        if let Some(kind) = chain_problem {
            self.report(kind, stored.pos, run_id);
        }
        if let (Some(run_id), Some(seq), Some(hash)) =
            (&stored.run_id, stored.seq, &stored.event_hash)
        {
            let head = Head {
                seq,
                hash: hash.clone(),
                pos: stored.pos,
            };
            self.heads.insert(run_id.clone(), head);
        }

        let content =
            self.check_content(conn, stored.pos, stored.run_id.as_deref(), body.as_ref())?;
        if content.erased {
            self.erased_events += 1;
        }
        if content.problem {
            self.report(VerifyProblemKind::ContentMissing, stored.pos, run_id);
        }
        Ok(())
    }

    /// The first thing wrong with the event's place in its run's chain or with
    /// its hash. `body` is the stored body parsed, `None` if it isn't JSON.
    fn chain_problem(&self, event: &Event<'_>, body: Option<&Value>) -> Option<VerifyProblemKind> {
        let head = self.heads.get(event.run_id);
        let expected_seq = head.map_or(Some(1), |head| head.seq.checked_add(1));
        if Some(event.seq) != expected_seq {
            return Some(VerifyProblemKind::SequenceGap);
        }
        let expected_prev = head.map_or(ZERO_HASH, |head| head.hash.as_str());
        if event.prev_hash != expected_prev {
            return Some(VerifyProblemKind::PrevHashMismatch);
        }
        let Some(body) = body else {
            return Some(VerifyProblemKind::EventHashMismatch);
        };
        let canonical = event::canonical_json(body).ok();
        if canonical.as_deref() != Some(event.body.as_bytes())
            || recomputed_hash(event, body).as_deref() != Some(event.event_hash)
        {
            return Some(VerifyProblemKind::EventHashMismatch);
        }
        None
    }

    /// Checks that the body's `content` array and the `event_content` rows
    /// agree, and that every address has a blob or was erased later for the
    /// event's run.
    fn check_content(
        &mut self,
        conn: &Connection,
        pos: i64,
        run_id: Option<&str>,
        body: Option<&Value>,
    ) -> rusqlite::Result<ContentCheck> {
        let mut check = ContentCheck::default();
        let listed = body.map(listed_addresses);
        let indexed = indexed_addresses(conn, pos)?;
        let mut addresses = BTreeSet::new();
        match (&listed, &indexed) {
            (Some(Some(listed)), Some(indexed)) => {
                check.problem = listed != indexed;
                addresses.extend(listed.iter().chain(indexed));
            }
            // The body isn't JSON (reported as a hash problem) and the index reads.
            (None, Some(indexed)) => addresses.extend(indexed),
            // The body's `content` member or an index row is malformed.
            (Some(Some(listed)), None) => {
                check.problem = true;
                addresses.extend(listed);
            }
            (Some(None), Some(indexed)) => {
                check.problem = true;
                addresses.extend(indexed);
            }
            _ => check.problem = true,
        }

        for address in addresses {
            if has_blob(conn, address)? {
                continue;
            }
            if !self.erasures.explains(address, pos, run_id) {
                // An erasure committed since the first pass may explain it.
                self.erasures.catch_up(conn)?;
            }
            if self.erasures.explains(address, pos, run_id) {
                check.erased = true;
            } else {
                check.problem = true;
            }
        }
        Ok(check)
    }

    /// The run-head problem with the smallest position, if any. Runs after
    /// the last event, in the same transaction.
    fn run_head_problem(&self, conn: &Connection) -> rusqlite::Result<Option<VerifyProblem>> {
        let mut first: Option<(i64, String)> = None;
        let mut note = |pos: i64, run_id: &str| {
            let candidate = (pos, run_id.to_owned());
            if first.as_ref().is_none_or(|first| candidate < *first) {
                first = Some(candidate);
            }
        };
        let mut listed = HashSet::new();
        let mut statement = conn.prepare_cached("SELECT run_id, last_seq, last_hash FROM runs")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let Some(run_id) = text(row, 0)? else {
                note(0, "");
                continue;
            };
            let last_seq = integer(row, 1)?;
            let last_hash = text(row, 2)?;
            match self.heads.get(&run_id) {
                None => note(0, &run_id),
                Some(head) => {
                    if last_seq != Some(head.seq) || last_hash.as_deref() != Some(&head.hash) {
                        note(head.pos, &run_id);
                    }
                }
            }
            listed.insert(run_id);
        }
        for (run_id, head) in &self.heads {
            if !listed.contains(run_id) {
                note(head.pos, run_id);
            }
        }
        Ok(first.map(|(pos, run_id)| problem(VerifyProblemKind::RunHeadMismatch, pos, &run_id)))
    }

    /// Keeps the first problem only.
    fn report(&mut self, kind: VerifyProblemKind, pos: i64, run_id: &str) {
        if self.first_problem.is_none() {
            self.first_problem = Some(problem(kind, pos, run_id));
        }
    }

    fn into_result(self) -> VerifyResult {
        VerifyResult {
            ok: self.first_problem.is_none(),
            events_checked: self.events_checked,
            erased_events: self.erased_events,
            first_problem: self.first_problem,
        }
    }
}

fn problem(kind: VerifyProblemKind, pos: i64, run_id: &str) -> VerifyProblem {
    VerifyProblem {
        kind,
        // A position below 1 is itself a gap; it is reported as 0.
        global_pos: u64::try_from(pos).unwrap_or(0),
        run_id: run_id.to_owned(),
    }
}

/// The event's hash as it should be, or `None` if a field can't be hashed.
fn recomputed_hash(event: &Event<'_>, body: &Value) -> Option<String> {
    event::event_hash(&HashedEvent {
        run_id: event.run_id,
        seq: u64::try_from(event.seq).ok()?,
        global_pos: u64::try_from(event.pos).ok()?,
        kind: event.kind,
        ts_ms: u64::try_from(event.ts_ms).ok()?,
        body,
        prev_hash: event.prev_hash,
    })
    .ok()
}

/// The distinct addresses in the body's `content` array: empty without one,
/// `None` if it isn't an array of strings.
fn listed_addresses(body: &Value) -> Option<BTreeSet<String>> {
    match body.get(CONTENT_FIELD) {
        None => Some(BTreeSet::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect(),
        Some(_) => None,
    }
}

/// The event's `event_content` addresses; `None` if one isn't text.
fn indexed_addresses(conn: &Connection, pos: i64) -> rusqlite::Result<Option<BTreeSet<String>>> {
    let mut statement =
        conn.prepare_cached("SELECT address FROM event_content WHERE global_pos = ?1")?;
    let mut rows = statement.query([pos])?;
    let mut addresses = BTreeSet::new();
    while let Some(row) = rows.next()? {
        match text(row, 0)? {
            Some(address) => addresses.insert(address),
            None => return Ok(None),
        };
    }
    Ok(Some(addresses))
}

fn has_blob(conn: &Connection, address: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached("SELECT EXISTS (SELECT 1 FROM blobs WHERE address = ?1)")?
        .query_row([address], |row| row.get(0))
}

/// The newest position; `None` for an empty log.
fn newest_position(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.prepare_cached("SELECT max(global_pos) FROM events")?
        .query_row([], |row| row.get(0))
}

/// [`CHUNK_EVENTS`] as an SQL integer.
fn chunk_limit() -> i64 {
    i64::try_from(CHUNK_EVENTS).unwrap_or(i64::MAX)
}

/// A text column; `None` if it holds another type or text that isn't UTF-8.
fn text(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<String>> {
    Ok(match row.get_ref(index)? {
        ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok().map(str::to_owned),
        _ => None,
    })
}

/// An integer column; `None` if it holds another type.
fn integer(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<i64>> {
    Ok(match row.get_ref(index)? {
        ValueRef::Integer(value) => Some(value),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;
    use crate::content::{ContentKey, content_address};
    use crate::db;
    use crate::secrets::{InMemorySecretStore, KEY_LEN};
    use crate::writer::AppendEvent;

    const KEY: [u8; KEY_LEN] = [42; KEY_LEN];
    const TS: u64 = 1_790_000_000_000;

    fn open(dir: &Path) -> Store {
        Store::open(dir, Arc::new(InMemorySecretStore::new(KEY))).unwrap()
    }

    fn event(run: &str, n: u64) -> AppendEvent {
        AppendEvent {
            run_id: run.to_owned(),
            kind: "test.event".to_owned(),
            ts_ms: TS + n,
            body: json!({ "n": n }),
            content: Vec::new(),
        }
    }

    fn with_content(run: &str, n: u64, items: &[&[u8]]) -> AppendEvent {
        AppendEvent {
            content: items.iter().map(|item| item.to_vec()).collect(),
            ..event(run, n)
        }
    }

    /// The event erasure (unit `erase`) appends, per the contract in the
    /// module docs.
    fn erasure(run: &str, addresses: &[&str], affected: &[&str]) -> AppendEvent {
        AppendEvent {
            run_id: run.to_owned(),
            kind: CONTENT_ERASED_KIND.to_owned(),
            ts_ms: TS,
            body: json!({ "addresses": addresses, "affectedRuns": affected, "planId": "00" }),
            content: Vec::new(),
        }
    }

    fn address(bytes: &[u8]) -> String {
        content_address(&ContentKey::take(&mut { KEY }), bytes)
    }

    /// Eight events, alternating runs `a` and `b`: odd positions are `a`,
    /// even ones `b`, so position `p` has `seq = (p + 1) / 2`.
    async fn two_runs(dir: &Path) -> Store {
        let store = open(dir);
        for n in 1..=8 {
            let run = if n % 2 == 1 { "a" } else { "b" };
            store.append(event(run, n)).await.unwrap();
        }
        store
    }

    /// A raw connection to a closed store's database with the append-only
    /// triggers dropped, as someone tampering with the file would.
    fn raw(dir: &Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(db::database_path(dir)).unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER IF EXISTS events_no_update;
             DROP TRIGGER IF EXISTS events_no_delete;",
        )
        .unwrap();
        conn
    }

    /// Closes `store`, lets `tamper` change the file, and opens it again.
    async fn tampered(
        store: Store,
        dir: &Path,
        tamper: impl FnOnce(&rusqlite::Connection),
    ) -> Store {
        store.close().await.unwrap();
        drop(store);
        let conn = raw(dir);
        tamper(&conn);
        drop(conn);
        open(dir)
    }

    fn sql(statements: &'static str) -> impl FnOnce(&rusqlite::Connection) {
        move |conn| conn.execute_batch(statements).unwrap()
    }

    fn hash_at(conn: &rusqlite::Connection, pos: i64) -> String {
        conn.query_row(
            "SELECT event_hash FROM events WHERE global_pos = ?1",
            [pos],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// Writes `row` with its hash computed, the way a careful forger would.
    fn insert_hashed(conn: &rusqlite::Connection, row: &HashedEvent<'_>) -> String {
        let hash = event::event_hash(row).unwrap();
        let body = String::from_utf8(event::canonical_json(row.body).unwrap()).unwrap();
        conn.execute(
            "INSERT INTO events (global_pos, run_id, seq, kind, ts_ms, body, prev_hash, event_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                row.global_pos as i64,
                row.run_id,
                row.seq as i64,
                row.kind,
                row.ts_ms as i64,
                body,
                row.prev_hash,
                hash
            ],
        )
        .unwrap();
        hash
    }

    fn problem_at(kind: VerifyProblemKind, pos: u64, run: &str) -> Option<VerifyProblem> {
        Some(VerifyProblem {
            kind,
            global_pos: pos,
            run_id: run.to_owned(),
        })
    }

    async fn verified(store: &Store) -> VerifyResult {
        let result = verify(store).await.unwrap();
        println!("{result:?}");
        assert_eq!(result.ok, result.first_problem.is_none());
        result
    }

    #[tokio::test]
    async fn an_empty_log_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        let result = verified(&store).await;
        assert_eq!(
            result,
            VerifyResult {
                ok: true,
                events_checked: 0,
                erased_events: 0,
                first_problem: None,
            }
        );
    }

    #[tokio::test]
    async fn a_clean_log_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let result = verified(&store).await;
        assert_eq!(
            result,
            VerifyResult {
                ok: true,
                events_checked: 8,
                erased_events: 0,
                first_problem: None,
            }
        );
    }

    #[tokio::test]
    async fn an_edited_field_is_an_event_hash_mismatch_at_that_event() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql(r#"UPDATE events SET body = '{"n":999}' WHERE global_pos = 5"#),
        )
        .await;
        let result = verified(&store).await;
        assert_eq!(
            result.first_problem,
            problem_at(VerifyProblemKind::EventHashMismatch, 5, "a")
        );
        assert_eq!(result.events_checked, 8);

        // Any hashed field: here the timestamp.
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql("UPDATE events SET ts_ms = ts_ms + 1 WHERE global_pos = 6"),
        )
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::EventHashMismatch, 6, "b")
        );
    }

    #[tokio::test]
    async fn a_body_that_is_not_its_canonical_json_is_an_event_hash_mismatch() {
        for body in [
            // Not JSON at all.
            "garbage",
            // Parses to the hashed value, but other readers may see 999.
            r#"{"n":999,"n":5}"#,
            // The same value, not in canonical form.
            r#"{ "n": 5 }"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let store = two_runs(dir.path()).await;
            let store = tampered(store, dir.path(), |conn| {
                conn.execute("UPDATE events SET body = ?1 WHERE global_pos = 5", [body])
                    .unwrap();
            })
            .await;
            let result = verified(&store).await;
            assert_eq!(
                result.first_problem,
                problem_at(VerifyProblemKind::EventHashMismatch, 5, "a"),
                "body {body}"
            );
            assert_eq!(result.events_checked, 8);
        }
    }

    #[tokio::test]
    async fn a_relinked_event_is_a_prev_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        // Event 5 (a, seq 3) now links to a's first event instead of its
        // second, with its own hash recomputed to match.
        let store = tampered(store, dir.path(), |conn| {
            let wrong_prev = hash_at(conn, 1);
            conn.execute("DELETE FROM events WHERE global_pos = 5", [])
                .unwrap();
            insert_hashed(
                conn,
                &HashedEvent {
                    run_id: "a",
                    seq: 3,
                    global_pos: 5,
                    kind: "test.event",
                    ts_ms: TS + 5,
                    body: &json!({ "n": 5 }),
                    prev_hash: &wrong_prev,
                },
            );
        })
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::PrevHashMismatch, 5, "a")
        );
    }

    #[tokio::test]
    async fn swapped_events_are_reported_at_the_first_moved_position() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        // Swap position 4 (b, seq 2) and 7 (a, seq 4).
        let store = tampered(
            store,
            dir.path(),
            sql("UPDATE events SET global_pos = -1 WHERE global_pos = 4;
                 UPDATE events SET global_pos = 4 WHERE global_pos = 7;
                 UPDATE events SET global_pos = 7 WHERE global_pos = -1;"),
        )
        .await;
        // Position 4 now holds a's seq 4 right after a's seq 2.
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::SequenceGap, 4, "a")
        );

        // Swapping neighbours from different runs keeps every run's order;
        // the hashes, which cover the position, still catch it.
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql("UPDATE events SET global_pos = -1 WHERE global_pos = 5;
                 UPDATE events SET global_pos = 5 WHERE global_pos = 6;
                 UPDATE events SET global_pos = 6 WHERE global_pos = -1;"),
        )
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::EventHashMismatch, 5, "b")
        );
    }

    #[tokio::test]
    async fn a_forged_event_inserted_in_the_middle_is_detected() {
        // A well-formed event for run a, hashed correctly, slipped in after
        // position 4 as a's seq 3: every later event moves up one position and
        // a's later events one seq.
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(store, dir.path(), |conn| {
            make_room_at_5(conn);
            conn.execute_batch(
                "UPDATE events SET seq = -seq WHERE run_id = 'a' AND seq >= 3;
                 UPDATE events SET seq = 1 - seq WHERE run_id = 'a' AND seq < 0;",
            )
            .unwrap();
            let prev = hash_at(conn, 3);
            insert_forged(conn, "a", 3, &prev);
        })
        .await;
        let result = verified(&store).await;
        // The forgery itself links and hashes correctly; a's next event still
        // links to a's seq 2.
        assert_eq!(
            result.first_problem,
            problem_at(VerifyProblemKind::PrevHashMismatch, 6, "a")
        );
        assert_eq!(result.events_checked, 9);

        // The same forgery as the first event of a new run: the next event
        // keeps its links, but its hash covers its old position.
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(store, dir.path(), |conn| {
            make_room_at_5(conn);
            conn.execute(
                "INSERT INTO runs (run_id, created_ms, last_seq, last_hash) VALUES ('c', 0, 0, '')",
                [],
            )
            .unwrap();
            let hash = insert_forged(conn, "c", 1, ZERO_HASH);
            conn.execute(
                "UPDATE runs SET last_seq = 1, last_hash = ?1 WHERE run_id = 'c'",
                [hash],
            )
            .unwrap();
        })
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::EventHashMismatch, 6, "a")
        );
    }

    /// Moves every event after position 4 up one position.
    fn make_room_at_5(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "UPDATE events SET global_pos = -global_pos WHERE global_pos > 4;
             UPDATE events SET global_pos = 1 - global_pos WHERE global_pos < 0;",
        )
        .unwrap();
    }

    fn insert_forged(conn: &rusqlite::Connection, run: &str, seq: u64, prev: &str) -> String {
        insert_hashed(
            conn,
            &HashedEvent {
                run_id: run,
                seq,
                global_pos: 5,
                kind: "test.event",
                ts_ms: TS + 100,
                body: &json!({ "forged": true }),
                prev_hash: prev,
            },
        )
    }

    #[tokio::test]
    async fn an_event_removed_from_the_middle_is_a_global_position_gap() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql("DELETE FROM events WHERE global_pos = 5"),
        )
        .await;
        let result = verified(&store).await;
        // Reported at the first event after the gap.
        assert_eq!(
            result.first_problem,
            problem_at(VerifyProblemKind::GlobalPositionGap, 6, "b")
        );
        assert_eq!(result.events_checked, 7);
    }

    /// R4.6: removing the newest events, with the run heads set back to match,
    /// leaves no trace. This asserts the limit SECURITY.md and PRIVACY.md
    /// document, until feature 04's signed checkpoints exist.
    #[tokio::test]
    async fn truncating_the_end_of_the_log_is_not_detected() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(store, dir.path(), |conn| {
            conn.execute_batch("DELETE FROM events WHERE global_pos > 6")
                .unwrap();
            let (a, b) = (hash_at(conn, 5), hash_at(conn, 6));
            conn.execute(
                "UPDATE runs SET last_seq = 3, last_hash = ?1 WHERE run_id = 'a'",
                [a],
            )
            .unwrap();
            conn.execute(
                "UPDATE runs SET last_seq = 3, last_hash = ?1 WHERE run_id = 'b'",
                [b],
            )
            .unwrap();
        })
        .await;
        let result = verified(&store).await;
        assert!(result.ok, "end-of-log truncation is undetectable (R4.6)");
        assert_eq!(result.events_checked, 6);
    }

    #[tokio::test]
    async fn truncating_the_end_without_fixing_the_run_heads_is_a_run_head_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql("DELETE FROM events WHERE global_pos > 6"),
        )
        .await;
        // Both heads are wrong; a's last remaining event comes first.
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::RunHeadMismatch, 5, "a")
        );
    }

    #[tokio::test]
    async fn a_run_head_that_disagrees_with_its_last_event_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql("UPDATE runs SET last_seq = 3 WHERE run_id = 'b'"),
        )
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::RunHeadMismatch, 8, "b")
        );

        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql("UPDATE runs SET last_hash = '00' WHERE run_id = 'a'"),
        )
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::RunHeadMismatch, 7, "a")
        );
    }

    #[tokio::test]
    async fn runs_and_events_that_do_not_match_up_are_run_head_mismatches() {
        // A run with no events, reported at position 0.
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql("INSERT INTO runs (run_id, created_ms, last_seq, last_hash)
                 VALUES ('ghost', 0, 1, 'ff')"),
        )
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::RunHeadMismatch, 0, "ghost")
        );

        // Events of a run that has no `runs` row.
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql("DELETE FROM runs WHERE run_id = 'b'"),
        )
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::RunHeadMismatch, 8, "b")
        );
    }

    #[tokio::test]
    async fn a_row_with_the_wrong_column_types_is_reported_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = two_runs(dir.path()).await;
        let store = tampered(
            store,
            dir.path(),
            sql("UPDATE events SET body = x'00ff' WHERE global_pos = 3;"),
        )
        .await;
        let result = verified(&store).await;
        assert_eq!(
            result.first_problem,
            problem_at(VerifyProblemKind::EventHashMismatch, 3, "a")
        );
        assert_eq!(result.events_checked, 8);
    }

    /// Runs `a` and `b` share one message; `a` has one of its own. Positions:
    /// 1 a [shared, own], 2 b [shared], 3 b (no content).
    async fn shared_content(dir: &Path) -> Store {
        let store = open(dir);
        assert!(store.set_capture_content(true).await.unwrap());
        store
            .append(with_content("a", 1, &[b"shared", b"own"]))
            .await
            .unwrap();
        store
            .append(with_content("b", 2, &[b"shared"]))
            .await
            .unwrap();
        store.append(event("b", 3)).await.unwrap();
        store
    }

    fn delete_blob(address: String) -> impl FnOnce(&rusqlite::Connection) {
        move |conn| {
            let deleted = conn
                .execute("DELETE FROM blobs WHERE address = ?1", [address])
                .unwrap();
            assert_eq!(deleted, 1);
        }
    }

    #[tokio::test]
    async fn stored_content_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let result = verified(&store).await;
        assert!(result.ok);
        assert_eq!(result.events_checked, 3);
        assert_eq!(result.erased_events, 0);
    }

    #[tokio::test]
    async fn a_missing_blob_without_an_erasure_is_content_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let store = tampered(store, dir.path(), delete_blob(address(b"shared"))).await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::ContentMissing, 1, "a")
        );
    }

    #[tokio::test]
    async fn a_blob_erased_later_in_another_run_verifies_and_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let shared = address(b"shared");
        // Erasing run b's content removes the blob run a shares; the erasure
        // event is appended to run b only.
        store
            .append(erasure("b", &[&shared], &["a", "b"]))
            .await
            .unwrap();
        let store = tampered(store, dir.path(), delete_blob(shared)).await;
        let result = verified(&store).await;
        assert_eq!(
            result,
            VerifyResult {
                ok: true,
                events_checked: 4,
                erased_events: 2,
                first_problem: None,
            }
        );
    }

    #[tokio::test]
    async fn an_erasure_before_the_reference_explains_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        assert!(store.set_capture_content(true).await.unwrap());
        let shared = address(b"shared");
        store
            .append(erasure("c", &[&shared], &["c"]))
            .await
            .unwrap();
        store
            .append(with_content("a", 2, &[b"shared"]))
            .await
            .unwrap();
        let store = tampered(store, dir.path(), delete_blob(shared)).await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::ContentMissing, 2, "a")
        );
    }

    #[tokio::test]
    async fn an_erasure_explains_its_own_run_and_the_runs_it_names_only() {
        // Appended to run b without naming a: b's reference is erased, a's
        // is missing.
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let shared = address(b"shared");
        store.append(erasure("b", &[&shared], &[])).await.unwrap();
        let store = tampered(store, dir.path(), delete_blob(shared)).await;
        let result = verified(&store).await;
        assert_eq!(
            result.first_problem,
            problem_at(VerifyProblemKind::ContentMissing, 1, "a")
        );
        assert_eq!(result.erased_events, 1);

        // Appended to a third run that names both: both are erased.
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let shared = address(b"shared");
        store
            .append(erasure("c", &[&shared], &["a", "b"]))
            .await
            .unwrap();
        let store = tampered(store, dir.path(), delete_blob(shared)).await;
        let result = verified(&store).await;
        assert!(result.ok, "{result:?}");
        assert_eq!(result.erased_events, 2);
    }

    #[tokio::test]
    async fn an_erasure_in_an_unrelated_run_explains_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let shared = address(b"shared");
        store
            .append(erasure("c", &[&shared], &["c"]))
            .await
            .unwrap();
        let store = tampered(store, dir.path(), delete_blob(shared)).await;
        let result = verified(&store).await;
        assert_eq!(
            result.first_problem,
            problem_at(VerifyProblemKind::ContentMissing, 1, "a")
        );
        assert_eq!(result.erased_events, 0);
    }

    #[tokio::test]
    async fn a_tampered_erasure_is_reported_at_the_erasure_event() {
        // An unrelated run's erasure rewritten to name runs a and b: the
        // references pass as erased, the rewritten erasure itself doesn't.
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let shared = address(b"shared");
        store
            .append(erasure("c", &[&shared], &["c"]))
            .await
            .unwrap();
        let forged = erasure("c", &[&shared], &["a", "b"]).body;
        let forged = String::from_utf8(event::canonical_json(&forged).unwrap()).unwrap();
        let store = tampered(store, dir.path(), |conn| {
            conn.execute("UPDATE events SET body = ?1 WHERE global_pos = 4", [forged])
                .unwrap();
            delete_blob(shared)(conn);
        })
        .await;
        let result = verified(&store).await;
        assert_eq!(
            result.first_problem,
            problem_at(VerifyProblemKind::EventHashMismatch, 4, "c")
        );
        assert_eq!(result.erased_events, 2);
    }

    #[tokio::test]
    async fn an_index_that_disagrees_with_the_body_is_content_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let own = address(b"own");
        let store = tampered(store, dir.path(), |conn| {
            conn.execute(
                "DELETE FROM event_content WHERE global_pos = 1 AND address = ?1",
                [&own],
            )
            .unwrap();
        })
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::ContentMissing, 1, "a")
        );

        // An index row the body doesn't list.
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let store = tampered(store, dir.path(), |conn| {
            conn.execute(
                "INSERT INTO event_content (global_pos, address) VALUES (3, ?1)",
                [&own],
            )
            .unwrap();
        })
        .await;
        assert_eq!(
            verified(&store).await.first_problem,
            problem_at(VerifyProblemKind::ContentMissing, 3, "b")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_erasure_committed_during_verification_is_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let shared = address(b"shared");

        // The first pass has already run when the erasure lands; the scan
        // still finds the erasure when it meets the missing blob.
        let mut erasures = Erasures::default();
        while !erasures.gathered {
            erasures = store
                .read(move |conn| {
                    erasures.gather_chunk(conn)?;
                    Ok(erasures)
                })
                .await
                .unwrap();
        }
        store
            .append(erasure("b", &[&shared], &["a", "b"]))
            .await
            .unwrap();
        let store = tampered(store, dir.path(), delete_blob(shared)).await;
        let mut scan = Scan::new(erasures);
        while !scan.finished {
            scan = store
                .read(move |conn| {
                    scan.chunk(conn)?;
                    Ok(scan)
                })
                .await
                .unwrap();
        }
        let result = scan.into_result();
        assert!(result.ok, "{result:?}");
        assert_eq!(result.erased_events, 2);
        assert_eq!(result.events_checked, 4);
    }

    /// Writes a valid log of `count` events over `runs` runs straight into a
    /// fresh database, far faster than appending one durable event at a time.
    async fn written_log(dir: &Path, count: u64, runs: u64) -> Store {
        open(dir).close().await.unwrap();
        let mut conn = raw(dir);
        let tx = conn.transaction().unwrap();
        insert_runs(&tx, 1..=count, runs);
        tx.commit().unwrap();
        drop(conn);
        open(dir)
    }

    /// Writes valid events at `positions` for `runs` new runs `run-0`, ...,
    /// with their `runs` rows.
    fn insert_runs(tx: &rusqlite::Connection, positions: RangeInclusive<u64>, runs: u64) {
        let mut heads: HashMap<String, (u64, String)> = HashMap::new();
        for pos in positions {
            let run_id = format!("run-{}", pos % runs);
            let (seq, prev) = match heads.get(&run_id) {
                None => (1, ZERO_HASH.to_owned()),
                Some((seq, hash)) => (seq + 1, hash.clone()),
            };
            if seq == 1 {
                tx.execute(
                    "INSERT INTO runs (run_id, created_ms, last_seq, last_hash)
                     VALUES (?1, 0, 0, '')",
                    [&run_id],
                )
                .unwrap();
            }
            let hash = insert_hashed(
                tx,
                &HashedEvent {
                    run_id: &run_id,
                    seq,
                    global_pos: pos,
                    kind: "test.event",
                    ts_ms: TS + pos,
                    body: &json!({ "n": pos }),
                    prev_hash: &prev,
                },
            );
            heads.insert(run_id, (seq, hash));
        }
        for (run_id, (seq, hash)) in &heads {
            tx.execute(
                "UPDATE runs SET last_seq = ?1, last_hash = ?2 WHERE run_id = ?3",
                params![*seq as i64, hash, run_id],
            )
            .unwrap();
        }
    }

    const MANY: u64 = 25_000;
    const RUNS: u64 = 7;
    const _: () = assert!(MANY > 2 * CHUNK_EVENTS as u64);

    fn run_at(pos: u64) -> String {
        format!("run-{}", pos % RUNS)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_log_over_several_chunks_verifies_and_a_gap_after_a_boundary_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = written_log(dir.path(), MANY, RUNS).await;

        let started = Instant::now();
        let result = verified(&store).await;
        println!("verified {MANY} events in {:?}", started.elapsed());
        assert_eq!(
            result,
            VerifyResult {
                ok: true,
                events_checked: MANY,
                erased_events: 0,
                first_problem: None,
            }
        );

        // The first event of the second chunk is removed: the gap shows at the
        // position after it, found with the position carried over.
        let boundary = CHUNK_EVENTS as u64;
        let store = tampered(store, dir.path(), |conn| {
            conn.execute(
                "DELETE FROM events WHERE global_pos = ?1",
                [boundary as i64 + 1],
            )
            .unwrap();
        })
        .await;
        let started = Instant::now();
        let result = verified(&store).await;
        println!("found a gap after a boundary in {:?}", started.elapsed());
        assert_eq!(
            result.first_problem,
            problem_at(
                VerifyProblemKind::GlobalPositionGap,
                boundary + 2,
                &run_at(boundary + 2)
            )
        );
        assert_eq!(result.events_checked, MANY - 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_relinked_event_just_after_the_second_boundary_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = written_log(dir.path(), MANY, RUNS).await;
        // Position 20 001 links to its run's event two back instead of one,
        // with its own hash recomputed: only the run head carried across two
        // chunks shows it.
        let pos = 2 * CHUNK_EVENTS as u64 + 1;
        let run = run_at(pos);
        let store = tampered(store, dir.path(), |conn| {
            let wrong_prev = hash_at(conn, (pos - 2 * RUNS) as i64);
            let seq: i64 = conn
                .query_row(
                    "SELECT seq FROM events WHERE global_pos = ?1",
                    [pos as i64],
                    |row| row.get(0),
                )
                .unwrap();
            conn.execute("DELETE FROM events WHERE global_pos = ?1", [pos as i64])
                .unwrap();
            insert_hashed(
                conn,
                &HashedEvent {
                    run_id: &run,
                    seq: seq as u64,
                    global_pos: pos,
                    kind: "test.event",
                    ts_ms: TS + pos,
                    body: &json!({ "n": pos }),
                    prev_hash: &wrong_prev,
                },
            );
        })
        .await;
        let started = Instant::now();
        let result = verified(&store).await;
        println!(
            "found a relinked event after two boundaries in {:?}",
            started.elapsed()
        );
        assert_eq!(
            result.first_problem,
            problem_at(VerifyProblemKind::PrevHashMismatch, pos, &run)
        );
        assert_eq!(result.events_checked, MANY);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn after_the_tail_chunks_one_chunk_reads_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let store = written_log(dir.path(), MANY, RUNS).await;
        // As if appends had kept up for `MAX_TAIL_CHUNKS` full chunks.
        let mut scan = Scan::new(Erasures {
            gathered: true,
            ..Erasures::default()
        });
        scan.tail_chunks = MAX_TAIL_CHUNKS;
        let scan = store
            .read(move |conn| {
                scan.chunk(conn)?;
                Ok(scan)
            })
            .await
            .unwrap();
        assert!(scan.finished);
        assert_eq!(
            scan.into_result(),
            VerifyResult {
                ok: true,
                events_checked: MANY,
                erased_events: 0,
                first_problem: None,
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_erasure_past_the_first_gathered_chunk_explains_an_earlier_reference() {
        let dir = tempfile::tempdir().unwrap();
        let store = shared_content(dir.path()).await;
        let shared = address(b"shared");
        // A full chunk of other events after the references, so the erasure
        // is read by the second chunk of the first pass.
        store.close().await.unwrap();
        drop(store);
        let filler = CHUNK_EVENTS as u64;
        let mut conn = raw(dir.path());
        let tx = conn.transaction().unwrap();
        insert_runs(&tx, 4..=3 + filler, RUNS);
        tx.commit().unwrap();
        drop(conn);
        let store = open(dir.path());
        store
            .append(erasure("b", &[&shared], &["a", "b"]))
            .await
            .unwrap();
        let store = tampered(store, dir.path(), delete_blob(shared)).await;
        let result = verified(&store).await;
        assert_eq!(
            result,
            VerifyResult {
                ok: true,
                events_checked: filler + 4,
                erased_events: 2,
                first_problem: None,
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_event_at_the_largest_position_is_read_once() {
        // A full chunk whose last event sits at `i64::MAX`: nothing can
        // follow it, so the scan ends there instead of reading it again.
        let dir = tempfile::tempdir().unwrap();
        let count = CHUNK_EVENTS as u64;
        let store = written_log(dir.path(), count, RUNS).await;
        let store = tampered(store, dir.path(), |conn| {
            conn.execute(
                "UPDATE events SET global_pos = ?1 WHERE global_pos = ?2",
                params![i64::MAX, count as i64],
            )
            .unwrap();
        })
        .await;
        let result = verified(&store).await;
        assert_eq!(
            result.first_problem,
            problem_at(
                VerifyProblemKind::GlobalPositionGap,
                i64::MAX as u64,
                &run_at(count)
            )
        );
        assert_eq!(result.events_checked, count);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn verification_runs_while_appends_continue() {
        const ROUNDS: usize = 3;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(written_log(dir.path(), MANY, RUNS).await);
        let appended = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let appender = {
            let (store, appended, stop) =
                (Arc::clone(&store), Arc::clone(&appended), Arc::clone(&stop));
            tokio::spawn(async move {
                let mut n = 0;
                while !stop.load(Ordering::SeqCst) {
                    // Old runs and new ones.
                    let run = format!("run-{}", n % (RUNS + 2));
                    store.append(event(&run, n)).await.unwrap();
                    appended.fetch_add(1, Ordering::SeqCst);
                    n += 1;
                }
            })
        };
        while appended.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let before = appended.load(Ordering::SeqCst);
        for _ in 0..ROUNDS {
            let result = tokio::time::timeout(Duration::from_secs(120), verify(&store))
                .await
                .expect("verify finished")
                .unwrap();
            assert!(result.ok, "{result:?}");
            assert!(result.events_checked > MANY);
        }
        let during = appended.load(Ordering::SeqCst) - before;
        stop.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(120), appender)
            .await
            .expect("appends finished")
            .unwrap();
        println!("{during} events appended during {ROUNDS} verifications");
        assert!(during > 0, "appends went on while verifying");

        let result = verified(&store).await;
        assert!(result.ok);
        assert_eq!(
            result.events_checked,
            MANY + appended.load(Ordering::SeqCst)
        );
    }
}

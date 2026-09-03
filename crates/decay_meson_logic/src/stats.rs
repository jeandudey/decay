//! Lightweight counters for where solver time goes.
//!
//! Every counter is a plain relaxed atomic, bumped on a hot path, so the
//! overhead is a single `add`. [`maybe_report`] emits a `tracing` line at
//! `info` every [`REPORT_EVERY`] real solver checks — enough to watch the shape
//! of a run without letting it finish. Nothing here changes behaviour; deleting
//! the module would leave the algorithm identical. The names are backend
//! neutral: "check" is one [`crate::Solver::is_sat`], whatever answers it.

use {
    std::{
        sync::atomic::{
            AtomicU64,
            Ordering::Relaxed, //
        },
        time::Duration,
    },
    tracing::info,
};

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        $(pub static $name: AtomicU64 = AtomicU64::new(0);)*
    };
}

counters!(
    // Logic::is_sat outcomes
    IS_SAT_CALLS,
    IS_SAT_CONST,  // answered by pc.is_true()/is_false()
    IS_SAT_HIT,    // answered from the `sat` memo
    IS_SAT_MISS,   // went to the backend
    // the actual backend check (z3 check_assumptions, or a BDD emptiness test)
    CHECK_CALLS,
    CHECK_NANOS,
    // assume(): each one narrows the space and wipes the `sat` memo
    ASSUME_CALLS,
    SAT_MEMO_DROPPED, // entries thrown away by assume()
    // Pc -> backend term lowering
    TERM_TOP_CALLS, // calls from is_sat/assume (not the internal recursion)
    TERM_TOP_HIT,   // top-level call answered straight from the `terms` memo
    TERM_NANOS,     // wall time building term trees (recursion included)
    // backend term construction
    TERM_AND,
    TERM_OR,
    TERM_NOT,
    TERM_LIT,
    VAR_DECLARE,
    // arena (structural, pre-backend)
    ARENA_AND_CALLS,
    ARENA_OR_CALLS,
    ARENA_NOT_CALLS,
    ARENA_INTERNED, // nodes actually added (memo miss)
    ARENA_NODES,    // high-water mark of nodes.len()
    ARENA_VARS,     // high-water mark of vars.len()
);

/// How many real solver checks between `tracing` reports.
const REPORT_EVERY: u64 = 1000;

#[inline]
pub fn bump(c: &AtomicU64) {
    c.fetch_add(1, Relaxed);
}

#[inline]
pub fn add(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Relaxed);
}

#[inline]
pub fn max(c: &AtomicU64, n: u64) {
    c.fetch_max(n, Relaxed);
}

fn get(c: &AtomicU64) -> u64 {
    c.load(Relaxed)
}

/// Report once every `REPORT_EVERY` solver checks. Call right after a check.
pub fn maybe_report() {
    let n = get(&CHECK_CALLS);
    if n != 0 && n % REPORT_EVERY == 0 {
        report();
    }
}

/// Emit the current picture.
pub fn report() {
    let checks = get(&CHECK_CALLS).max(1);
    let is_sat = get(&IS_SAT_CALLS).max(1);
    let term_top = get(&TERM_TOP_CALLS).max(1);

    info!(
        target: "solver_stats",
        is_sat_calls = get(&IS_SAT_CALLS),
        is_sat_const = get(&IS_SAT_CONST),
        is_sat_memo_hit = get(&IS_SAT_HIT),
        is_sat_backend = get(&IS_SAT_MISS),
        memo_hit_pct = 100 * get(&IS_SAT_HIT) / is_sat,
        checks = get(&CHECK_CALLS),
        check_ms = Duration::from_nanos(get(&CHECK_NANOS)).as_millis(),
        us_per_check = get(&CHECK_NANOS) / 1000 / checks,
        assume_calls = get(&ASSUME_CALLS),
        sat_memo_dropped = get(&SAT_MEMO_DROPPED),
        term_top_calls = get(&TERM_TOP_CALLS),
        term_top_hit_pct = 100 * get(&TERM_TOP_HIT) / term_top,
        term_ms = Duration::from_nanos(get(&TERM_NANOS)).as_millis(),
        term_and = get(&TERM_AND),
        term_or = get(&TERM_OR),
        term_not = get(&TERM_NOT),
        term_lit = get(&TERM_LIT),
        var_declare = get(&VAR_DECLARE),
        arena_and = get(&ARENA_AND_CALLS),
        arena_or = get(&ARENA_OR_CALLS),
        arena_not = get(&ARENA_NOT_CALLS),
        arena_interned = get(&ARENA_INTERNED),
        arena_nodes = get(&ARENA_NODES),
        arena_vars = get(&ARENA_VARS),
        "solver stats",
    );
}

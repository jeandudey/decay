//! Lightweight counters for where presence-condition time and space go.
//!
//! Every counter is a plain relaxed atomic, bumped on a hot path, so the
//! overhead is a single `add`. [`maybe_report`] emits a `tracing` line at
//! `info` every [`REPORT_EVERY`] reachability checks — enough to watch the
//! shape of a run without letting it finish. Nothing here changes behaviour.

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
    IS_SAT_CONST, // answered by pc.is_true()/is_false()
    IS_SAT_HIT,   // answered from the `sat` memo
    IS_SAT_MISS,  // hit the diagrams
    // the reachability check itself (bdd ∧ context ≠ ⊥)
    CHECK_CALLS,
    CHECK_NANOS,
    // assume(): narrows the space and drops the SAT entries of the `sat` memo
    ASSUME_CALLS,
    SAT_MEMO_DROPPED,
    // arena
    ARENA_AND_CALLS,
    ARENA_OR_CALLS,
    ARENA_NOT_CALLS,
    ARENA_INTERNED,  // conditions actually added (dedup miss)
    PC_COUNT,        // distinct conditions stored (high-water)
    BDD_NODES_TOTAL, // summed node count of every stored condition
    BDD_MAX_SIZE,    // node count of the largest single condition
    ARENA_VARS,      // configuration variables declared (high-water)
    VAR_DECLARE,
);

/// How many reachability checks between `tracing` reports.
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

/// Report once every `REPORT_EVERY` checks. Call right after a check.
pub fn maybe_report() {
    let n = get(&CHECK_CALLS);
    if n != 0 && n.is_multiple_of(REPORT_EVERY) {
        report();
    }
}

/// Emit the current picture.
pub fn report() {
    let checks = get(&CHECK_CALLS).max(1);
    let is_sat = get(&IS_SAT_CALLS).max(1);
    let pcs = get(&PC_COUNT).max(1);

    info!(
        target: "solver_stats",
        is_sat_calls = get(&IS_SAT_CALLS),
        is_sat_const = get(&IS_SAT_CONST),
        is_sat_memo_hit = get(&IS_SAT_HIT),
        is_sat_check = get(&IS_SAT_MISS),
        memo_hit_pct = 100 * get(&IS_SAT_HIT) / is_sat,
        checks = get(&CHECK_CALLS),
        check_ms = Duration::from_nanos(get(&CHECK_NANOS)).as_millis(),
        us_per_check = get(&CHECK_NANOS) / 1000 / checks,
        assume_calls = get(&ASSUME_CALLS),
        sat_memo_dropped = get(&SAT_MEMO_DROPPED),
        arena_and = get(&ARENA_AND_CALLS),
        arena_or = get(&ARENA_OR_CALLS),
        arena_not = get(&ARENA_NOT_CALLS),
        arena_interned = get(&ARENA_INTERNED),
        pc_count = get(&PC_COUNT),
        bdd_nodes_total = get(&BDD_NODES_TOTAL),
        bdd_avg_size = get(&BDD_NODES_TOTAL) / pcs,
        bdd_max_size = get(&BDD_MAX_SIZE),
        arena_vars = get(&ARENA_VARS),
        var_declare = get(&VAR_DECLARE),
        "solver stats",
    );
}

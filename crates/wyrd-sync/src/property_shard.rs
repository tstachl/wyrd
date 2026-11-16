//! Shared proptest sharding for the generated-sequence properties:
//! the slowest properties run as four tests of [`SHARD_CASES`] novel
//! cases each instead of one test of 256. The per-run budget is
//! unchanged, but nextest executes the shards in parallel, so wall
//! time roughly quarters on a multicore machine. Persisted
//! regression seeds replay in every shard on top. Bodies are written
//! once; the macro stamps out the shard tests, so shrinking and
//! failure reporting behave exactly like hand-written tests.
//!
//! Test-only: both the budget and the macro resolve at the call
//! site, so each `properties` module imports them explicitly.

/// Novel proptest cases per shard: four shards hold the 256-case
/// budget of one unsharded slow property.
pub(crate) const SHARD_CASES: u32 = 64;

/// Stamp out one shard test per `shards` entry, each running
/// [`SHARD_CASES`] cases over the shared `body`. Shard names are
/// the caller's `fn` names, so failure output reads like
/// hand-written tests.
macro_rules! sharded_property {
    (
        body $body:block
        shards { $($name:ident ( $($arg:ident in $strategy:expr),* ))* }
    ) => {
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(SHARD_CASES))]
            $(
                #[test]
                fn $name($($arg in $strategy),*) $body
            )*
        }
    };
}

pub(crate) use sharded_property;

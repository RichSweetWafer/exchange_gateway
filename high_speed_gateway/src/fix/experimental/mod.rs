//! Parser prototypes from the SIMD / tape investigation. Not used by the
//! gateway. Each file's header says what it is and what was measured.
//!
//! - `parser_fast`: memchr / SSE2 / AVX2 field scanners on top of parser_v2 (~1.1x at best).
//! - `parser_tape`: parser_v2 writing into the tape index; keeps repeated tags for groups.
//! - `flat_index`: copy of `fix::flat_index` with the register-held-length `Writer`.
//! - `diff_tests`: differential tests of the prototypes against `fix::parser_v2`.
//! - `benches`: the benchmarks, as `#[ignore]` tests.
//!
//! `parser_fast` needs the `memchr` crate (already used by `fix::framer`).

pub mod flat_index;
pub mod parser_fast;
pub mod parser_tape;

#[cfg(test)]
mod diff_tests;
#[cfg(test)]
mod benches;

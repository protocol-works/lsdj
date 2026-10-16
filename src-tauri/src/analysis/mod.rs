//! Musical analysis in the shell (ADR-0025, ADR-0030): the beat estimator
//! and its honesty gates, ported verbatim in intent from the corpus-locked
//! TypeScript. Runs on shell threads — never the `cpal` callback, which must
//! never read analysis state (ADR-0025).

pub mod bands;
pub mod beat;
pub mod grid;
pub mod key;
pub mod live;
pub mod media;
pub mod track;

#[cfg(test)]
mod beat_corpus;

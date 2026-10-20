//! AMBE+2 (DMR, 3600x2450) voice decoder: a native port of the decoder in Tytera MD-380
//! firmware D002.032. See DESIGN.md (what and why) and NOTES.md (routine map and status).
mod decoder;
pub mod fft;
pub mod frame;
pub mod math;
pub mod ops;
pub mod spectral;
pub mod state;
pub mod synth;
pub mod tables;
#[cfg(test)]
pub mod testvec;
pub mod unvoiced;
pub mod voiced;

pub use decoder::{Decoder, FLAG_BAD_FRAME};

//! AMBE+2 (DMR, 3600x2450) voice decoder: a native port of the decoder in Tytera MD-380
//! firmware D002.032. See DESIGN.md in this directory.
pub mod fft;
pub mod math;
pub mod ops;
pub mod state;
pub mod tables;
#[cfg(test)]
pub mod testvec;
pub mod unvoiced;

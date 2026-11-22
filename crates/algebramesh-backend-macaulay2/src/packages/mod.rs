//! The packages of research operations this backend offers (`docs/roadmap.md`,
//! section 6.1), each behind its Cargo feature.

#[cfg(feature = "pkg-gkz")]
pub(crate) mod gkz;
#[cfg(feature = "pkg-topology")]
pub(crate) mod topology;

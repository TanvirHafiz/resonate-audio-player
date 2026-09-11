//! Phase 3+ intelligence layer: audio feature extraction, rule-based
//! mood/genre tagging, listening-affinity scoring, and the "More Like
//! This" / "For You" recommendation modes. This whole module only exists
//! in builds compiled with `--features pro` (see `Cargo.toml`) — it is the
//! paid-tier boundary for the app.

pub mod affinity_engine;
pub mod api;
pub mod enrichment;
pub mod feature_extraction;
pub mod mood_tagging;
pub mod playlists;
pub mod recommendation_engine;
pub mod settings;
pub mod worker;

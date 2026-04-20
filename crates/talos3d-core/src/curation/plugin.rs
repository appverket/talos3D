//! `CurationPlugin` — installs the curation-substrate resources and
//! seeds the Canonical-tier sources at startup.
//!
//! Per ADR-040, this plugin is always on (core-owned) and domain crates
//! / jurisdiction packs layer their own content on top at plugin build
//! time.

use bevy::prelude::*;

use super::nomination::NominationQueue;
use super::registry::{ensure_canonical_seed, SourceRegistry};

pub struct CurationPlugin;

impl Plugin for CurationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SourceRegistry>()
            .init_resource::<NominationQueue>()
            .add_systems(Startup, seed_canonical_sources);
    }
}

/// Startup system: seeds the SourceRegistry with Canonical-tier entries.
/// Idempotent; safe to run alongside plugin-registered jurisdiction
/// content.
fn seed_canonical_sources(mut registry: ResMut<SourceRegistry>) {
    ensure_canonical_seed(&mut registry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curation::identity::{SourceId, SourceRevision};

    #[test]
    fn plugin_installs_registry_and_seeds_canonicals() {
        let mut app = App::new();
        app.add_plugins(CurationPlugin);
        app.update(); // runs Startup schedule

        let registry = app.world().resource::<SourceRegistry>();
        assert!(registry
            .get(&SourceId::new("iso.129-1"), &SourceRevision::new("2018"))
            .is_some());
        assert!(registry
            .get(&SourceId::new("asme.y14.5"), &SourceRevision::new("2018"))
            .is_some());
        assert!(registry
            .get(&SourceId::new("iso.80000-1"), &SourceRevision::new("2022"))
            .is_some());
    }

    #[test]
    fn plugin_startup_is_idempotent() {
        let mut app = App::new();
        app.add_plugins(CurationPlugin);
        app.update();
        let count_after_first = app.world().resource::<SourceRegistry>().revision_count();
        // Re-run the startup schedule.
        app.world_mut()
            .run_schedule(bevy::app::Startup);
        let count_after_second = app.world().resource::<SourceRegistry>().revision_count();
        assert_eq!(count_after_first, count_after_second);
    }
}

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct IdentityPlugin;

impl Plugin for IdentityPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ElementIdAllocator>();
    }
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ElementId(pub u64);

#[derive(Resource, Debug, Default)]
pub struct ElementIdAllocator {
    next: AtomicU64,
}

impl ElementIdAllocator {
    pub fn next_id(&self) -> ElementId {
        ElementId(self.next.fetch_add(1, Ordering::Relaxed))
    }

    pub fn set_next(&mut self, next: u64) {
        self.next.store(next, Ordering::Relaxed);
    }

    pub fn next_value(&self) -> u64 {
        self.next.load(Ordering::Relaxed)
    }
}

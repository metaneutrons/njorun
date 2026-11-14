//! Deterministic memory limits that preserve the owner's HA-configured priorities.
use crate::{EntityDomain, MAX_ENTITY_COUNT, settings::Settings};
use std::collections::BTreeSet;

#[derive(Debug, PartialEq, Eq)]
pub struct Selection {
    pub entity_ids: BTreeSet<String>,
    pub omitted: usize,
}

#[must_use]
pub const fn domain_priority(domain: EntityDomain) -> u8 {
    match domain {
        EntityDomain::Sensor => 3,
        EntityDomain::BinarySensor => 2,
        EntityDomain::Scene
        | EntityDomain::Script
        | EntityDomain::Automation
        | EntityDomain::Button
        | EntityDomain::InputButton
        | EntityDomain::Camera => 1,
        _ => 0,
    }
}

#[must_use]
pub fn select(candidates: impl IntoIterator<Item = String>, settings: &Settings) -> Selection {
    let candidates: BTreeSet<_> = candidates
        .into_iter()
        .filter(|id| !settings.excludes(id) && EntityDomain::from_entity_id(id).is_some())
        .collect();
    let count = candidates.len();
    let ordered: Vec<_> = settings.entity_order.values().flatten().collect();
    let mut ranked: Vec<_> = candidates
        .into_iter()
        .map(|id| {
            let favorite = settings
                .favorites
                .iter()
                .position(|candidate| candidate == &id)
                .unwrap_or(usize::MAX);
            let explicit = ordered
                .iter()
                .position(|candidate| **candidate == id)
                .unwrap_or(usize::MAX);
            let domain = EntityDomain::from_entity_id(&id).map_or(u8::MAX, domain_priority);
            ((favorite, explicit, domain), id)
        })
        .collect();
    ranked.sort_unstable();
    Selection {
        entity_ids: ranked
            .into_iter()
            .take(MAX_ENTITY_COUNT)
            .map(|(_, id)| id)
            .collect(),
        omitted: count.saturating_sub(MAX_ENTITY_COUNT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusions_and_favorites_precede_domain_rank_at_the_capacity_limit() {
        let settings = Settings {
            favorites: vec!["sensor.favorite".into()],
            excluded_entities: vec!["light.excluded".into()],
            ..Settings::default()
        };
        let ids = (0..MAX_ENTITY_COUNT)
            .map(|index| format!("light.item_{index}"))
            .chain([
                "sensor.favorite".into(),
                "light.excluded".into(),
                "sun.sun".into(),
            ]);
        let selected = select(ids, &settings);
        assert_eq!(selected.entity_ids.len(), MAX_ENTITY_COUNT);
        assert_eq!(selected.omitted, 2);
        assert!(selected.entity_ids.contains("sensor.favorite"));
        assert!(!selected.entity_ids.contains("light.excluded"));
    }
}

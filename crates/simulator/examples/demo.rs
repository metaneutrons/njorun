//! Interactive desktop dashboard with disposable, local test data.
use njorun_app_core::{
    ActionAcknowledgement, AppCore, DetailAction, HomeAssistantService, PrimaryTap,
    settings::{AppliedSettings, Settings},
};
use njorun_simulator::{RuntimeCommand, RuntimeEvent, ui};
use std::{collections::BTreeMap, error::Error, sync::mpsc, thread};

fn main() -> Result<(), Box<dyn Error>> {
    let orientation = std::env::args().nth(1).unwrap_or_else(|| "portrait".into());
    let (width, height) = match orientation.as_str() {
        "portrait" => (800, 1280),
        "landscape" => (1280, 800),
        _ => return Err("Usage: demo [portrait|landscape]".into()),
    };
    let mut core = ui::qualification::demo_core()?;
    let settings = AppliedSettings {
        revision: 1,
        settings: Settings {
            language: "de".into(),
            orientation,
            brightness: 100,
            screensaver: "disabled".into(),
            home_view: "mixed".into(),
            favorites: vec!["light.living_room_lamp".into(), "scene.movie_time".into()],
            entity_names: BTreeMap::from([
                ("light.living_room_lamp".into(), "Leselicht".into()),
                ("switch.reading_light".into(), "Wandleuchte".into()),
                ("scene.movie_time".into(), "Filmabend".into()),
                ("sensor.living_room_temperature".into(), "Wohnzimmer".into()),
                ("binary_sensor.hallway_motion".into(), "Bewegung".into()),
                ("light.kitchen_main".into(), "Deckenlicht".into()),
                ("sensor.outdoor_temperature".into(), "Draußen".into()),
            ]),
            room_names: BTreeMap::from([
                ("living_room".into(), "Wohnzimmer".into()),
                ("kitchen".into(), "Küche".into()),
                ("hallway".into(), "Flur".into()),
                ("unassigned".into(), "Draußen".into()),
            ]),
            room_icons: BTreeMap::from([
                ("living_room".into(), "sofa".into()),
                ("kitchen".into(), "kitchen".into()),
                ("hallway".into(), "home".into()),
            ]),
            ..Settings::default()
        },
    };
    let (events, incoming) = mpsc::sync_channel(16);
    let (commands, outgoing) = mpsc::sync_channel(16);
    events.send(RuntimeEvent::Dashboard(core.dashboard().clone()))?;
    events.send(RuntimeEvent::Settings(Box::new(settings)))?;
    let worker = thread::spawn(move || {
        while let Ok(command) = outgoing.recv() {
            match command {
                RuntimeCommand::Tap(entity) => {
                    if let Ok(PrimaryTap::CallService { service, .. }) = core.primary_tap(&entity) {
                        if events
                            .send(RuntimeEvent::Dashboard(core.dashboard().clone()))
                            .is_err()
                        {
                            break;
                        }
                        if apply_local_service(&mut core, &entity, service).is_err()
                            || events
                                .send(RuntimeEvent::Dashboard(core.dashboard().clone()))
                                .is_err()
                        {
                            break;
                        }
                    }
                }
                RuntimeCommand::Action { entity_id, action } => {
                    if let Ok(PrimaryTap::Call { .. }) = core.request_action(&entity_id, &action)
                        && (events
                            .send(RuntimeEvent::Dashboard(core.dashboard().clone()))
                            .is_err()
                            || apply_local_action(&mut core, &entity_id, &action).is_err()
                            || events
                                .send(RuntimeEvent::Dashboard(core.dashboard().clone()))
                                .is_err())
                    {
                        break;
                    }
                }
                RuntimeCommand::FetchImage { entity_id, path } => {
                    let _ = events.send(RuntimeEvent::Image {
                        entity_id,
                        path,
                        result: Err("No camera connected to this local demo".into()),
                    });
                }
                RuntimeCommand::SettingsApplied(_) => {}
                RuntimeCommand::Stop => break,
            }
        }
    });
    ui::run(incoming, commands, width, height)?;
    worker
        .join()
        .map_err(|_| "Demo worker stopped unexpectedly")?;
    Ok(())
}

fn apply_local_service(
    core: &mut AppCore,
    entity: &str,
    service: HomeAssistantService,
) -> Result<(), Box<dyn Error>> {
    core.acknowledge(entity, ActionAcknowledgement::Accepted)?;
    if service != HomeAssistantService::SceneTurnOn {
        let mut state = core
            .dashboard()
            .entity(entity)
            .and_then(|tile| tile.state.clone())
            .ok_or("Missing demo entity state")?;
        state.state = if state.state == "on" { "off" } else { "on" }.into();
        state.last_updated = Some(chrono::Utc::now().to_rfc3339());
        core.apply_state(state)?;
    }
    Ok(())
}

fn apply_local_action(
    core: &mut AppCore,
    entity: &str,
    action: &DetailAction,
) -> Result<(), Box<dyn Error>> {
    core.acknowledge(entity, ActionAcknowledgement::Accepted)?;
    let mut state = core
        .dashboard()
        .entity(entity)
        .and_then(|tile| tile.state.clone())
        .ok_or("Missing demo entity state")?;
    match action {
        DetailAction::SetBrightness(percent) => {
            let brightness = (u64::from(*percent) * 255 + 50) / 100;
            state.state = if *percent == 0 { "off" } else { "on" }.into();
            state
                .attributes
                .insert("brightness".into(), serde_json::json!(brightness));
        }
        DetailAction::Entity { key, argument } => match key.as_str() {
            "turn_on" => state.state = "on".into(),
            "turn_off" => state.state = "off".into(),
            "brightness" => {
                let percent = argument.as_u64().ok_or("Invalid demo brightness")?;
                state.state = "on".into();
                state.attributes.insert(
                    "brightness".into(),
                    serde_json::json!((percent * 255 + 50) / 100),
                );
            }
            "activate" | "press" | "start" => {}
            _ => return Err("Unsupported local demo detail action".into()),
        },
        _ => return Err("Unsupported local demo detail action".into()),
    }
    state.last_updated = Some(chrono::Utc::now().to_rfc3339());
    core.apply_state(state)?;
    Ok(())
}

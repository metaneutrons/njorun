//! Interactive desktop dashboard with disposable, local test data.
use njorun_app_core::{
    ActionAcknowledgement, AppCore, HomeAssistantService, PrimaryTap,
    settings::{AppliedSettings, Settings},
};
use njorun_simulator::{RuntimeCommand, RuntimeEvent, ui};
use std::{error::Error, sync::mpsc, thread};

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
                RuntimeCommand::SettingsApplied(_) | RuntimeCommand::Action { .. } => {}
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

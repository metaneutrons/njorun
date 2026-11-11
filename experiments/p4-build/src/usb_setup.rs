//! First setup over the USB-C cable (plan 005): Improv Wi-Fi Serial.
//!
//! A task reads the serial port, finds Improv frames in what arrives (log text shares the
//! stream) and answers them. A profile with the Wi-Fi name, the password and optionally the
//! Home Assistant address joins the network and hands over to the setup screens, which then
//! pair with Home Assistant. Once the tablet has a binding, a profile is refused.

use std::sync::mpsc::Sender;
use std::time::Duration;

use njorun_app_core::improv::{
    check_settings, device_info_frame, error_frame, parse_command, redirect_frame, scan_frames,
    state_frame, Command, ErrorCode, Frame, Parser, Settings, State,
};

use crate::commissioning::Action;
use crate::{board, store};

const READ_TIMEOUT_MS: u32 = 200;
const STACK_BYTES: usize = 24 * 1024;
/// Offered to a client that wants an address to visit after a profile without one.
const PROJECT_URL: &str = "https://github.com/metaneutrons/njorun";

/// Starts the task that serves the USB-C port.
pub(crate) fn start(actions: Sender<Action>) {
    let spawned = std::thread::Builder::new()
        .name("usb-setup".to_owned())
        .stack_size(STACK_BYTES)
        .spawn(move || serve(&actions));
    if let Err(error) = spawned {
        log::error!(target: "usb", "USB setup task failed to start: {error}");
    }
}

struct Port {
    state: State,
}

fn serve(actions: &Sender<Action>) {
    let mut parser = Parser::new();
    let mut buffer = [0_u8; 128];
    let mut port = Port {
        state: State::Ready,
    };
    let mut reported = false;
    loop {
        match board::serial_read(&mut buffer, READ_TIMEOUT_MS) {
            Ok(0) => {}
            Ok(length) => {
                for frame in parser.push(&buffer[..length]) {
                    port.handle(&frame, actions);
                }
            }
            Err(code) => {
                if !reported {
                    log::warn!(target: "usb", "USB serial port unavailable: {code:#x}");
                    reported = true;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn send(frame: &[u8]) {
    if frame.is_empty() {
        return;
    }
    if let Err(code) = board::serial_write(frame) {
        log::warn!(target: "usb", "could not write to the USB serial port: {code:#x}");
    }
}

impl Port {
    fn handle(&mut self, frame: &Frame, actions: &Sender<Action>) {
        match parse_command(frame) {
            Err(code) => send(&error_frame(code)),
            Ok(Command::GetState) => send(&state_frame(self.state)),
            Ok(Command::GetDeviceInfo) => {
                send(&device_info_frame(concat!(
                    env!("CARGO_PKG_VERSION"),
                    "-d1001"
                )));
            }
            Ok(Command::ScanWifi) => self.scan(),
            Ok(Command::WifiSettings(settings)) => self.provision(&settings, actions),
            Ok(Command::Unknown(code)) => {
                log::info!(target: "usb", "unknown command {code:#x}");
                send(&error_frame(ErrorCode::UnknownCommand));
            }
        }
    }

    fn scan(&self) {
        let networks = board::scan_wifi().unwrap_or_else(|code| {
            log::warn!(target: "usb", "Wi-Fi scan failed: {code:#x}");
            Vec::new()
        });
        let rows: Vec<(&str, i8, bool)> = networks
            .iter()
            .map(|network| {
                (
                    network.ssid.as_str(),
                    network.rssi,
                    network.password_required,
                )
            })
            .collect();
        for frame in scan_frames(&rows) {
            send(&frame);
        }
    }

    fn provision(&mut self, settings: &Settings, actions: &Sender<Action>) {
        let locked = store::binding().is_some();
        let accepted = match check_settings(settings, locked) {
            Ok(accepted) => accepted,
            Err(code) => {
                log::warn!(target: "usb", "profile refused: {code:?}");
                send(&error_frame(code));
                return;
            }
        };
        log::info!(target: "usb", "profile received for network {:?}", accepted.ssid);
        self.state = State::Provisioning;
        send(&state_frame(self.state));
        send(&error_frame(ErrorCode::None));
        if let Some(utc) = accepted.utc {
            match board::set_utc(utc) {
                Ok(()) => log::info!(target: "usb", "clock set from the profile"),
                Err(code) => log::warn!(target: "usb", "could not set the clock: {code:#x}"),
            }
        }
        match board::connect_wifi(&accepted.ssid, &accepted.password) {
            Ok(()) => {
                store::save_wifi(&accepted.ssid, &accepted.password);
                self.state = State::Provisioned;
                send(&state_frame(self.state));
                send(&redirect_frame(
                    accepted.url.as_deref().unwrap_or(PROJECT_URL),
                ));
                let _ = actions.send(Action::UsbProvisioned {
                    ssid: accepted.ssid,
                    url: accepted.url,
                });
            }
            Err(code) => {
                log::warn!(target: "usb", "the network did not connect: {code:#x}");
                self.state = State::Ready;
                send(&state_frame(self.state));
                send(&error_frame(ErrorCode::UnableToConnect));
            }
        }
    }
}

//! Improv Wi-Fi Serial for the first setup over USB-C (plan 005), without any transport.
//!
//! The frames are those of the Improv Serial specification, with the extension strings of
//! Vellum's `WIFI_SETTINGS` (see `docs/plans/005-usb-provisioning.md`): `IMPROV`, version 1,
//! type, length, payload and an 8-bit sum. Frames and console text share one byte stream, so
//! the [`Parser`] scans for the magic and drops everything else.
//!
//! The firmware reads bytes from the USB-Serial-JTAG port, feeds them to the parser, turns the
//! frames into [`Command`]s, checks a profile with [`check_settings`] and writes the answers
//! built here. The test vectors in `protocol/fixtures/improv-frames.json` were written by
//! Vellum's TypeScript client.

use crate::problems::valid_base_url;

/// The first six bytes of every frame.
pub const MAGIC: &[u8; 6] = b"IMPROV";
/// The only frame version.
pub const VERSION: u8 = 1;
/// The longest payload a frame can carry (its length is one byte).
pub const MAX_PAYLOAD: usize = 255;
/// Longest `WIFI_SETTINGS` data: its length is one byte and the frame adds two.
pub const MAX_SETTINGS_BYTES: usize = 253;
/// Longest SSID and password the profile may carry.
pub const MAX_SSID_BYTES: usize = 32;
pub const MAX_PASSWORD_BYTES: usize = 64;
/// The time of a profile must lie in 2024-01-01 to 2099-12-31 (UTC Unix seconds).
pub const MIN_UTC: i64 = 1_704_067_200;
pub const MAX_UTC: i64 = 4_102_444_799;
/// The parser never keeps more than this many bytes of an unfinished frame or of noise.
const MAX_BUFFER: usize = 2 * (MAX_PAYLOAD + 10);

/// Frame types.
pub mod frame_type {
    pub const CURRENT_STATE: u8 = 0x01;
    pub const ERROR_STATE: u8 = 0x02;
    pub const RPC_COMMAND: u8 = 0x03;
    pub const RPC_RESULT: u8 = 0x04;
}

/// RPC command codes.
pub mod command_code {
    pub const WIFI_SETTINGS: u8 = 0x01;
    pub const GET_STATE: u8 = 0x02;
    pub const GET_DEVICE_INFO: u8 = 0x03;
    pub const SCAN_WIFI: u8 = 0x04;
}

/// What the device reports about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ready = 0x02,
    Provisioning = 0x03,
    Provisioned = 0x04,
}

/// Why a command was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    None = 0x00,
    InvalidRpc = 0x01,
    UnknownCommand = 0x02,
    UnableToConnect = 0x03,
    InsecureUrl = 0x04,
    AuthRequired = 0x05,
    AuthFailed = 0x06,
    AuthExpired = 0x07,
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0_u8, |sum, byte| sum.wrapping_add(*byte))
}

/// Wraps a type and a payload into a frame; `None` when the payload does not fit.
#[must_use]
pub fn encode_frame(frame_type: u8, payload: &[u8]) -> Option<Vec<u8>> {
    let length = u8::try_from(payload.len()).ok()?;
    let mut frame = Vec::with_capacity(10 + payload.len());
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&[VERSION, frame_type, length]);
    frame.extend_from_slice(payload);
    frame.push(checksum(&frame));
    Some(frame)
}

/// The frame that reports `state`.
#[must_use]
pub fn state_frame(state: State) -> Vec<u8> {
    encode_frame(frame_type::CURRENT_STATE, &[state as u8]).unwrap_or_default()
}

/// The frame that reports `error`.
#[must_use]
pub fn error_frame(error: ErrorCode) -> Vec<u8> {
    encode_frame(frame_type::ERROR_STATE, &[error as u8]).unwrap_or_default()
}

/// An `RPC_RESULT` frame: the command, the length of the strings and the strings, each with its
/// length. `None` when a string is longer than 255 bytes or they do not fit one frame.
#[must_use]
pub fn result_frame(command: u8, strings: &[&str]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    for text in strings {
        body.push(u8::try_from(text.len()).ok()?);
        body.extend_from_slice(text.as_bytes());
    }
    let mut payload = vec![command, u8::try_from(body.len()).ok()?];
    payload.extend_from_slice(&body);
    encode_frame(frame_type::RPC_RESULT, &payload)
}

/// The answer to `GET_DEVICE_INFO`: name, firmware version, chip and device.
#[must_use]
pub fn device_info_frame(firmware: &str) -> Vec<u8> {
    result_frame(
        command_code::GET_DEVICE_INFO,
        &["Njörun", firmware, "ESP32-P4", "reTerminal D1001"],
    )
    .unwrap_or_default()
}

/// The frames that answer `SCAN_WIFI`.
///
/// One result per network (name, signal in dBm, `YES` when it needs a password) and an empty
/// result that ends the list. Networks whose name does not fit a frame are skipped.
#[must_use]
pub fn scan_frames(networks: &[(&str, i8, bool)]) -> Vec<Vec<u8>> {
    let mut frames: Vec<Vec<u8>> = networks
        .iter()
        .filter_map(|(ssid, rssi, secured)| {
            result_frame(
                command_code::SCAN_WIFI,
                &[ssid, &rssi.to_string(), if *secured { "YES" } else { "NO" }],
            )
        })
        .collect();
    frames.push(result_frame(command_code::SCAN_WIFI, &[]).unwrap_or_default());
    frames
}

/// The result of a successful `WIFI_SETTINGS`: the address a client may offer to visit.
#[must_use]
pub fn redirect_frame(url: &str) -> Vec<u8> {
    result_frame(command_code::WIFI_SETTINGS, &[url]).unwrap_or_default()
}

/// One frame read from the stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub frame_type: u8,
    pub payload: Vec<u8>,
}

/// Finds frames in a byte stream that also carries console text.
#[derive(Debug, Default)]
pub struct Parser {
    buffer: Vec<u8>,
}

impl Parser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds bytes and returns the frames that are complete now. Bytes that cannot start a frame
    /// are dropped; a frame with a wrong version or sum is skipped one byte at a time, so a frame
    /// that starts inside it is still found.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        loop {
            let Some(start) = self.header_start() else {
                // Only a tail that may be the beginning of a magic is worth keeping.
                let keep = self.buffer.len().min(MAGIC.len() - 1);
                let from = self.buffer.len() - keep;
                self.buffer.drain(..from);
                break;
            };
            self.buffer.drain(..start);
            // Text that merely contains the magic is recognized at the first byte that cannot
            // belong to a frame, not after the length it seems to announce.
            if (self.buffer.len() > 6 && self.buffer[6] != VERSION)
                || (self.buffer.len() > 7 && !(1..=4).contains(&self.buffer[7]))
            {
                self.buffer.drain(..1);
                continue;
            }
            if self.buffer.len() < 10 {
                break;
            }
            let length = usize::from(self.buffer[8]);
            let total = 10 + length;
            if self.buffer.len() < total {
                break;
            }
            let valid = self.buffer[6] == VERSION
                && checksum(&self.buffer[..total - 1]) == self.buffer[total - 1];
            if valid {
                frames.push(Frame {
                    frame_type: self.buffer[7],
                    payload: self.buffer[9..total - 1].to_vec(),
                });
                self.buffer.drain(..total);
            } else {
                self.buffer.drain(..1);
            }
        }
        if self.buffer.len() > MAX_BUFFER {
            let from = self.buffer.len() - MAX_BUFFER;
            self.buffer.drain(..from);
        }
        frames
    }

    /// Where the buffer begins to match the magic, completely or as far as it goes.
    fn header_start(&self) -> Option<usize> {
        (0..self.buffer.len()).find(|&start| {
            self.buffer[start..]
                .iter()
                .zip(MAGIC)
                .all(|(byte, expected)| byte == expected)
        })
    }
}

/// The profile of a `WIFI_SETTINGS` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub ssid: String,
    pub password: String,
    /// The Home Assistant address; `None` when the string is absent or empty.
    pub url: Option<String>,
    /// The voucher of a later slice; `None` when absent or empty.
    pub voucher: Option<String>,
    /// The NTP server of a later slice; `None` when absent or empty.
    pub ntp: Option<String>,
    /// The client's UTC time in Unix seconds.
    pub utc: Option<i64>,
}

/// A command that was read from an `RPC_COMMAND` frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    GetState,
    GetDeviceInfo,
    ScanWifi,
    WifiSettings(Settings),
    /// A command this firmware does not know.
    Unknown(u8),
}

/// Reads a command from a frame.
///
/// # Errors
/// [`ErrorCode::InvalidRpc`] for a frame that is no command, whose lengths disagree or whose
/// profile is malformed.
pub fn parse_command(frame: &Frame) -> Result<Command, ErrorCode> {
    if frame.frame_type != frame_type::RPC_COMMAND {
        return Err(ErrorCode::InvalidRpc);
    }
    let (code, data) = match frame.payload.as_slice() {
        [code, length, data @ ..] if usize::from(*length) == data.len() => (*code, data),
        _ => return Err(ErrorCode::InvalidRpc),
    };
    match code {
        command_code::GET_STATE => Ok(Command::GetState),
        command_code::GET_DEVICE_INFO => Ok(Command::GetDeviceInfo),
        command_code::SCAN_WIFI => Ok(Command::ScanWifi),
        command_code::WIFI_SETTINGS => parse_settings(data).map(Command::WifiSettings),
        other => Ok(Command::Unknown(other)),
    }
}

/// Takes one length-prefixed string from `data` at `*position`.
fn take_string<'a>(data: &'a [u8], position: &mut usize) -> Result<&'a str, ErrorCode> {
    let length = usize::from(*data.get(*position).ok_or(ErrorCode::InvalidRpc)?);
    let start = *position + 1;
    let bytes = data
        .get(start..start + length)
        .ok_or(ErrorCode::InvalidRpc)?;
    *position = start + length;
    std::str::from_utf8(bytes).map_err(|_| ErrorCode::InvalidRpc)
}

fn parse_settings(data: &[u8]) -> Result<Settings, ErrorCode> {
    let mut position = 0;
    let ssid = take_string(data, &mut position)?;
    let password = take_string(data, &mut position)?;
    if ssid.is_empty() || ssid.len() > MAX_SSID_BYTES || password.len() > MAX_PASSWORD_BYTES {
        return Err(ErrorCode::InvalidRpc);
    }
    let optional = |position: &mut usize| -> Result<Option<String>, ErrorCode> {
        if *position >= data.len() {
            return Ok(None);
        }
        take_string(data, position).map(|text| Some(text.to_owned()))
    };
    let url = optional(&mut position)?.filter(|text| !text.is_empty());
    let voucher = optional(&mut position)?.filter(|text| !text.is_empty());
    let ntp = optional(&mut position)?.filter(|text| !text.is_empty());
    let utc = match optional(&mut position)? {
        None => None,
        Some(text) => {
            let valid = !text.is_empty()
                && text.len() <= 20
                && text.bytes().all(|byte| byte.is_ascii_digit());
            let seconds = text
                .parse::<i64>()
                .ok()
                .filter(|value| valid && (MIN_UTC..=MAX_UTC).contains(value));
            Some(seconds.ok_or(ErrorCode::InvalidRpc)?)
        }
    };
    if position != data.len() {
        return Err(ErrorCode::InvalidRpc);
    }
    Ok(Settings {
        ssid: ssid.to_owned(),
        password: password.to_owned(),
        url,
        voucher,
        ntp,
        utc,
    })
}

/// What a tablet does with a profile it accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub ssid: String,
    pub password: String,
    pub url: Option<String>,
    pub utc: Option<i64>,
}

/// Decides whether this firmware takes the profile (plan 005, decisions U2, U3 and U5).
///
/// `locked` is true once the tablet has a binding to Home Assistant: then only the read-only
/// commands work.
///
/// # Errors
/// [`ErrorCode::AuthRequired`] when locked, [`ErrorCode::InvalidRpc`] for an address that is no
/// base URL and for a voucher or an NTP server, which a later slice takes.
pub fn check_settings(settings: &Settings, locked: bool) -> Result<Accepted, ErrorCode> {
    if locked {
        return Err(ErrorCode::AuthRequired);
    }
    if settings.voucher.is_some() || settings.ntp.is_some() {
        return Err(ErrorCode::InvalidRpc);
    }
    if settings
        .url
        .as_deref()
        .is_some_and(|url| !valid_base_url(url))
    {
        return Err(ErrorCode::InvalidRpc);
    }
    Ok(Accepted {
        ssid: settings.ssid.clone(),
        password: settings.password.clone(),
        url: settings.url.clone(),
        utc: settings.utc,
    })
}

//! Bounded, same-origin fetching for Home Assistant image attributes.
//!
//! Entity image URLs are untrusted state data. The access token is only sent to the configured
//! Home Assistant origin, redirects are disabled, and response bodies are capped before return.

use std::io::Read;
use std::time::Duration;

use url::Url;

use crate::{ClientError, websocket_url};

const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 2_048;
const MAX_IMAGE_PIXELS: u64 = 4_194_304;
const MAX_TOKEN_BYTES: usize = 8 * 1024;
const MAX_URL_CHARS: usize = 4 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Fetches an HA image over the configured HA origin, using its access token.
///
/// `path` may be a HA-relative path such as `/api/media_player_proxy/...` or an absolute URL
/// with the same origin as `endpoint`. URLs on other origins and non-HTTP schemes are rejected.
/// Redirects are not followed. Only PNG and JPEG responses are accepted; encoded bodies are
/// capped at 2 MiB and image dimensions at 2,048 pixels per side / 4 megapixels.
///
/// # Errors
/// Returns a sanitized [`ClientError`]. The endpoint, path and access token are never included
/// in error messages.
pub fn fetch(
    endpoint: &str,
    token: &str,
    allow_http: bool,
    path: &str,
) -> Result<Vec<u8>, ClientError> {
    if token.is_empty()
        || token.len() > MAX_TOKEN_BYTES
        || !token.is_ascii()
        || token.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(ClientError::Authentication);
    }

    let url = image_url(endpoint, allow_http, path)?;
    let authorization = format!("Bearer {token}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .max_redirects(0)
        .build()
        .into();
    let mut response = agent
        .get(url.as_str())
        .header("Authorization", &authorization)
        .header("Accept", "image/png,image/jpeg")
        .call()
        .map_err(|error| map_http_error(&error))?;

    if !response.status().is_success() {
        return Err(ClientError::Rejected);
    }

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(ClientError::Protocol)?
        .to_ascii_lowercase();
    if !supported_mime(&content_type) {
        return Err(ClientError::Protocol);
    }

    if let Some(length) = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        && length > MAX_IMAGE_BYTES
    {
        return Err(ClientError::Limit);
    }

    let mut body = Vec::new();
    let read_result = response
        .body_mut()
        .as_reader()
        .take(u64::try_from(MAX_IMAGE_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut body);
    read_result.map_err(|error| map_read_error(&error))?;
    if body.len() > MAX_IMAGE_BYTES {
        return Err(ClientError::Limit);
    }
    if !matches_mime_signature(&content_type, &body) {
        return Err(ClientError::Protocol);
    }
    image_dimensions(&content_type, &body)?;
    Ok(body)
}

fn endpoint_base(endpoint: &str, allow_http: bool) -> Result<Url, ClientError> {
    // Share the HA endpoint's scheme, credentials, query, and local HTTP-consent validation.
    let mut websocket = websocket_url(endpoint, allow_http)?;
    let original_scheme = match websocket.scheme() {
        "wss" => "https",
        "ws" => "http",
        _ => return Err(ClientError::InvalidEndpoint),
    };
    websocket
        .set_scheme(original_scheme)
        .map_err(|()| ClientError::InvalidEndpoint)?;
    let base_path = websocket
        .path()
        .strip_suffix("/api/websocket")
        .ok_or(ClientError::InvalidEndpoint)?;
    let normalized_path = if base_path.is_empty() {
        "/".to_owned()
    } else if base_path.ends_with('/') {
        base_path.to_owned()
    } else {
        format!("{base_path}/")
    };
    websocket.set_path(&normalized_path);
    websocket.set_query(None);
    websocket.set_fragment(None);
    Ok(websocket)
}

fn image_url(endpoint: &str, allow_http: bool, path: &str) -> Result<Url, ClientError> {
    if path.is_empty() || path.chars().count() > MAX_URL_CHARS || path.contains('\0') {
        return Err(ClientError::InvalidEndpoint);
    }
    let base = endpoint_base(endpoint, allow_http)?;
    let target = match Url::parse(path) {
        Ok(absolute) => absolute,
        Err(_) => base.join(path).map_err(|_| ClientError::InvalidEndpoint)?,
    };
    if !matches!(target.scheme(), "http" | "https")
        || !target.username().is_empty()
        || target.password().is_some()
        || target.fragment().is_some()
        || target.host_str().is_none()
        || target.origin() != base.origin()
    {
        return Err(ClientError::InvalidEndpoint);
    }
    Ok(target)
}

fn supported_mime(mime: &str) -> bool {
    matches!(mime, "image/jpeg" | "image/png")
}

fn matches_mime_signature(mime: &str, bytes: &[u8]) -> bool {
    match mime {
        "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        _ => false,
    }
}

fn image_dimensions(mime: &str, bytes: &[u8]) -> Result<(u32, u32), ClientError> {
    let (width, height) = match mime {
        "image/jpeg" => jpeg_dimensions(bytes),
        "image/png" => png_dimensions(bytes),
        _ => Err(ClientError::Protocol),
    }?;
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Err(ClientError::Limit);
    }
    Ok((width, height))
}

fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), ClientError> {
    if bytes.len() < 24 || &bytes[12..16] != b"IHDR" {
        return Err(ClientError::Protocol);
    }
    Ok((
        u32::from_be_bytes(
            bytes[16..20]
                .try_into()
                .map_err(|_| ClientError::Protocol)?,
        ),
        u32::from_be_bytes(
            bytes[20..24]
                .try_into()
                .map_err(|_| ClientError::Protocol)?,
        ),
    ))
}

fn jpeg_dimensions(bytes: &[u8]) -> Result<(u32, u32), ClientError> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Err(ClientError::Protocol);
    }
    let mut offset = 2;
    while offset < bytes.len() {
        if bytes[offset] != 0xff {
            offset += 1;
            continue;
        }
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        let Some(&marker) = bytes.get(offset) else {
            break;
        };
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            continue;
        }
        let Some(length_bytes) = bytes.get(offset..offset + 2) else {
            return Err(ClientError::Protocol);
        };
        let segment_length = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if segment_length < 2 {
            return Err(ClientError::Protocol);
        }
        let segment_end = offset
            .checked_add(segment_length)
            .filter(|end| *end <= bytes.len())
            .ok_or(ClientError::Protocol)?;
        if is_jpeg_sof(marker) {
            if segment_length < 7 {
                return Err(ClientError::Protocol);
            }
            let height = u32::from(u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]));
            let width = u32::from(u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]));
            return Ok((width, height));
        }
        offset = segment_end;
    }
    Err(ClientError::Protocol)
}

const fn is_jpeg_sof(marker: u8) -> bool {
    matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf)
}

const fn map_http_error(error: &ureq::Error) -> ClientError {
    match error {
        ureq::Error::Timeout(_) => ClientError::Timeout,
        ureq::Error::StatusCode(_)
        | ureq::Error::TooManyRedirects
        | ureq::Error::RedirectFailed => ClientError::Rejected,
        _ => ClientError::Connection,
    }
}

fn map_read_error(error: &std::io::Error) -> ClientError {
    if error.kind() == std::io::ErrorKind::TimedOut {
        ClientError::Timeout
    } else {
        ClientError::Connection
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_IMAGE_BYTES, endpoint_base, fetch, image_dimensions, image_url, matches_mime_signature,
        supported_mime,
    };
    use crate::ClientError;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn resolves_relative_and_absolute_same_origin_image_urls() {
        let base = endpoint_base("https://ha.example.test:8443/ha", false).unwrap();
        assert_eq!(base.as_str(), "https://ha.example.test:8443/ha/");
        assert_eq!(
            image_url("https://ha.example.test:8443/ha", false, "/api/image.png")
                .unwrap()
                .as_str(),
            "https://ha.example.test:8443/api/image.png"
        );
        assert_eq!(
            image_url("https://ha.example.test:8443/ha", false, "api/image.png")
                .unwrap()
                .as_str(),
            "https://ha.example.test:8443/ha/api/image.png"
        );
        assert_eq!(
            image_url(
                "https://ha.example.test:8443/ha",
                false,
                "https://ha.example.test:8443/local/cover.jpg?auth=opaque"
            )
            .unwrap()
            .as_str(),
            "https://ha.example.test:8443/local/cover.jpg?auth=opaque"
        );
    }

    #[test]
    fn rejects_foreign_origins_unsafe_schemes_and_http_without_consent() {
        for path in [
            "https://attacker.example.test/image.png",
            "http://ha.example.test/image.png",
            "//attacker.example.test/image.png",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://user:password@ha.example.test/image.png",
            "https://ha.example.test/image.png#fragment",
        ] {
            assert_eq!(
                image_url("https://ha.example.test", false, path),
                Err(ClientError::InvalidEndpoint),
                "accepted unsafe image URL {path}"
            );
        }
        assert_eq!(
            image_url("http://ha.example.test", false, "/image.png"),
            Err(ClientError::HttpNotAllowed)
        );
        assert!(image_url("http://ha.example.test", true, "/image.png").is_ok());
    }

    #[test]
    fn accepts_only_supported_image_mimes_with_matching_signatures() {
        assert!(supported_mime("image/png"));
        assert!(supported_mime("image/jpeg"));
        assert!(!supported_mime("image/webp"));
        assert!(!supported_mime("image/svg+xml"));
        assert!(!supported_mime("image/avif"));
        assert!(matches_mime_signature(
            "image/png",
            b"\x89PNG\r\n\x1a\nrest"
        ));
        assert!(!matches_mime_signature("image/png", b"<html>not an image"));
        assert!(matches_mime_signature(
            "image/jpeg",
            &[0xff, 0xd8, 0xff, 0x00]
        ));
        assert!(!matches_mime_signature("image/jpeg", b"GIF89a"));
    }

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes
    }

    #[test]
    fn image_dimension_sniffing_rejects_large_or_incomplete_headers() {
        let valid = png_header(2048, 2048);
        assert_eq!(image_dimensions("image/png", &valid), Ok((2048, 2048)));
        assert_eq!(
            image_dimensions("image/png", &png_header(2049, 1)),
            Err(ClientError::Limit)
        );
        assert_eq!(
            image_dimensions("image/png", b"\x89PNG\r\n\x1a\n"),
            Err(ClientError::Protocol)
        );
        let jpeg_oversized = [
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01, 0x10, 0x00, 0x01, 0x01, 0x11,
            0x00,
        ];
        assert_eq!(
            image_dimensions("image/jpeg", &jpeg_oversized),
            Err(ClientError::Limit)
        );
        let jpeg_valid = [
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x01, 0xe0, 0x02, 0x80, 0x01, 0x01, 0x11,
            0x00,
        ];
        assert_eq!(image_dimensions("image/jpeg", &jpeg_valid), Ok((640, 480)));
    }

    fn serve_once(status: &str, mime: &str, body: Vec<u8>) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_owned();
        let mime = mime.to_owned();
        let thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap_or(0);
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            let _ = stream.write_all(&body);
            String::from_utf8_lossy(&request).into_owned()
        });
        (format!("http://{address}"), thread)
    }

    #[test]
    fn fetch_sends_bearer_only_to_ha_and_returns_a_bounded_image() {
        let bytes = png_header(640, 480);
        let (endpoint, server) = serve_once("200 OK", "image/png", bytes.clone());
        let token = "super-secret-access-token";
        let result = fetch(&endpoint, token, true, "/api/image.png").unwrap();
        let request = server.join().unwrap();
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer super-secret-access-token")
        );
        assert_eq!(result, bytes);

        // Errors use fixed messages and never echo endpoint, path, or authorization values.
        let (endpoint, server) = serve_once("200 OK", "text/html", b"no".to_vec());
        let error = fetch(&endpoint, token, true, "/secret/path").unwrap_err();
        let _ = server.join().unwrap();
        let message = error.to_string();
        assert!(!message.contains(token));
        assert!(!message.contains("secret/path"));
    }

    #[test]
    fn fetch_does_not_follow_redirects_or_accept_oversized_responses() {
        let (endpoint, server) =
            serve_once("302 Found", "image/png", b"\x89PNG\r\n\x1a\n".to_vec());
        let result = fetch(&endpoint, "token", true, "/redirect");
        assert!(result.is_err());
        let _ = server.join().unwrap();

        let too_large = vec![0; MAX_IMAGE_BYTES + 1];
        let (endpoint, server) = serve_once("200 OK", "image/png", too_large);
        assert_eq!(
            fetch(&endpoint, "token", true, "/large"),
            Err(ClientError::Limit)
        );
        let _ = server.join().unwrap();
    }

    #[test]
    fn fetch_rejects_compressed_image_headers_with_oversized_dimensions() {
        let (endpoint, server) = serve_once("200 OK", "image/png", png_header(4096, 4096));
        assert_eq!(
            fetch(&endpoint, "token", true, "/oversized.png"),
            Err(ClientError::Limit)
        );
        let _ = server.join().unwrap();
    }
}

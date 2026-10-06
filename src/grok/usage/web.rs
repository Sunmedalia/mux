//! Read-only web billing fallback when the CLI proxy omits the usage scalar.
//! Protocol reference: GrokWebBillingFetcher in steipete/CodexBar.
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{io::Read, time::Duration};
const ENDPOINT: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
#[derive(Debug)]
enum Field<'a> {
    Integer(u64),
    Float(f32),
    Message(&'a [u8]),
    Other,
}
fn take<'a>(bytes: &mut &'a [u8], size: usize) -> Result<&'a [u8]> {
    if size > bytes.len() {
        bail!("Invalid Grok billing message");
    }
    let (value, rest) = bytes.split_at(size);
    *bytes = rest;
    Ok(value)
}
fn integer(bytes: &mut &[u8]) -> Result<u64> {
    let mut value = 0;
    for shift in (0..=63).step_by(7) {
        let byte = take(bytes, 1)?[0];
        if shift == 63 && byte > 1 {
            bail!("Invalid Grok billing integer");
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    bail!("Invalid Grok billing integer")
}
fn fields(mut bytes: &[u8]) -> Result<Vec<(u64, Field<'_>)>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        if result.len() >= 4096 {
            bail!("Grok billing message exceeds field limit");
        }
        let key = integer(&mut bytes)?;
        let number = key >> 3;
        if number == 0 {
            bail!("Invalid Grok billing field");
        }
        let field = match key & 7 {
            0 => Field::Integer(integer(&mut bytes)?),
            1 => {
                take(&mut bytes, 8)?;
                Field::Other
            }
            2 => {
                let size =
                    usize::try_from(integer(&mut bytes)?).context("Invalid Grok billing length")?;
                Field::Message(take(&mut bytes, size)?)
            }
            5 => Field::Float(f32::from_le_bytes(take(&mut bytes, 4)?.try_into().unwrap())),
            _ => bail!("Invalid Grok billing wire type"),
        };
        result.push((number, field));
    }
    Ok(result)
}
fn field<'a>(fields: &'a [(u64, Field<'a>)], number: u64) -> Result<Option<&'a Field<'a>>> {
    let mut matches = fields.iter().filter(|(id, _)| *id == number);
    let value = matches.next().map(|(_, value)| value);
    if matches.next().is_some() {
        bail!("Duplicate Grok billing field");
    }
    Ok(value)
}
fn message<'a>(fields: &'a [(u64, Field<'a>)], number: u64) -> Result<Option<&'a [u8]>> {
    match field(fields, number)? {
        Some(Field::Message(bytes)) => Ok(Some(bytes)),
        None => Ok(None),
        _ => bail!("Invalid Grok billing message field"),
    }
}
fn timestamp(bytes: &[u8]) -> Result<DateTime<Utc>> {
    let fields = fields(bytes)?;
    let Some(Field::Integer(seconds)) = field(&fields, 1)? else {
        bail!("Missing Grok billing timestamp");
    };
    DateTime::from_timestamp(
        i64::try_from(*seconds).context("Invalid Grok billing timestamp")?,
        0,
    )
    .context("Invalid Grok billing timestamp")
}
fn status(value: &str) -> Result<()> {
    if value.trim() != "0" {
        bail!("Grok web billing RPC failed");
    }
    Ok(())
}
fn parse(
    bytes: &[u8],
    now: DateTime<Utc>,
    header_status: Option<&str>,
) -> Result<(f64, Option<DateTime<Utc>>)> {
    let mut bytes = bytes;
    let mut payload = None;
    let mut successful = false;
    if let Some(value) = header_status {
        status(value)?;
        successful = true;
    }
    let mut trailers = false;
    while !bytes.is_empty() {
        let header = take(&mut bytes, 5)?;
        let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
        let body = take(&mut bytes, length)?;
        match header[0] {
            0 if payload.is_none() && !trailers => payload = Some(body),
            128 if !trailers => {
                trailers = true;
                let text = std::str::from_utf8(body).context("Invalid Grok billing trailers")?;
                let mut count = 0;
                for line in text.lines() {
                    if let Some((key, value)) = line.split_once(':')
                        && key.eq_ignore_ascii_case("grpc-status")
                    {
                        count += 1;
                        status(value)?;
                        successful = true;
                    }
                }
                if count != 1 {
                    bail!("Missing or duplicate Grok billing RPC status");
                }
            }
            _ => bail!("Invalid Grok billing frame"),
        }
    }
    if !successful {
        bail!("Missing Grok billing RPC status");
    }
    let root = fields(payload.context("Missing Grok billing response")?)?;
    let config = fields(message(&root, 1)?.context("Missing Grok billing config")?)?;
    let period = message(&config, 8)?.map(fields).transpose()?;
    let end = period
        .as_ref()
        .map(|period| message(period, 3))
        .transpose()?
        .flatten()
        .map(timestamp)
        .transpose()?;
    match field(&config, 1)? {
        Some(Field::Float(percent)) if percent.is_finite() && *percent >= 0.0 => {
            Ok((f64::from(*percent), end))
        }
        Some(_) => bail!("Invalid Grok web credit usage percentage"),
        None => {
            // Only a complete, successful protobuf response with an active typed
            // period establishes proto3's omitted zero scalar. JSON omission alone does not.
            let period = period.context("Grok web usage percentage unavailable")?;
            let active_type = matches!(field(&period, 1)?, Some(Field::Integer(1 | 2)));
            let start = message(&period, 2)?.map(timestamp).transpose()?;
            if active_type
                && start.is_some_and(|start| start <= now)
                && end.is_some_and(|end| end > now)
                && !config
                    .iter()
                    .any(|(_, field)| matches!(field, Field::Float(_)))
            {
                Ok((0.0, end))
            } else {
                bail!("Grok web usage percentage unavailable");
            }
        }
    }
}
pub(super) fn fetch(entry: &Value) -> Result<(f64, Option<DateTime<Utc>>)> {
    fetch_at(ENDPOINT, entry)
}
fn fetch_at(endpoint: &str, entry: &Value) -> Result<(f64, Option<DateTime<Utc>>)> {
    let token = entry["key"]
        .as_str()
        .or_else(|| entry["access_token"].as_str())
        .context("Missing Grok OAuth token")?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| anyhow::anyhow!("Cannot create Grok billing client"))?;
    let mut response = client
        .post(endpoint)
        .bearer_auth(token)
        .header("Content-Type", "application/grpc-web+proto")
        .header("x-grpc-web", "1")
        .header("Origin", "https://grok.com")
        .header("Referer", "https://grok.com/?_s=usage")
        .header("x-user-agent", "connect-es/2.1.1")
        .header("User-Agent", "Mux")
        .body(vec![0, 0, 0, 0, 2, 8, 0])
        .send()
        .map_err(|_| anyhow::anyhow!("Cannot reach Grok web billing"))?;
    if !response.status().is_success() {
        bail!(
            "Grok web billing returned HTTP {}",
            response.status().as_u16()
        );
    }
    let rpc_status = response
        .headers()
        .get("grpc-status")
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()
        .map_err(|_| anyhow::anyhow!("Invalid Grok billing RPC status"))?;
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("Cannot read Grok web billing"))?;
    if bytes.len() > 1024 * 1024 {
        bail!("Grok billing response exceeds size limit");
    }
    parse(&bytes, Utc::now(), rpc_status.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        while value >= 128 {
            out.push((value as u8 & 127) | 128);
            value >>= 7;
        }
        out.push(value as u8);
        out
    }
    fn number(field: u64, value: u64) -> Vec<u8> {
        let mut bytes = varint(field << 3);
        bytes.extend(varint(value));
        bytes
    }
    fn message(field: u64, data: &[u8]) -> Vec<u8> {
        let mut bytes = varint(field << 3 | 2);
        bytes.extend(varint(data.len() as u64));
        bytes.extend(data);
        bytes
    }
    fn framed(flag: u8, data: &[u8]) -> Vec<u8> {
        let mut bytes = vec![flag];
        bytes.extend((data.len() as u32).to_be_bytes());
        bytes.extend(data);
        bytes
    }
    fn response(percent: Option<f32>, start: i64, end: i64) -> Vec<u8> {
        let mut period = number(1, 2);
        period.extend(message(2, &number(1, start as u64)));
        period.extend(message(3, &number(1, end as u64)));
        let mut config = message(8, &period);
        if let Some(percent) = percent {
            config.push(13);
            config.extend(percent.to_le_bytes());
        }
        let mut bytes = framed(0, &message(1, &config));
        bytes.extend(framed(128, b"grpc-status: 0\r\n"));
        bytes
    }
    #[test]
    fn explicit_percent_and_complete_active_zero_period_are_distinct_from_unknown() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let end = now.timestamp() + 86400;
        let (percent, reset) =
            parse(&response(Some(42.5), now.timestamp() - 100, end), now, None).unwrap();
        assert_eq!(percent, 42.5);
        assert_eq!(reset.unwrap().timestamp(), end);
        assert_eq!(
            parse(&response(None, now.timestamp() - 100, end), now, None)
                .unwrap()
                .0,
            0.0
        );
        assert!(
            parse(
                &response(None, now.timestamp() - 100, now.timestamp() - 1),
                now,
                None
            )
            .is_err()
        );
        assert!(parse(&response(None, now.timestamp() + 1, end), now, None).is_err());
        assert!(
            parse(
                &response(Some(f32::NAN), now.timestamp() - 100, end),
                now,
                None
            )
            .is_err()
        );
        let mut metadata_only = framed(0, &message(1, &message(5, &number(1, end as u64))));
        metadata_only.extend(framed(128, b"grpc-status: 0\r\n"));
        assert!(parse(&metadata_only, now, None).is_err());
    }
    #[test]
    fn incomplete_failed_duplicate_or_compressed_responses_never_become_zero_usage() {
        let now = Utc::now();
        let good = response(None, now.timestamp() - 10, now.timestamp() + 86400);
        for size in 0..good.len() {
            assert!(parse(&good[..size], now, None).is_err());
        }
        assert!(parse(&good, now, Some("16")).is_err());
        let mut compressed = good.clone();
        compressed[0] = 1;
        assert!(parse(&compressed, now, None).is_err());
        let mut duplicate = good.clone();
        duplicate.extend(&good);
        assert!(parse(&duplicate, now, None).is_err());
        let mut failed = framed(0, &message(1, &[]));
        failed.extend(framed(128, b"grpc-status: 16\r\ngrpc-message: SECRET\r\n"));
        let error = parse(&failed, now, None).unwrap_err().to_string();
        assert!(!error.contains("SECRET"));
    }
    #[test]
    fn billing_fallback_is_authenticated_read_only_and_errors_never_echo_bodies() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
        };
        for status in ["200 OK", "403 Forbidden", "302 Found"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/billing", listener.local_addr().unwrap());
            let body = if status == "200 OK" {
                let now = Utc::now().timestamp();
                response(Some(23.0), now - 100, now + 1000)
            } else {
                b"SECRET".to_vec()
            };
            let worker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                }
                let mut request_body = [0; 7];
                reader.read_exact(&mut request_body).unwrap();
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                socket.write_all(&body).unwrap();
                (headers.to_lowercase(), request_body)
            });
            let result = fetch_at(&endpoint, &serde_json::json!({"key":"SECRET"}));
            let (headers, body) = worker.join().unwrap();
            assert!(
                headers.starts_with("post /billing ")
                    && headers.contains("authorization: bearer secret")
            );
            assert!(headers.contains("application/grpc-web+proto"));
            assert_eq!(body, [0, 0, 0, 0, 2, 8, 0]);
            if status == "200 OK" {
                assert_eq!(result.unwrap().0, 23.0);
            } else {
                assert!(!result.unwrap_err().to_string().contains("SECRET"));
            }
        }
    }
}

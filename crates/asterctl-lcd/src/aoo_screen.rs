// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

use crate::FakeSerialPort;
use crate::ToRgb565;

use anyhow::{Context, anyhow};
use bytes::{BufMut, BytesMut};
use log::{debug, error, info, warn};
use serialport::{SerialPort, SerialPortType};
use std::io::{Read, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};

pub const DISPLAY_SIZE: (u32, u32) = (960, 376);

const SERIAL_RETRY: u8 = 3;
const UART_BAUDRATE: u32 = 1_500_000;

const USB_UART_VID: u16 = 0x416;
const USB_UART_PID: u16 = 0x90A1;

const IMG_CHUNK_SIZE: usize = 47;

static DISPLAY_OFF: [u8; 8] = [0xAA, 0x55, 0xAA, 0x55, 0x0A, 0x00, 0x00, 0x00];
static DISPLAY_ON: [u8; 8] = [0xAA, 0x55, 0xAA, 0x55, 0x0B, 0x00, 0x00, 0x00];

static HEADER_START: [u8; 16] = [
    0xAA, 0x55, 0xAA, 0x55, 0x05, 0x00, 0x00, 0x00, 0x04, 0x00, 0x0F, 0x2F, 0x00, 0x04, 0x0B, 0x00,
];
static HEADER_END: [u8; 8] = [0xAA, 0x55, 0xAA, 0x55, 0x06, 0x00, 0x00, 0x00];
static HEADER: [u8; 8] = [0xAA, 0x55, 0xAA, 0x55, 0x08, 0x00, 0x00, 0x00];

#[derive(Default)]
pub struct AooScreenBuilder {
    timeout: Option<Duration>,
    enable_cache: Option<bool>,
    no_init_check: Option<bool>,
}

#[allow(dead_code)]
impl AooScreenBuilder {
    pub fn new() -> Self {
        Self::default()
    }
    /// Set the amount of time to wait to receive data before timing out. Defaults to 1 sec.
    pub fn timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = Some(timeout);
        self
    }

    /// Cache previous frame sent to display for future diff updates. Enabled by default.
    pub fn enable_cache(&mut self, enable: bool) -> &mut Self {
        self.enable_cache = Some(enable);
        self
    }

    /// Disable LCD initialization check and only write data to the display. Defaults to false.
    pub fn no_init_check(&mut self, no_check: bool) -> &mut Self {
        self.no_init_check = Some(no_check);
        self
    }

    /// Open the default AOOSTAR LCD USB UART device 416:90A1.
    pub fn open_default(self) -> anyhow::Result<AooScreen> {
        self.open_usb(USB_UART_VID, USB_UART_PID)
    }

    /// Simulate the LCD device. No real device or serial port is required.
    pub fn simulate(self) -> anyhow::Result<AooScreen> {
        Ok(AooScreen {
            port: Some(Box::new(FakeSerialPort::new())),
            enable_cache: self.enable_cache.unwrap_or(true),
            prev_frame: None,
            no_init_check: self.no_init_check.unwrap_or(false),
            timeout: Duration::from_millis(1000),
            reopen: Reopen::Simulated,
        })
    }

    /// Like `simulate`, but the fake port fails its first `fail_writes`
    /// writes — for tests of the reconnect logic.
    pub fn simulate_with_failures(self, fail_writes: u32) -> anyhow::Result<AooScreen> {
        Ok(AooScreen {
            port: Some(Box::new(FakeSerialPort::new().with_failures(fail_writes))),
            enable_cache: self.enable_cache.unwrap_or(true),
            prev_frame: None,
            no_init_check: self.no_init_check.unwrap_or(false),
            timeout: Duration::from_millis(1000),
            reopen: Reopen::Simulated,
        })
    }

    /// Open the specified USB UART device id. Format: vid:pid
    pub fn open_usb_id(self, id: &str) -> anyhow::Result<AooScreen> {
        let (vid, pid) = id
            .split_once(':')
            .with_context(|| "Error parsing serial port ID. Expected `vid:pid` format.")?;
        self.open_usb(u16::from_str_radix(vid, 16)?, u16::from_str_radix(pid, 16)?)
    }

    /// Open the specified USB UART
    pub fn open_usb(self, vid: u16, pid: u16) -> anyhow::Result<AooScreen> {
        let serial_dev = find_usb_serial_port(vid, pid)?;
        let timeout = self.timeout.unwrap_or(Duration::from_millis(1000));
        let port = open_serial_port(&serial_dev, timeout)?;
        Ok(AooScreen {
            port: Some(port),
            enable_cache: self.enable_cache.unwrap_or(true),
            prev_frame: None,
            no_init_check: self.no_init_check.unwrap_or(false),
            timeout,
            reopen: Reopen::Usb { vid, pid },
        })
    }

    /// Open the specified serial device
    pub fn open_device(self, device: &str) -> anyhow::Result<AooScreen> {
        let timeout = self.timeout.unwrap_or(Duration::from_millis(1000));
        let port = open_serial_port(device, timeout)?;
        Ok(AooScreen {
            port: Some(port),
            enable_cache: self.enable_cache.unwrap_or(true),
            prev_frame: None,
            no_init_check: self.no_init_check.unwrap_or(false),
            timeout,
            reopen: Reopen::Device(device.to_string()),
        })
    }
}

/// How to reopen the LCD port after a serial failure (e.g. after resume).
enum Reopen {
    /// Discover by USB VID/PID — robust against the COM number changing.
    Usb { vid: u16, pid: u16 },
    /// Reopen an explicit serial device name.
    Device(String),
    /// Simulated port: reconnect just creates a fresh fake.
    Simulated,
}

pub struct AooScreen {
    port: Option<Box<dyn SerialPort>>,
    enable_cache: bool,
    prev_frame: Option<BytesMut>,
    no_init_check: bool,
    /// Resolved serial timeout (builder default 1s).
    timeout: Duration,
    /// How to reopen the port after a serial failure (e.g. after resume).
    reopen: Reopen,
}

#[allow(dead_code)]
impl AooScreen {
    pub fn init(&mut self) -> anyhow::Result<()> {
        let port = self.port.as_mut().ok_or(anyhow!("LCD port not open"))?;

        port.write(&DISPLAY_ON)
            .with_context(|| "Error sending display on command")?;

        if self.no_init_check {
            warn!("Test mode: only writing to the display");
        } else {
            // quick and dirty response check as in the original app
            sleep(Duration::from_secs(1));

            let available = port
                .bytes_to_read()
                .with_context(|| "Failed to get available bytes from serial port")?;
            if available == 0 {
                return Err(anyhow!("Initialization failed, no response received"));
            }
            let mut serial_buf: Vec<u8> = vec![0; available as usize];
            port.read(serial_buf.as_mut_slice())
                .with_context(|| "Failed to read from serial port")?;

            let marker = b'A';
            if !serial_buf.contains(&marker) {
                return Err(anyhow!(
                    "Initialization failed, received: {}",
                    String::from_utf8_lossy(&serial_buf)
                ));
            }
        }

        info!("Display initialized!");

        Ok(())
    }

    pub fn close(&mut self) {
        if self.port.is_some() {
            if let Err(e) = self.off() {
                warn!("Failed to close display: {e}");
            }
            self.port = None;
        }
    }

    pub fn on(&mut self) -> anyhow::Result<()> {
        self.send(&DISPLAY_ON)
            .with_context(|| "Failed to send display on")
    }

    pub fn off(&mut self) -> anyhow::Result<()> {
        self.send(&DISPLAY_OFF)
            .with_context(|| "Failed to send display off")
    }

    pub fn send_image(&mut self, image: impl ToRgb565) -> anyhow::Result<()> {
        let img_rgb565 = image.to_rgb565_le();
        debug!(
            "Start sending image (size {}) {} cache... ",
            img_rgb565.len(),
            if self.enable_cache && self.prev_frame.is_some() {
                "with"
            } else {
                "without"
            }
        );

        let start_time = Instant::now();
        self.write(&HEADER_START)
            .with_context(|| "Failed to send header start")?;

        let mut buf = BytesMut::with_capacity(HEADER.len() + 4 + IMG_CHUNK_SIZE);
        let mut sent_chunks = 0;
        for (idx, chunk) in img_rgb565.chunks(IMG_CHUNK_SIZE).enumerate() {
            let offset = idx * IMG_CHUNK_SIZE;

            if self.enable_cache
                && let Some(cache) = self.prev_frame.as_mut()
            {
                let offset = idx * IMG_CHUNK_SIZE;
                if offset + IMG_CHUNK_SIZE <= cache.len()
                    && cache[offset..offset + IMG_CHUNK_SIZE].eq(chunk)
                {
                    // Block is unchanged from the previous frame; skip sending
                    continue;
                }
            }

            buf.clear();
            buf.extend(&HEADER);
            buf.put_u32_le(offset as u32);
            buf.extend(chunk);

            self.write(&buf)
                .with_context(|| format!("Failed to send image data chunk {idx}"))?;
            sent_chunks += 1;
        }

        self.write(&HEADER_END)
            .with_context(|| "Failed to send header end")?;

        // Single flush for the entire frame instead of one per chunk
        let port = self.port.as_mut().ok_or(anyhow!("LCD port not open"))?;
        port.flush().with_context(|| "Failed to flush image data")?;

        if self.enable_cache {
            self.prev_frame.replace(img_rgb565);
        }

        debug!(
            "Image sent: {}ms, {sent_chunks} chunks",
            start_time.elapsed().as_millis()
        );

        Ok(())
    }

    pub fn enable_cache(&mut self, enable: bool) {
        self.enable_cache = enable;
        if !enable {
            self.clear_cache();
        }
    }

    pub fn is_cache_enabled(&self) -> bool {
        self.enable_cache
    }

    pub fn clear_cache(&mut self) {
        self.prev_frame = None;
    }

    /// Drops the current serial handle and reopens the LCD port, re-running
    /// the initialization handshake. The frame cache is cleared so the next
    /// `send_image` sends a full frame (after a resume the display may be in
    /// an arbitrary state).
    pub fn reconnect(&mut self) -> anyhow::Result<()> {
        warn!("Reconnecting LCD serial port");
        self.port = None;
        self.prev_frame = None;

        let port: Box<dyn SerialPort> = match &self.reopen {
            Reopen::Simulated => Box::new(FakeSerialPort::new()),
            Reopen::Device(name) => open_serial_port(name, self.timeout)?,
            Reopen::Usb { vid, pid } => {
                let device = find_usb_serial_port(*vid, *pid)?;
                open_serial_port(&device, self.timeout)?
            }
        };

        self.port = Some(port);
        if let Err(e) = self.init() {
            // Never leave a half-open handle behind: a port whose handshake
            // failed must not be usable by a caller that ignores the error.
            self.port = None;
            return Err(e);
        }
        info!("LCD reconnected");
        Ok(())
    }

    /// Backoff delay for the n-th reconnect attempt: 1s → 2s → ... → 60s
    /// cap. Extracted so the growth/cap is unit-testable.
    fn backoff_delay(attempt: u32) -> Duration {
        let secs = 1u64 << attempt.min(6);
        Duration::from_secs(secs.min(60))
    }

    /// Attempts to reconnect forever, backing off between attempts (1s →
    /// 2s → ... → 60s cap). Never returns an error: on failure it logs and
    /// retries. Returns once the port is open and initialized again.
    pub fn reconnect_with_retry(&mut self) {
        let mut attempt = 0;
        loop {
            match self.reconnect() {
                Ok(()) => {
                    info!("LCD reconnected");
                    return;
                }
                Err(e) => {
                    let delay = Self::backoff_delay(attempt);
                    warn!("Reconnect failed ({e:?}); retrying in {}s", delay.as_secs());
                    sleep(delay);
                    attempt += 1;
                }
            }
        }
    }

    /// Write data and flush immediately. Used for control commands (on/off/init).
    fn send(&mut self, data: &[u8]) -> anyhow::Result<()> {
        self.write(data)?;
        let port = self.port.as_mut().ok_or(anyhow!("LCD port not open"))?;
        port.flush()?;
        Ok(())
    }

    /// Write data without flushing. Used for image data where a single flush at the end suffices.
    fn write(&mut self, data: &[u8]) -> anyhow::Result<()> {
        // TODO not sure if retry logic is required. Need a real device to test...
        let mut retry = 0;

        let port = self.port.as_mut().ok_or(anyhow!("LCD port not open"))?;

        loop {
            return match port.write_all(data) {
                Ok(()) => Ok(()),
                Err(e) => {
                    debug!(
                        "Bytes queued to send: {}",
                        port.bytes_to_write()
                            .with_context(|| "Error calling bytes_to_write")?
                    );
                    if retry < SERIAL_RETRY {
                        warn!("Failed to write to display, retrying! Error: {e}");
                        retry += 1;
                        continue;
                    }
                    error!("Failed to write to display: {e}");
                    Err(e.into())
                }
            };
        }
    }
}

fn open_serial_port(device: &str, timeout: Duration) -> anyhow::Result<Box<dyn SerialPort>> {
    let port = serialport::new(device, UART_BAUDRATE)
        .timeout(timeout)
        .open()
        .with_context(|| format!("Error opening serial port: {device}"))?;

    info!(
        "Opened serial port {device}: baud={}, {}:{}:{}",
        port.baud_rate()?,
        port.data_bits()?,
        port.parity()?,
        port.stop_bits()?
    );

    Ok(port)
}

pub fn find_usb_serial_port(vid: u16, pid: u16) -> serialport::Result<String> {
    info!("Looking for USB serial port {vid:x}:{pid:x}");
    let ports = serialport::available_ports()?;
    for p in ports {
        debug!("Found serial port: {}", p.port_name);
        if let SerialPortType::UsbPort(info) = p.port_type
            && info.pid == pid
            && info.vid == vid
        {
            return Ok(p.port_name);
        }
    }

    Err(serialport::Error::new(
        serialport::ErrorKind::NoDevice,
        format!("USB serial port {vid:x}:{pid:x} not found"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbImage;

    fn blank_image() -> RgbImage {
        RgbImage::new(DISPLAY_SIZE.0, DISPLAY_SIZE.1)
    }

    #[test]
    fn fake_port_fails_then_recovers() {
        let mut port = FakeSerialPort::new().with_failures(4);
        let buf = [0u8; 8];
        assert!(port.write(&buf).is_err());
        assert!(port.write(&buf).is_err());
        assert!(port.write(&buf).is_err());
        assert!(port.write(&buf).is_err());
        assert!(port.write(&buf).is_ok());
    }

    #[test]
    fn reconnect_recovers_from_wedged_port() {
        // 4 injected failures = one `send()` call's worth of attempts
        // (initial write + SERIAL_RETRY = 3 retries), so the first
        // send_image fails as if the port were wedged after resume.
        let mut screen = AooScreenBuilder::new().simulate_with_failures(4).unwrap();
        assert!(screen.send_image(&blank_image()).is_err());

        // reconnect() drops the stale handle, opens a fresh (healthy) port,
        // re-runs init and clears the frame cache.
        screen.reconnect().unwrap();
        assert!(screen.send_image(&blank_image()).is_ok());
    }

    #[test]
    fn reconnect_clears_frame_cache() {
        let mut screen = AooScreenBuilder::new().simulate().unwrap();
        screen.send_image(&blank_image()).unwrap();
        assert!(screen.prev_frame.is_some());
        screen.reconnect().unwrap();
        assert!(screen.prev_frame.is_none());
    }

    #[test]
    fn reconnect_with_retry_recovers() {
        let mut screen = AooScreenBuilder::new().simulate_with_failures(4).unwrap();
        assert!(screen.send_image(&blank_image()).is_err());
        // Smoke test of the happy path: the simulated reopen always
        // succeeds, so this must return (not loop forever).
        screen.reconnect_with_retry();
        assert!(screen.send_image(&blank_image()).is_ok());
    }

    #[test]
    fn backoff_delay_grows_then_caps_at_60s() {
        // The retry loop itself is not exercisable with the fake (reopen
        // always succeeds on a simulated port), so the backoff schedule is
        // tested directly: 1s → 2s → ... → 32s → 60s cap.
        assert_eq!(AooScreen::backoff_delay(0), Duration::from_secs(1));
        assert_eq!(AooScreen::backoff_delay(1), Duration::from_secs(2));
        assert_eq!(AooScreen::backoff_delay(2), Duration::from_secs(4));
        assert_eq!(AooScreen::backoff_delay(3), Duration::from_secs(8));
        assert_eq!(AooScreen::backoff_delay(4), Duration::from_secs(16));
        assert_eq!(AooScreen::backoff_delay(5), Duration::from_secs(32));
        assert_eq!(AooScreen::backoff_delay(6), Duration::from_secs(60));
        assert_eq!(AooScreen::backoff_delay(99), Duration::from_secs(60));
    }
}

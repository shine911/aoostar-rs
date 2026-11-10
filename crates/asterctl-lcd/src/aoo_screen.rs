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

/// Image packets are coalesced into writes of at least this many bytes
/// (header + 59-byte packets), instead of one serial write per packet. A
/// full frame drops from ~15k write calls to ~110. 8 KB takes ~55ms on the
/// 1.5 Mbaud UART, far below the 1s port timeout.
const WRITE_BATCH_SIZE: usize = 8 * 1024;

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
        self.simulate_with_port(FakeSerialPort::new())
    }

    /// Like `simulate`, but the fake port fails its first `fail_writes`
    /// writes — for tests of the reconnect logic.
    pub fn simulate_with_failures(self, fail_writes: u32) -> anyhow::Result<AooScreen> {
        self.simulate_with_port(FakeSerialPort::new().with_failures(fail_writes))
    }

    /// Like `simulate`, with a caller-configured fake port (e.g. one that
    /// records the written byte stream). A reconnect opens a plain fake.
    pub fn simulate_with_port(self, port: FakeSerialPort) -> anyhow::Result<AooScreen> {
        Ok(self.build(
            Box::new(port),
            Duration::from_millis(1000),
            Reopen::Simulated,
        ))
    }

    fn build(self, port: Box<dyn SerialPort>, timeout: Duration, reopen: Reopen) -> AooScreen {
        AooScreen {
            port: Some(port),
            enable_cache: self.enable_cache.unwrap_or(true),
            prev_frame: None,
            frame_buf: BytesMut::new(),
            tx_buf: BytesMut::new(),
            generation: 0,
            no_init_check: self.no_init_check.unwrap_or(false),
            timeout,
            reopen,
        }
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
        Ok(self.build(port, timeout, Reopen::Usb { vid, pid }))
    }

    /// Open the specified serial device
    pub fn open_device(self, device: &str) -> anyhow::Result<AooScreen> {
        let timeout = self.timeout.unwrap_or(Duration::from_millis(1000));
        let port = open_serial_port(device, timeout)?;
        Ok(self.build(port, timeout, Reopen::Device(device.to_string())))
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
    /// RGB565 data of the last frame sent successfully (diff base).
    prev_frame: Option<BytesMut>,
    /// Spare RGB565 buffer, swapped with `prev_frame` after each send so the
    /// ~720 KB frame buffers are reused instead of reallocated per frame.
    frame_buf: BytesMut,
    /// Reused serial transmit buffer for coalesced image packets.
    tx_buf: BytesMut,
    /// Bumped whenever the frame cache is invalidated; see [`AooScreen::generation`].
    generation: u64,
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
        let mut frame = std::mem::take(&mut self.frame_buf);
        image.write_rgb565_le(&mut frame);
        let prev = if self.enable_cache {
            self.prev_frame.take()
        } else {
            None
        };
        debug!(
            "Start sending image (size {}) {} cache... ",
            frame.len(),
            if prev.is_some() { "with" } else { "without" }
        );

        let start_time = Instant::now();
        let mut tx = std::mem::take(&mut self.tx_buf);
        let result = self.send_frame_data(&mut tx, &frame, prev.as_deref());
        self.tx_buf = tx;

        match result {
            Ok(sent_chunks) => {
                if self.enable_cache {
                    // double buffering: the old diff base becomes the spare
                    self.prev_frame = Some(frame);
                    self.frame_buf = prev.unwrap_or_default();
                } else {
                    self.frame_buf = frame;
                }
                debug!(
                    "Image sent: {}ms, {sent_chunks} chunks",
                    start_time.elapsed().as_millis()
                );
                Ok(())
            }
            Err(e) => {
                // The display may now show any mix of the old and the new
                // frame: never diff against either, the next send must be a
                // full frame.
                self.frame_buf = frame;
                self.invalidate_cache();
                Err(e)
            }
        }
    }

    /// Writes the start header, all image packets that differ from `prev`
    /// and the end header, coalescing packets into [`WRITE_BATCH_SIZE`]
    /// writes. The byte stream is identical to sending every packet with its
    /// own write. Returns the number of image packets sent.
    fn send_frame_data(
        &mut self,
        tx: &mut BytesMut,
        frame: &[u8],
        prev: Option<&[u8]>,
    ) -> anyhow::Result<usize> {
        tx.clear();
        tx.extend_from_slice(&HEADER_START);

        let mut sent_chunks = 0;
        for (idx, chunk) in frame.chunks(IMG_CHUNK_SIZE).enumerate() {
            let offset = idx * IMG_CHUNK_SIZE;

            if let Some(cache) = prev
                && offset + IMG_CHUNK_SIZE <= cache.len()
                && cache[offset..offset + IMG_CHUNK_SIZE].eq(chunk)
            {
                // Block is unchanged from the previous frame; skip sending
                continue;
            }

            tx.extend_from_slice(&HEADER);
            tx.put_u32_le(offset as u32);
            tx.extend_from_slice(chunk);
            sent_chunks += 1;

            if tx.len() >= WRITE_BATCH_SIZE {
                self.write(tx)
                    .with_context(|| format!("Failed to send image data up to chunk {idx}"))?;
                tx.clear();
            }
        }

        tx.extend_from_slice(&HEADER_END);
        self.write(tx)
            .with_context(|| "Failed to send image data and header end")?;

        // Single flush for the entire frame
        let port = self.port.as_mut().ok_or(anyhow!("LCD port not open"))?;
        port.flush().with_context(|| "Failed to flush image data")?;

        Ok(sent_chunks)
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
        self.invalidate_cache();
    }

    /// Counter that changes whenever the frame cache is invalidated: on
    /// [`AooScreen::reconnect`], [`AooScreen::clear_cache`] and after a failed
    /// `send_image`. A changed value means the display content can no longer
    /// be assumed to match the last frame sent, so callers that skip sending
    /// unchanged frames must send again.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn invalidate_cache(&mut self) {
        if let Some(prev) = self.prev_frame.take()
            && self.frame_buf.capacity() < prev.capacity()
        {
            // keep the larger allocation for the next frame
            self.frame_buf = prev;
        }
        self.generation = self.generation.wrapping_add(1);
    }

    /// Drops the current serial handle and reopens the LCD port, re-running
    /// the initialization handshake. The frame cache is cleared so the next
    /// `send_image` sends a full frame (after a resume the display may be in
    /// an arbitrary state).
    pub fn reconnect(&mut self) -> anyhow::Result<()> {
        warn!("Reconnecting LCD serial port");
        self.port = None;
        self.invalidate_cache();

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
        let port = self.port.as_mut().ok_or(anyhow!("LCD port not open"))?;
        write_resuming(port.as_mut(), data)
    }
}

/// Writes all of `data`, retrying failed write calls up to [`SERIAL_RETRY`]
/// times per buffer.
///
/// Retries resume at the first byte the port has not accepted yet: a write
/// call that returns an error has written nothing (`std::io::Write`
/// contract), and a short write (`Ok(n)`, e.g. a Windows write timeout) is
/// continued from byte `n`. Resending the whole buffer after a partial write
/// — what the former `write_all` retry loop did — would duplicate bytes and
/// corrupt the image stream. `Ok(0)` (nothing accepted) counts as a failure.
/// Once the budget is exhausted the error is returned and the caller
/// reconnects, which forces a full frame.
fn write_resuming(port: &mut dyn SerialPort, data: &[u8]) -> anyhow::Result<()> {
    let mut written = 0;
    let mut retry = 0;
    while written < data.len() {
        let err = match port.write(&data[written..]) {
            Ok(0) => std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "serial port accepted no data",
            ),
            Ok(n) => {
                written += n;
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => e,
        };
        if let Ok(queued) = port.bytes_to_write() {
            debug!("Bytes queued to send: {queued}");
        }
        if retry < SERIAL_RETRY {
            warn!(
                "Failed to write to display ({written}/{} bytes written), retrying! Error: {err}",
                data.len()
            );
            retry += 1;
            continue;
        }
        error!(
            "Failed to write to display ({written}/{} bytes written): {err}",
            data.len()
        );
        return Err(err.into());
    }
    Ok(())
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

    use crate::{WriteRecord, WriteStep};
    use image::Rgb;
    use std::sync::{Arc, Mutex};

    /// Byte stream and write-call count of the original implementation: one
    /// write for the start header, one per changed 59-byte packet, one for
    /// the end header.
    fn reference_stream(frames: &[&RgbImage]) -> (Vec<u8>, usize) {
        let mut out = Vec::new();
        let mut writes = 0;
        let mut prev: Option<BytesMut> = None;
        for img in frames {
            let data = img.to_rgb565_le();
            out.extend_from_slice(&HEADER_START);
            writes += 1;
            for (idx, chunk) in data.chunks(IMG_CHUNK_SIZE).enumerate() {
                let offset = idx * IMG_CHUNK_SIZE;
                if let Some(cache) = &prev
                    && offset + IMG_CHUNK_SIZE <= cache.len()
                    && cache[offset..offset + IMG_CHUNK_SIZE].eq(chunk)
                {
                    continue;
                }
                out.extend_from_slice(&HEADER);
                out.extend_from_slice(&(offset as u32).to_le_bytes());
                out.extend_from_slice(chunk);
                writes += 1;
            }
            out.extend_from_slice(&HEADER_END);
            writes += 1;
            prev = Some(data);
        }
        (out, writes)
    }

    fn pattern_image(seed: u32) -> RgbImage {
        RgbImage::from_fn(DISPLAY_SIZE.0, DISPLAY_SIZE.1, |x, y| {
            Rgb([
                (x ^ seed) as u8,
                (y.wrapping_mul(3) ^ seed) as u8,
                (x.wrapping_add(y) ^ seed) as u8,
            ])
        })
    }

    /// `base` with a few changed regions, including the very first and the
    /// very last pixel.
    fn partially_changed(base: &RgbImage) -> RgbImage {
        let mut img = base.clone();
        let (w, h) = img.dimensions();
        for (x, y) in [(0, 0), (w - 1, h - 1), (w / 2, h / 2)] {
            img.put_pixel(x, y, Rgb([1, 2, 3]));
        }
        for y in 100..140 {
            for x in 300..520 {
                img.put_pixel(x, y, Rgb([255, 0, 128]));
            }
        }
        img
    }

    fn recording_screen(port: FakeSerialPort) -> (AooScreen, Arc<Mutex<WriteRecord>>) {
        let record = Arc::new(Mutex::new(WriteRecord::default()));
        let port = port.without_delay().with_record(record.clone());
        let screen = AooScreenBuilder::new().simulate_with_port(port).unwrap();
        (screen, record)
    }

    fn take_record(record: &Arc<Mutex<WriteRecord>>) -> WriteRecord {
        std::mem::take(&mut *record.lock().unwrap())
    }

    #[test]
    fn batched_stream_matches_per_packet_stream() {
        let full = pattern_image(0);
        let partial = partially_changed(&full);
        let (mut screen, record) = recording_screen(FakeSerialPort::new());

        // full frame (empty cache)
        screen.send_image(&full).unwrap();
        let got = take_record(&record);
        let (expected, ref_writes) = reference_stream(&[&full]);
        assert_eq!(got.bytes.len(), expected.len());
        assert!(got.bytes == expected, "full frame byte stream differs");
        assert_eq!(ref_writes, 15_362);
        assert!(got.writes <= 112, "full frame took {} writes", got.writes);
        println!(
            "full frame: {} bytes, writes {} -> {}",
            got.bytes.len(),
            ref_writes,
            got.writes
        );

        // partial-diff frame
        screen.send_image(&partial).unwrap();
        let got_partial = take_record(&record);
        let (both, both_writes) = reference_stream(&[&full, &partial]);
        assert!(
            got_partial.bytes == both[expected.len()..],
            "partial frame byte stream differs"
        );
        println!(
            "partial frame: {} bytes, writes {} -> {}",
            got_partial.bytes.len(),
            both_writes - ref_writes,
            got_partial.writes
        );

        // unchanged frame: only the start and end headers, in one write
        screen.send_image(&partial).unwrap();
        let got_same = take_record(&record);
        let mut headers = HEADER_START.to_vec();
        headers.extend_from_slice(&HEADER_END);
        assert_eq!(got_same.bytes, headers);
        assert_eq!(got_same.writes, 1);
    }

    #[test]
    fn rgba_frames_use_the_same_stream() {
        let full = pattern_image(7);
        let rgba = image::DynamicImage::ImageRgb8(full.clone()).to_rgba8();
        let (mut screen, record) = recording_screen(FakeSerialPort::new());
        screen.send_image(&rgba).unwrap();
        let (expected, _) = reference_stream(&[&full]);
        assert!(take_record(&record).bytes == expected);
    }

    #[test]
    fn short_writes_and_retries_never_duplicate_bytes() {
        // A short write followed by a failure of the same buffer, an `Ok(0)`
        // and another failure after partial progress: every retry must
        // resume at the first unsent byte. (The former `write_all` +
        // resend-the-whole-buffer loop duplicated the accepted prefix here.)
        let full = pattern_image(3);
        let partial = partially_changed(&full);
        let port = FakeSerialPort::new().with_script([
            WriteStep::Partial(1000),
            WriteStep::Fail,
            WriteStep::Partial(3),
            WriteStep::Partial(0),
            WriteStep::Partial(5000),
            WriteStep::Fail,
        ]);
        let (mut screen, record) = recording_screen(port);
        screen.send_image(&full).unwrap();
        screen.send_image(&partial).unwrap();
        let (expected, _) = reference_stream(&[&full, &partial]);
        let got = take_record(&record);
        assert_eq!(got.bytes.len(), expected.len());
        assert!(got.bytes == expected);
    }

    #[test]
    fn failed_send_invalidates_cache_and_resends_full_frame() {
        let full = pattern_image(5);
        let partial = partially_changed(&full);
        let (mut screen, record) = recording_screen(FakeSerialPort::new());
        screen.send_image(&full).unwrap();
        let generation = screen.generation();
        take_record(&record);

        // A port that wedges mid-frame: some bytes get through, then the
        // initial attempt and all SERIAL_RETRY retries fail.
        let failures = std::iter::repeat_n(WriteStep::Fail, SERIAL_RETRY as usize + 1);
        screen.port = Some(Box::new(
            FakeSerialPort::new()
                .without_delay()
                .with_record(record.clone())
                .with_script(std::iter::once(WriteStep::Partial(500)).chain(failures)),
        ));
        assert!(screen.send_image(&partial).is_err());
        assert!(screen.prev_frame.is_none());
        assert_ne!(screen.generation(), generation);
        take_record(&record);

        // the next send is a full frame of the new image
        screen.send_image(&partial).unwrap();
        let (expected, _) = reference_stream(&[&partial]);
        assert!(take_record(&record).bytes == expected);
    }

    #[test]
    fn generation_changes_on_cache_invalidation() {
        let mut screen = AooScreenBuilder::new()
            .simulate_with_port(FakeSerialPort::new().without_delay())
            .unwrap();
        let g0 = screen.generation();
        screen.send_image(&blank_image()).unwrap();
        assert_eq!(screen.generation(), g0, "a successful send keeps it");
        screen.reconnect().unwrap();
        let g1 = screen.generation();
        assert_ne!(g1, g0);
        screen.clear_cache();
        assert_ne!(screen.generation(), g1);
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

// SPDX-License-Identifier: GPL-3.0-or-later
//! The audio-device plugin of Playground's browser build of OxiGuitar (patches/guitar-pro.yaml in
//! github.com/daijro/openplayground): the service `audio-device` `web-audio`, through cpal's Web Audio host. The page
//! has one thread, so a stream is opened and closed right in the call (the desktop plugins keep a thread for that), and
//! its callback, which hands the core its buffer (`AudioOut::render`), runs on the page's thread too. Browsers start a
//! page's audio only once the person has clicked or typed (index.html resumes it then).
#![cfg(target_arch = "wasm32")]
// export_plugin! asks for the `oxp` feature, which a plugin linked into the page has no use for.
#![allow(unexpected_cfgs)]

use std::cell::RefCell;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use oxiguitar_plugin::api::audio_device::{CLOSE, DEVICES, OPEN, Open, Opened};
use oxiguitar_plugin::api::{decode, encode};
use oxiguitar_plugin::{Availability, Error, ErrorKind, Host, Level, Plugin, Service};

/// The output's name.
const NAME: &str = "Web Audio";

thread_local! {
    /// The open streams, by number (a cpal stream stays on the thread that made it).
    static STREAMS: RefCell<Vec<(u64, cpal::Stream)>> = const { RefCell::new(Vec::new()) };
}

/// The plugin.
#[derive(Debug)]
pub struct Audio {
    host: Host,
}

impl Plugin for Audio {
    fn start(host: Host) -> Result<Audio, Error> {
        Ok(Audio { host })
    }

    fn service(&self, interface: &str, id: &str) -> Option<&dyn Service> {
        (interface == "audio-device" && id == "web-audio").then_some(self as &dyn Service)
    }
}

fn failed(what: &str, e: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::Failed, format!("{what} ({e})"))
}

impl Audio {
    fn open(&self, open: &Open) -> Result<Opened, Error> {
        let out = self.host.audio_out().ok_or_else(|| Error::new(ErrorKind::Unsupported, "this OxiGuitar can't play sound"))?;
        let device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "the browser has no audio output"))?;
        let config = cpal::StreamConfig { channels: 2, sample_rate: open.rate, buffer_size: cpal::BufferSize::Default };
        let stream_no = open.stream;
        let errors = self.host.clone();
        let stream = device
            .build_output_stream(
                config,
                move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                    let ts = info.timestamp();
                    let delay = u64::try_from(ts.playback.duration_since(ts.callback).as_nanos()).unwrap_or(0);
                    out.render(stream_no, data, 2, delay);
                },
                move |e: cpal::Error| errors.notify(Level::Error, format!("{NAME}: the sound stopped ({e}).")),
                None,
            )
            .map_err(|e| failed("the output couldn't be opened", e))?;
        stream.play().map_err(|e| failed("the output couldn't be started", e))?;
        STREAMS.with_borrow_mut(|s| s.push((stream_no, stream)));
        Ok(Opened { rate: open.rate, channels: 2, frames: 0, device: NAME.to_owned() })
    }
}

impl Service for Audio {
    fn probe(&self) -> Availability {
        match cpal::default_host().default_output_device() {
            Some(_) => Availability::Available,
            None => Availability::Unavailable("the browser has no Web Audio".into()),
        }
    }

    fn call(&self, method: &str, input: &[u8]) -> Result<Vec<u8>, Error> {
        let invalid = |e: oxiguitar_plugin::api::DecodeError| Error::new(ErrorKind::Invalid, format!("the input can't be read ({e})"));
        let unencoded = |e: oxiguitar_plugin::api::DecodeError| Error::new(ErrorKind::Failed, e.to_string());
        match method {
            OPEN => encode(&self.open(&decode::<Open>(input).map_err(invalid)?)?).map_err(unencoded),
            CLOSE => {
                let stream: u64 = decode(input).map_err(invalid)?;
                // Dropping a cpal stream stops it: its callback isn't called again once this returns.
                let closed: Vec<_> = STREAMS.with_borrow_mut(|s| s.extract_if(.., |(n, _)| *n == stream).collect());
                drop(closed);
                Ok(Vec::new())
            }
            DEVICES => encode(&vec![NAME.to_owned()]).map_err(unencoded),
            _ => Err(Error::not_found(format!("no method {method:?}"))),
        }
    }
}

oxiguitar_plugin::export_plugin!(Audio);

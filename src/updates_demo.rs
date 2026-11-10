use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    thread,
    time::Duration,
};

use freshen::{Cancellation, Error, Event, ReleaseSource, Result, Transport};
use url::Url;

pub const PRODUCT: &str = "directcut-rs-demo";
pub const VERSION: &str = match option_env!("DIRECTCUT_DEMO_VERSION") {
    Some(version) => version,
    None => "0.0.0",
};
pub const PUBLISHER_KEY: &str = match option_env!("DIRECTCUT_DEMO_PUBLIC_KEY") {
    Some(key) => key,
    None => "",
};
pub const CONFIRM_DELAY_MS: u32 = 4_000;

pub struct UpdateTransport(PathBuf);

pub fn transport() -> Result<UpdateTransport> {
    Ok(UpdateTransport(
        super::installation_root()?.join("demo-feed"),
    ))
}

pub fn source() -> ReleaseSource {
    ReleaseSource::Manifest {
        document: "https://demo.invalid/freshen-manifest.json"
            .parse()
            .unwrap(),
        signature: "https://demo.invalid/freshen-manifest.json.sig"
            .parse()
            .unwrap(),
    }
}

impl Transport for UpdateTransport {
    fn download(
        &self,
        url: &Url,
        output: &mut dyn Write,
        limit: u64,
        cancel: &Cancellation,
        events: &mut dyn FnMut(Event),
    ) -> Result<u64> {
        let name = match (url.host_str(), url.path()) {
            (Some("demo.invalid"), "/freshen-manifest.json") => "freshen-manifest.json",
            (Some("demo.invalid"), "/freshen-manifest.json.sig") => "freshen-manifest.json.sig",
            (Some("demo.invalid"), "/demo-update.zip") => "demo-update.zip",
            _ => return Err(Error::Invalid("Unknown update asset".into())),
        };
        cancel.check()?;
        let mut file = File::open(self.0.join(name))?;
        let total = file.metadata()?.len();
        if total > limit {
            return Err(Error::SizeLimit);
        }
        let mut received = 0;
        let mut buffer = [0; 32 * 1024];
        loop {
            cancel.check()?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                return Ok(received);
            }
            received += count as u64;
            if received > limit {
                return Err(Error::SizeLimit);
            }
            output.write_all(&buffer[..count])?;
            events(Event::Downloading {
                received,
                total: Some(total),
            });
            if name == "demo-update.zip" {
                // Make the actual byte progress easy to follow during a rehearsal.
                thread::sleep(Duration::from_millis(85));
            }
        }
    }
}

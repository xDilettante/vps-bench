use crate::bench::command;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::OsStr;
use std::path::Path;

pub struct Volume {
    pub source: String,
    pub filesystem: String,
    pub device: Option<BlockDevice>,
}

impl Volume {
    pub fn read(directory: &Path) -> Option<Self> {
        let output = command(
            "findmnt",
            [
                OsStr::new("--json"),
                OsStr::new("--first-only"),
                OsStr::new("--output=SOURCE,FSTYPE,MAJ:MIN"),
                OsStr::new("--target"),
                directory.as_os_str(),
            ],
            3,
            None,
        )
        .ok()?;
        let report: Value = serde_json::from_str(&output).ok()?;
        let mount = report.get("filesystems")?.as_array()?.first()?;
        let source = mount.get("source")?.as_str()?.to_owned();
        let filesystem = mount.get("fstype")?.as_str()?.to_owned();
        let device = mount["maj:min"].as_str().and_then(|number| {
            if number.starts_with("0:") {
                return None;
            }
            let output = command(
                "lsblk",
                [
                    "--json",
                    "--bytes",
                    "--list",
                    "--output=NAME,TYPE,SIZE,ROTA,TRAN,MODEL,MAJ:MIN",
                ],
                3,
                None,
            )
            .ok()?;
            BlockDevice::find(&serde_json::from_str(&output).ok()?, number)
        });
        Some(Self {
            source,
            filesystem,
            device,
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockDevice {
    pub name: String,
    pub kind: Option<String>,
    pub size_bytes: Option<u64>,
    pub rotational: Option<bool>,
    pub transport: Option<String>,
    pub model: Option<String>,
}

impl BlockDevice {
    pub fn find(report: &Value, number: &str) -> Option<Self> {
        let device = report["blockdevices"]
            .as_array()?
            .iter()
            .find(|entry| entry["maj:min"].as_str() == Some(number))?;
        let text = |key| {
            device[key]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let numeric = |key| {
            device[key]
                .as_u64()
                .or_else(|| device[key].as_str()?.parse().ok())
        };
        Some(Self {
            name: text("name")?,
            kind: text("type"),
            size_bytes: numeric("size"),
            rotational: device["rota"].as_bool().or_else(|| match numeric("rota") {
                Some(0) => Some(false),
                Some(1) => Some(true),
                _ => None,
            }),
            transport: text("tran"),
            model: text("model"),
        })
    }
}

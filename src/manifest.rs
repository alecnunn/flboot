use indexmap::IndexMap;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
pub struct BinaryEntry {
    pub url: String,
    pub sha1: Option<String>,
    pub dest: String,
}

#[derive(Deserialize)]
#[allow(dead_code)] // off-target-platform entry is deserialized but read only via cfg-gated current()
pub struct BinaryTool {
    pub path_names: Vec<String>,
    pub windows: BinaryEntry,
    pub linux: BinaryEntry,
}

impl BinaryTool {
    #[cfg(windows)]
    pub fn current(&self) -> &BinaryEntry {
        &self.windows
    }
    #[cfg(unix)]
    pub fn current(&self) -> &BinaryEntry {
        &self.linux
    }
}

#[derive(Deserialize)]
pub struct ZipEntry {
    pub url: String,
    pub sha1: Option<String>,
    pub archive: String,
    pub entry: String,
    pub dest: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct ZipTool {
    pub path_names: Vec<String>,
    pub windows: ZipEntry,
    pub linux: ZipEntry,
}

impl ZipTool {
    #[cfg(windows)]
    pub fn current(&self) -> &ZipEntry {
        &self.windows
    }
    #[cfg(unix)]
    pub fn current(&self) -> &ZipEntry {
        &self.linux
    }
}

#[derive(Deserialize)]
pub struct Msvc6 {
    pub url: String,
    pub sha1: Option<String>,
    pub dest_dir: String,
    pub sentinel: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct SevenZipWindows {
    pub msi_url: String,
    pub msi_dest: String,
    pub extract_dir: String,
    pub exe_subpath: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct SevenZipLinux {
    pub url: String,
    pub sha1: Option<String>,
    pub archive: String,
    pub extract_dir: String,
    pub dest: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct SevenZip {
    pub path_names: Vec<String>,
    pub windows: SevenZipWindows,
    pub linux: SevenZipLinux,
}

impl SevenZip {
    #[cfg(windows)]
    pub fn dest(&self) -> PathBuf {
        Path::new(&self.windows.extract_dir).join(&self.windows.exe_subpath)
    }
    #[cfg(unix)]
    pub fn dest(&self) -> PathBuf {
        PathBuf::from(&self.linux.dest)
    }
}

#[derive(Deserialize)]
pub struct ToolsManifest {
    pub delink: BinaryTool,
    pub objdiff_cli: BinaryTool,
    pub objdiff: BinaryTool,
    pub ninja: ZipTool,
    pub msvc6: Msvc6,
    pub seven_zip: SevenZip,
}

#[derive(Deserialize)]
pub struct OrigManifest {
    pub archive_url: String,
    pub binaries: IndexMap<String, String>,
}

pub fn load_tools_manifest() -> anyhow::Result<ToolsManifest> {
    crate::model::load_jsonc(Path::new("config/tools.json"))
}

pub fn load_orig_manifest(config_id: &str) -> anyhow::Result<OrigManifest> {
    crate::model::load_jsonc(&PathBuf::from(format!("config/{config_id}/orig.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools_fixture() -> &'static str {
        r#"{
            "delink": {
                "path_names": ["delink"],
                "windows": { "url": "https://x/delink.exe", "sha1": "AA", "dest": "build/tools/delink-windows-x86_64.exe" },
                "linux":   { "url": "https://x/delink",     "sha1": "bb", "dest": "build/tools/delink-linux-x86_64" }
            },
            "objdiff_cli": {
                "path_names": ["objdiff-cli"],
                "windows": { "url": "https://x/oc.exe", "sha1": "CC", "dest": "build/tools/objdiff-cli-windows-x86_64.exe" },
                "linux":   { "url": "https://x/oc",     "sha1": "dd", "dest": "build/tools/objdiff-cli-linux-x86_64" }
            },
            "objdiff": {
                "path_names": ["objdiff"],
                "windows": { "url": "https://x/o.exe", "sha1": "EE", "dest": "build/tools/objdiff-windows-x86_64.exe" },
                "linux":   { "url": "https://x/o",     "sha1": "ff", "dest": "build/tools/objdiff-linux-x86_64" }
            },
            "ninja": {
                "path_names": ["ninja"],
                "windows": { "url": "https://x/ninja-win.zip",   "sha1": "GG", "archive": "build/tools/ninja-win.zip",   "entry": "ninja.exe", "dest": "build/tools/ninja.exe" },
                "linux":   { "url": "https://x/ninja-linux.zip", "sha1": "hh", "archive": "build/tools/ninja-linux.zip", "entry": "ninja",     "dest": "build/tools/ninja" }
            },
            "msvc6": { "url": "https://x/msvc6.tar.gz", "dest_dir": "build/msvc6.0", "sentinel": "Bin/CL.EXE" },
            "seven_zip": {
                "path_names": ["7zz", "7z"],
                "windows": { "msi_url": "https://x/7z.msi", "msi_dest": "build/tools/7z.msi", "extract_dir": "build/tools/7z", "exe_subpath": "Files/7-Zip/7z.exe" },
                "linux":   { "url": "https://x/7z.tar.xz", "sha1": "ii", "archive": "build/tools/7z.tar.xz", "extract_dir": "build/tools/7z", "dest": "build/tools/7z/7zzs" }
            }
        }"#
    }

    #[test]
    fn tools_manifest_round_trips() {
        let m: ToolsManifest = serde_json::from_str(tools_fixture()).unwrap();
        assert_eq!(m.delink.path_names, vec!["delink"]);
        assert_eq!(m.seven_zip.path_names, vec!["7zz", "7z"]);
        assert_eq!(m.msvc6.sentinel, "Bin/CL.EXE");
        assert_eq!(m.ninja.windows.entry, "ninja.exe");
        assert_eq!(m.ninja.linux.entry, "ninja");
    }

    #[test]
    fn binary_tool_current_returns_target_platform_entry() {
        let m: ToolsManifest = serde_json::from_str(tools_fixture()).unwrap();
        #[cfg(windows)]
        assert_eq!(m.delink.current().dest, "build/tools/delink-windows-x86_64.exe");
        #[cfg(unix)]
        assert_eq!(m.delink.current().dest, "build/tools/delink-linux-x86_64");
    }

    #[test]
    fn seven_zip_dest_is_platform_specific() {
        let m: ToolsManifest = serde_json::from_str(tools_fixture()).unwrap();
        #[cfg(windows)]
        assert_eq!(m.seven_zip.dest(), PathBuf::from("build/tools/7z").join("Files/7-Zip/7z.exe"));
        #[cfg(unix)]
        assert_eq!(m.seven_zip.dest(), PathBuf::from("build/tools/7z/7zzs"));
    }

    #[test]
    fn orig_manifest_preserves_order_and_reads_hashes() {
        let json = r#"{
            "archive_url": "https://x/orig.7z",
            "binaries": { "Common.dll": "AAA", "Content.dll": "BBB", "zlib.dll": "CCC" }
        }"#;
        let m: OrigManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.archive_url, "https://x/orig.7z");
        let keys: Vec<&String> = m.binaries.keys().collect();
        assert_eq!(keys, vec!["Common.dll", "Content.dll", "zlib.dll"]);
        assert_eq!(m.binaries["zlib.dll"], "CCC");
    }
}

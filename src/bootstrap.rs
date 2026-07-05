use crate::fetch;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
pub enum ToolMiss {
    Download,
    RequireBootstrapped,
}

pub fn which_lookup(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

pub fn resolve_tool(
    candidates: &[&str],
    lookup: impl Fn(&str) -> Option<PathBuf>,
    fixed_path: &Path,
    on_miss: ToolMiss,
    download: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<PathBuf> {
    for name in candidates {
        if let Some(path) = lookup(name) {
            crate::log::info(&format!("using {name} from PATH: {}", path.display()));
            return Ok(path);
        }
    }
    match on_miss {
        ToolMiss::Download => {
            download(fixed_path)?;
            Ok(fixed_path.to_path_buf())
        }
        ToolMiss::RequireBootstrapped => {
            if fixed_path.exists() {
                Ok(fixed_path.to_path_buf())
            } else {
                anyhow::bail!(
                    "{} not found on PATH or at {}; run `flboot bootstrap` first",
                    candidates[0],
                    fixed_path.display()
                )
            }
        }
    }
}

#[cfg(test)]
mod resolve_tool_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn finds_candidate_on_path_without_downloading() {
        let downloaded = Cell::new(false);
        let result = resolve_tool(
            &["ninja"],
            |name| if name == "ninja" { Some(PathBuf::from("/usr/bin/ninja")) } else { None },
            Path::new("build/tools/ninja"),
            ToolMiss::Download,
            |_| {
                downloaded.set(true);
                Ok(())
            },
        );
        assert_eq!(result.unwrap(), PathBuf::from("/usr/bin/ninja"));
        assert!(!downloaded.get());
    }

    #[test]
    fn downloads_when_path_lookup_misses_and_mode_is_download() {
        let downloaded = Cell::new(false);
        let fixed = Path::new("build/tools/ninja");
        let result = resolve_tool(
            &["ninja"],
            |_| None,
            fixed,
            ToolMiss::Download,
            |path| {
                downloaded.set(true);
                assert_eq!(path, fixed);
                Ok(())
            },
        );
        assert_eq!(result.unwrap(), fixed.to_path_buf());
        assert!(downloaded.get());
    }

    #[test]
    fn errors_when_require_bootstrapped_and_fixed_path_absent() {
        let result = resolve_tool(
            &["ninja"],
            |_| None,
            Path::new("does/not/exist/ninja"),
            ToolMiss::RequireBootstrapped,
            |_| unreachable!("download must not be called in RequireBootstrapped mode"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn returns_fixed_path_when_require_bootstrapped_and_it_exists() {
        let dir = std::env::temp_dir().join(format!("flboot-resolve-tool-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fixed = dir.join("ninja");
        std::fs::write(&fixed, b"fake").unwrap();

        let result = resolve_tool(
            &["ninja"],
            |_| None,
            &fixed,
            ToolMiss::RequireBootstrapped,
            |_| unreachable!("download must not be called in RequireBootstrapped mode"),
        );

        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(result.unwrap(), fixed);
    }
}

use crate::manifest::{BinaryEntry, BinaryTool, SevenZip, ToolsManifest, ZipEntry, ZipTool};

/// Downloads a single-binary tool (delink/objdiff-cli/objdiff) and marks it
/// executable (no-op on Windows). Platform difference lives entirely in the
/// manifest entry, so this is not cfg-split.
fn download_binary(entry: &BinaryEntry, dest: &Path) -> anyhow::Result<()> {
    fetch::download_file(&entry.url, dest, entry.sha1.as_deref())?;
    fetch::set_executable(dest)
}

fn resolve_binary(
    tool: &BinaryTool,
    override_path: Option<&str>,
    label: &str,
    on_miss: ToolMiss,
) -> anyhow::Result<PathBuf> {
    if let Some(p) = override_path {
        crate::log::info(&format!("using overridden {label}: {p}"));
        return Ok(PathBuf::from(p));
    }
    let entry = tool.current();
    let candidates: Vec<&str> = tool.path_names.iter().map(String::as_str).collect();
    resolve_tool(&candidates, which_lookup, Path::new(&entry.dest), on_miss, |dest| {
        download_binary(entry, dest)
    })
}

pub fn resolve_delink(
    manifest: &ToolsManifest,
    override_path: Option<&str>,
    on_miss: ToolMiss,
) -> anyhow::Result<PathBuf> {
    resolve_binary(&manifest.delink, override_path, "delink", on_miss)
}

pub fn resolve_objdiff_cli(
    manifest: &ToolsManifest,
    override_path: Option<&str>,
    on_miss: ToolMiss,
) -> anyhow::Result<PathBuf> {
    resolve_binary(&manifest.objdiff_cli, override_path, "objdiff-cli", on_miss)
}

pub fn resolve_objdiff(manifest: &ToolsManifest, on_miss: ToolMiss) -> anyhow::Result<PathBuf> {
    resolve_binary(&manifest.objdiff, None, "objdiff", on_miss)
}

/// Downloads a zip-packaged tool (ninja), extracts one entry, and marks it
/// executable (no-op on Windows). Not cfg-split -- platform data is in the manifest.
fn download_zip_tool(entry: &ZipEntry, dest: &Path) -> anyhow::Result<()> {
    fetch::download_file(&entry.url, Path::new(&entry.archive), entry.sha1.as_deref())?;
    let dest_dir = dest.parent().expect("tool download path always has a parent");
    crate::log::info(&format!("extracting {} -> {}", entry.archive, dest_dir.display()));
    fetch::extract_zip_entry(Path::new(&entry.archive), &entry.entry, dest_dir)?;
    fetch::set_executable(dest)
}

pub fn resolve_ninja(manifest: &ToolsManifest, on_miss: ToolMiss) -> anyhow::Result<PathBuf> {
    let tool: &ZipTool = &manifest.ninja;
    let entry = tool.current();
    let candidates: Vec<&str> = tool.path_names.iter().map(String::as_str).collect();
    resolve_tool(&candidates, which_lookup, Path::new(&entry.dest), on_miss, |dest| {
        download_zip_tool(entry, dest)
    })
}

pub fn ensure_tools(
    manifest: &ToolsManifest,
    delink_override: Option<&str>,
    objdiff_cli_override: Option<&str>,
) -> anyhow::Result<()> {
    resolve_delink(manifest, delink_override, ToolMiss::Download)?;
    resolve_objdiff_cli(manifest, objdiff_cli_override, ToolMiss::Download)?;
    resolve_objdiff(manifest, ToolMiss::Download)?;
    resolve_ninja(manifest, ToolMiss::Download)?;

    let msvc6 = &manifest.msvc6;
    let msvc6_dir = Path::new(&msvc6.dest_dir);
    if msvc6_dir.join(&msvc6.sentinel).exists() {
        crate::log::info(&format!(
            "{} already present, skipping MSVC 6.0 download and extraction",
            msvc6_dir.display()
        ));
    } else {
        let msvc6_archive = PathBuf::from("build/tools/msvc6.0.tar.gz");
        fetch::download_file(&msvc6.url, &msvc6_archive, msvc6.sha1.as_deref())?;
        crate::log::info(&format!("extracting {} -> {}", msvc6_archive.display(), msvc6_dir.display()));
        fetch::extract_tar_gz(&msvc6_archive, msvc6_dir)?;
    }

    Ok(())
}

#[cfg(windows)]
fn download_seven_zip(sz: &SevenZip, _dest: &Path) -> anyhow::Result<()> {
    let w = &sz.windows;
    let msi_path = PathBuf::from(&w.msi_dest);
    fetch::download_file(&w.msi_url, &msi_path, None)?;

    let extract_dir = Path::new(&w.extract_dir);
    let extract_dir_abs = std::env::current_dir()?.join(extract_dir);
    crate::log::info(&format!("extracting {} -> {}", msi_path.display(), extract_dir_abs.display()));
    std::fs::create_dir_all(extract_dir)?;
    let msi_abs = std::fs::canonicalize(&msi_path)?;
    let status = std::process::Command::new("msiexec")
        .arg("/a")
        .arg(&msi_abs)
        .arg("/qn")
        .arg(format!("TARGETDIR={}", extract_dir_abs.display()))
        .status()
        .map_err(|e| anyhow::anyhow!("running msiexec: {e}"))?;
    if !status.success() {
        anyhow::bail!("msiexec extraction failed (exit code {:?})", status.code());
    }
    Ok(())
}

#[cfg(unix)]
fn download_seven_zip(sz: &SevenZip, dest: &Path) -> anyhow::Result<()> {
    let l = &sz.linux;
    let archive = PathBuf::from(&l.archive);
    fetch::download_file(&l.url, &archive, l.sha1.as_deref())?;
    let extract_dir = Path::new(&l.extract_dir);
    crate::log::info(&format!("extracting {} -> {}", archive.display(), extract_dir.display()));
    fetch::extract_tar_xz(&archive, extract_dir)?;
    fetch::set_executable(dest)
}

fn resolve_seven_zip(manifest: &ToolsManifest, on_miss: ToolMiss) -> anyhow::Result<PathBuf> {
    let sz = &manifest.seven_zip;
    let candidates: Vec<&str> = sz.path_names.iter().map(String::as_str).collect();
    let dest = sz.dest();
    resolve_tool(&candidates, which_lookup, &dest, on_miss, |d| download_seven_zip(sz, d))
}

pub fn ensure_orig(config_id: &str, tools: &ToolsManifest, orig: &crate::manifest::OrigManifest) -> anyhow::Result<()> {
    let orig_dir = PathBuf::from(format!("orig/{config_id}"));
    if orig_dir.is_dir() && std::fs::read_dir(&orig_dir)?.next().is_some() {
        crate::log::info(&format!("{} already present, skipping orig download", orig_dir.display()));
        return Ok(());
    }

    let orig_archive = PathBuf::from(format!("build/tools/{config_id}.7z"));
    fetch::download_file(&orig.archive_url, &orig_archive, None)?;

    let seven_zip = resolve_seven_zip(tools, ToolMiss::Download)?;
    std::fs::create_dir_all(&orig_dir)?;
    crate::log::info(&format!("extracting {} -> {}", orig_archive.display(), orig_dir.display()));
    let status = std::process::Command::new(&seven_zip)
        .arg("x")
        .arg(&orig_archive)
        .arg(format!("-o{}", orig_dir.display()))
        .arg("-y")
        .status()
        .map_err(|e| anyhow::anyhow!("running 7z: {e}"))?;
    if !status.success() {
        anyhow::bail!("7z extraction failed (exit code {:?})", status.code());
    }
    Ok(())
}

pub fn verify_orig(config_id: &str, orig: &crate::manifest::OrigManifest) -> anyhow::Result<()> {
    for (name, hash) in &orig.binaries {
        let path = Path::new("orig").join(config_id).join(name);
        fetch::verify_hash(true, &path, hash)?;
    }
    Ok(())
}

use crate::model::Objects;

pub fn delink_one(objects: &Objects, config_id: &str, unit: &str, delink_exe: &Path) -> anyhow::Result<()> {
    let lib = objects
        .get(unit)
        .ok_or_else(|| anyhow::anyhow!("unknown unit: {unit}"))?;
    let binary = lib.binary.clone().unwrap_or_else(|| format!("orig/{config_id}/{unit}"));
    let delink_name = lib.delink.clone().unwrap_or_else(|| unit.to_string());
    let json_in = format!("config/{config_id}/delink/{unit}.delink.json");
    let outdir = format!("build/{config_id}/delink/{delink_name}");
    std::fs::create_dir_all(&outdir)?;

    let binary_abs = std::fs::canonicalize(&binary)
        .map_err(|e| anyhow::anyhow!("resolving binary path {binary}: {e}"))?;

    let status = std::process::Command::new(delink_exe)
        .arg("ida-split")
        .arg(&json_in)
        .arg(&binary_abs)
        .arg("--idapro")
        .arg(&lib.idapro)
        .arg("-o")
        .arg(&outdir)
        .status()
        .map_err(|e| anyhow::anyhow!("running delink for {unit}: {e}"))?;
    if !status.success() {
        anyhow::bail!("delink ida-split failed for {unit} (exit code {:?})", status.code());
    }
    Ok(())
}

/// Best-effort split: a failed library is logged and recorded, but does not
/// abort the run -- matches bootstrap.py's split_all, which still generates
/// build.ninja/objdiff.json/compile_commands.json for whatever succeeded.
pub fn split_all(objects: &Objects, config_id: &str, delink_exe: &Path) -> Vec<(String, String)> {
    let mut failures = Vec::new();

    for (unit, lib) in objects {
        let json_in = PathBuf::from(format!("config/{config_id}/delink/{unit}.delink.json"));
        let idapro = PathBuf::from(&lib.idapro);
        let binary = PathBuf::from(lib.binary.clone().unwrap_or_else(|| format!("orig/{config_id}/{unit}")));

        if !json_in.exists() {
            let msg = format!("missing export {}", json_in.display());
            crate::log::warn(&format!("[{unit}] skip - {msg}"));
            failures.push((unit.clone(), msg));
            continue;
        }
        if !idapro.exists() {
            let msg = format!("missing idapro {}", idapro.display());
            crate::log::warn(&format!("[{unit}] skip - {msg}"));
            failures.push((unit.clone(), msg));
            continue;
        }
        if !binary.exists() {
            let msg = format!("missing binary {}", binary.display());
            crate::log::warn(&format!("[{unit}] skip - {msg}"));
            failures.push((unit.clone(), msg));
            continue;
        }

        if let Err(e) = delink_one(objects, config_id, unit, delink_exe) {
            crate::log::error(&format!("[{unit}] failed: {e}"));
            failures.push((unit.clone(), e.to_string()));
        }
    }

    let ok = objects.len() - failures.len();
    crate::log::info(&format!("split phase: {ok}/{} libraries ok", objects.len()));
    if !failures.is_empty() {
        crate::log::error(&format!("{} failures:", failures.len()));
        for (name, why) in &failures {
            crate::log::error(&format!("  {name}: {why}"));
        }
    }
    failures
}

/// The stub written for a missing module entrypoint: a comment naming the PE
/// module (the objects.json library key) and a commented example of the unity
/// `#include` pattern. All-comment, so a fresh stub is an empty translation
/// unit that compiles to an empty obj.
fn stub_content(lib_name: &str) -> String {
    format!(
        "// {lib_name}\n\
         //\n\
         // Unity build entrypoint. Include this module's source files here, e.g.:\n\
         // #include \"quaternion.cpp\"\n"
    )
}

/// For every `src` path in every library's `objects` map, create the file under
/// `root` (with parent dirs) containing `stub_content(lib_name)` if it does not
/// already exist. Never overwrites. Returns the created `src` paths.
fn scaffold_sources(root: &Path, objects: &Objects) -> anyhow::Result<Vec<String>> {
    let mut created = Vec::new();
    for (lib_name, lib) in objects {
        for src in lib.objects.keys() {
            let path = root.join(src);
            if !path.exists() {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| anyhow::anyhow!("creating source dir {}: {e}", parent.display()))?;
                }
                std::fs::write(&path, stub_content(lib_name))
                    .map_err(|e| anyhow::anyhow!("writing stub {}: {e}", path.display()))?;
                created.push(src.clone());
            }
        }
    }
    Ok(created)
}

pub fn run_bootstrap(
    config_id: &str,
    config_override: Option<&str>,
    delink_override: Option<&str>,
    objdiff_cli_override: Option<&str>,
    skip_delink: bool,
    only: &[String],
) -> anyhow::Result<()> {
    let config: crate::model::Config = crate::model::load_jsonc(&crate::model::config_path(config_id, config_override))?;
    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;
    let objects = crate::model::filter_only(objects, only)?;

    for created in scaffold_sources(Path::new("."), &objects)? {
        crate::log::info(&format!("scaffolded stub -> {created}"));
    }

    let tools = crate::manifest::load_tools_manifest()?;
    let orig = crate::manifest::load_orig_manifest(config_id)?;

    ensure_tools(&tools, delink_override, objdiff_cli_override)?;
    ensure_orig(config_id, &tools, &orig)?;
    verify_orig(config_id, &orig)?;

    if !skip_delink {
        let delink_exe = resolve_delink(&tools, delink_override, ToolMiss::RequireBootstrapped)?;
        split_all(&objects, config_id, &delink_exe);
    }

    std::fs::write("build.ninja", crate::codegen::write_ninja(config_id, &config, &objects)?)?;
    crate::log::info("generated -> build.ninja");

    let objdiff_json = crate::codegen::write_objdiff(config_id, &config, &objects);
    std::fs::write("objdiff.json", serde_json::to_string_pretty(&objdiff_json)?)?;
    crate::log::info("generated -> objdiff.json");

    let compile_commands = crate::codegen::write_compile_commands(&config, &objects)?;
    std::fs::write("compile_commands.json", serde_json::to_string_pretty(&compile_commands)?)?;
    crate::log::info("generated -> compile_commands.json");

    Ok(())
}

#[cfg(test)]
mod scaffold_tests {
    use super::*;

    fn one_lib_objects(src: &str) -> Objects {
        let json = format!(
            r#"{{ "x86math.dll": {{ "progress_category": "x", "cflags": "x", "idapro": "x", "objects": {{ "{src}": "x86math.dll.obj" }} }} }}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir()
            .join(format!("flboot-scaffold-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn stub_names_module_and_shows_commented_include() {
        let stub = stub_content("x86math.dll");
        assert!(stub.contains("x86math.dll"), "stub: {stub:?}");
        assert!(stub.contains("// #include \""), "stub: {stub:?}");
        for line in stub.lines().filter(|l| !l.trim().is_empty()) {
            assert!(line.trim_start().starts_with("//"), "non-comment line: {line:?}");
        }
    }

    #[test]
    fn creates_missing_entrypoint_with_stub_content() {
        let root = temp_root();
        let objects = one_lib_objects("src/x86math.dll.cpp");
        let created = scaffold_sources(&root, &objects).unwrap();
        let written = std::fs::read_to_string(root.join("src/x86math.dll.cpp")).unwrap();
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(created, vec!["src/x86math.dll.cpp".to_string()]);
        assert_eq!(written, stub_content("x86math.dll"));
    }

    #[test]
    fn never_overwrites_existing_source() {
        let root = temp_root();
        let objects = one_lib_objects("src/x86math.dll.cpp");
        let path = root.join("src/x86math.dll.cpp");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "REAL DECOMP CODE").unwrap();
        let created = scaffold_sources(&root, &objects).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&root).ok();
        assert!(created.is_empty(), "should not report an untouched file: {created:?}");
        assert_eq!(after, "REAL DECOMP CODE");
    }

    #[test]
    fn creates_intervening_subdirectories() {
        let root = temp_root();
        let objects = one_lib_objects("src/x86math.dll/x86math.dll.cpp");
        let created = scaffold_sources(&root, &objects).unwrap();
        let exists = root.join("src/x86math.dll/x86math.dll.cpp").is_file();
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(created, vec!["src/x86math.dll/x86math.dll.cpp".to_string()]);
        assert!(exists);
    }
}

use crate::model::{Config, FlagSet, Objects};
use indexmap::IndexMap;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

pub const SOURCE_ROOT: &str = ".";

pub fn substitute_flag(flag: &str, compiler_root: &str, sdk_root: &str) -> String {
    flag.replace("$(SOURCE_ROOT)", SOURCE_ROOT)
        .replace("$(COMPILER_ROOT)", compiler_root)
        .replace("$(SDK_ROOT)", sdk_root)
}

pub fn flatten_cflags(
    name: &str,
    cflags: &IndexMap<String, FlagSet>,
    compiler_root: &str,
    sdk_root: &str,
) -> anyhow::Result<Vec<String>> {
    fn recurse(
        name: &str,
        cflags: &IndexMap<String, FlagSet>,
        compiler_root: &str,
        sdk_root: &str,
        out: &mut Vec<String>,
    ) -> anyhow::Result<()> {
        let entry = cflags
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("unknown cflags key: {name}"))?;
        if let Some(base) = &entry.base {
            recurse(base, cflags, compiler_root, sdk_root, out)?;
        }
        out.extend(entry.flags.iter().map(|f| substitute_flag(f, compiler_root, sdk_root)));
        Ok(())
    }
    let mut result = Vec::new();
    recurse(name, cflags, compiler_root, sdk_root, &mut result)?;
    Ok(result)
}

pub fn get_flags(
    flags_key: &str,
    flags_dict: &IndexMap<String, FlagSet>,
    compiler_root: &str,
    sdk_root: &str,
) -> anyhow::Result<Vec<String>> {
    if !flags_dict.contains_key(flags_key) {
        return Ok(Vec::new());
    }
    flatten_cflags(flags_key, flags_dict, compiler_root, sdk_root)
}

pub fn to_forward_path(p: &str) -> String {
    p.replace('\\', "/")
}

pub fn get_delink_path(config_id: &str, lib_name: &str, target: &str) -> String {
    format!("build/{config_id}/delink/{lib_name}/{}", to_forward_path(target))
}

pub fn strip_last_extension(s: &str) -> &str {
    match s.rfind('.') {
        Some(idx) => &s[..idx],
        None => s,
    }
}

pub fn get_target_path(config_id: &str, lib_name: &str, src: &str) -> String {
    let base = format!("build/{config_id}/obj/{lib_name}/{}", to_forward_path(src));
    format!("{}.obj", strip_last_extension(&base))
}

pub fn strip_arg_quotes(flag: &str) -> String {
    let bytes = flag.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        flag[1..flag.len() - 1].to_string()
    } else {
        flag.to_string()
    }
}

pub fn is_c_source(src: &str) -> bool {
    Path::new(src)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase() == "c")
        .unwrap_or(false)
}

/// Absolute-izes `path` without requiring it to exist (Rust's
/// `fs::canonicalize` errors on missing paths; Python's `Path.resolve()`
/// does not). Joins onto the current directory and lexically collapses `.`
/// and `..` components.
pub fn lexical_absolute(path: &Path) -> std::io::Result<PathBuf> {
    let mut absolute = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir()?
    };
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                absolute.pop();
            }
            component => absolute.push(component.as_os_str()),
        }
    }
    Ok(absolute)
}

#[derive(serde::Serialize)]
pub struct ObjdiffUnitMetadata {
    pub complete: bool,
    pub reverse_fn_order: bool,
    pub source_path: String,
    pub progress_categories: Vec<String>,
    pub auto_generated: bool,
}

#[derive(serde::Serialize)]
pub struct ObjdiffUnit {
    pub name: String,
    pub target_path: String,
    pub base_path: String,
    pub metadata: ObjdiffUnitMetadata,
}

#[derive(serde::Serialize)]
pub struct ObjdiffCategory {
    pub id: String,
    pub name: String,
}

#[derive(serde::Serialize)]
pub struct ObjdiffFile {
    pub min_version: String,
    pub custom_make: String,
    pub build_target: bool,
    pub watch_patterns: Vec<String>,
    pub units: Vec<ObjdiffUnit>,
    pub progress_categories: Vec<ObjdiffCategory>,
}

#[derive(serde::Serialize)]
pub struct CompileCommandEntry {
    pub directory: String,
    pub file: String,
    pub arguments: Vec<String>,
}

/// Returns the C++ compile flags for a unit by merging `cflags` and `cxxflags`:
/// all flags from `cflags[key]` (the shared base) are always included, and any
/// additional flags that appear in `cxxflags[key]` but not already in the cflags
/// set are appended after. When `cxxflags` is `None`, this is equivalent to
/// `get_flags(key, cflags, ...)`.
///
/// This prevents per-unit flags declared only in `cflags` (e.g. `/GX`, `/G6`)
/// from being silently dropped when `cxxflags` is also present in the config.
pub fn get_cxx_flags_merged(
    flags_key: &str,
    cflags: &IndexMap<String, FlagSet>,
    cxxflags: Option<&IndexMap<String, FlagSet>>,
    compiler_root: &str,
    sdk_root: &str,
) -> anyhow::Result<Vec<String>> {
    let mut result = get_flags(flags_key, cflags, compiler_root, sdk_root)?;
    if let Some(cxx_dict) = cxxflags {
        let extra = get_flags(flags_key, cxx_dict, compiler_root, sdk_root)?;
        let seen: std::collections::HashSet<String> = result.iter().cloned().collect();
        for f in extra {
            if !seen.contains(&f) {
                result.push(f);
            }
        }
    }
    Ok(result)
}

/// Parses a single line for a local `#include "..."` directive, returning the
/// quoted path. Matches `#include "x"` with optional whitespace after `#` and
/// after `include`. Returns None for `#include <...>` (angle brackets) and for
/// commented lines like `// #include "x"` (which don't start with `#`).
fn parse_local_include(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('#')?;
    let rest = rest.trim_start().strip_prefix("include")?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Joins `rel` onto `base_dir`, collapsing `.`/`..` lexically while staying
/// relative (unlike `lexical_absolute`, which prepends the CWD).
fn lexical_join(base_dir: &Path, rel: &str) -> PathBuf {
    let mut result = base_dir.to_path_buf();
    for component in Path::new(rel).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            c => result.push(c.as_os_str()),
        }
    }
    result
}

/// Recursively collects local `#include`d files that exist on disk into
/// `visited` (keyed by forward-slash path). Read errors / missing files
/// contribute nothing.
fn scan_includes_into(file: &Path, visited: &mut BTreeSet<String>) {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(_) => return,
    };
    let dir = file.parent().unwrap_or_else(|| Path::new(""));
    for line in text.lines() {
        if let Some(inc) = parse_local_include(line) {
            let resolved = lexical_join(dir, inc);
            if resolved.exists() {
                let key = to_forward_path(&resolved.to_string_lossy());
                if visited.insert(key) {
                    scan_includes_into(&resolved, visited);
                }
            }
        }
    }
}

/// Transitive set of local project files an entrypoint `#include`s, as sorted,
/// deduped, forward-slash paths (excluding the entrypoint). Used to declare
/// ninja implicit inputs for unity-build entrypoints, since MSVC 6.0 has no
/// `/showIncludes` for ninja's `deps = msvc`. Empty for a missing or
/// include-free entrypoint.
pub fn collect_implicit_deps(entrypoint: &Path) -> Vec<String> {
    let mut visited = BTreeSet::new();
    let entry_key = to_forward_path(&entrypoint.to_string_lossy());
    visited.insert(entry_key.clone());
    scan_includes_into(entrypoint, &mut visited);
    visited.remove(&entry_key);
    visited.into_iter().collect()
}

pub fn write_ninja(config_id: &str, config: &Config, objects: &Objects) -> anyhow::Result<String> {
    let mut out = String::new();
    let cxx = &config.compiler;
    let cc = config.compiler_c.as_deref().unwrap_or(cxx);
    let is_msvc = cxx.to_lowercase().ends_with("cl.exe");

    out.push_str(&format!("cc = {cc}\n\n"));
    out.push_str(&format!("cxx = {cxx}\n\n"));

    if is_msvc {
        out.push_str("rule compile_c\n");
        out.push_str("  command = $cc $cflags /c $in \"/Fo$out\"\n");
        out.push_str("  description = Compiling $in\n\n");
        out.push_str("rule compile_cxx\n");
        out.push_str("  command = $cxx $cxxflags /c $in \"/Fo$out\"\n");
        out.push_str("  description = Compiling $in\n\n");
    } else {
        out.push_str("rule compile_c\n");
        out.push_str("  command = $cc $cflags -c $in -o $out\n");
        out.push_str("  description = Compiling $in\n\n");
        out.push_str("rule compile_cxx\n");
        out.push_str("  command = $cxx $cxxflags -c $in -o $out\n");
        out.push_str("  description = Compiling $in\n\n");
    }

    let mut all_objs = Vec::new();
    for (lib_name, lib) in objects {
        let c_flags = get_flags(&lib.cflags, &config.cflags, &config.compiler_root, &config.sdk_root)?.join(" ");
        let cxx_flags = get_cxx_flags_merged(&lib.cflags, &config.cflags, config.cxxflags.as_ref(), &config.compiler_root, &config.sdk_root)?.join(" ");

        for src in lib.objects.keys() {
            let obj = get_target_path(config_id, lib_name, src);
            all_objs.push(obj.clone());
            let (rule, flags_var, flags_str) = if is_c_source(src) {
                ("compile_c", "cflags", &c_flags)
            } else {
                ("compile_cxx", "cxxflags", &cxx_flags)
            };
            let input = to_forward_path(&format!("{SOURCE_ROOT}/{src}"));
            let deps = collect_implicit_deps(Path::new(src));
            if deps.is_empty() {
                out.push_str(&format!("build {obj}: {rule} {input}\n"));
            } else {
                let dep_str = deps
                    .iter()
                    .map(|d| to_forward_path(&format!("{SOURCE_ROOT}/{d}")))
                    .collect::<Vec<_>>()
                    .join(" ");
                out.push_str(&format!("build {obj}: {rule} {input} | {dep_str}\n"));
            }
            out.push_str(&format!("  {flags_var} = {flags_str}\n\n"));
        }
    }

    out.push_str("build all: phony $\n");
    for obj in &all_objs {
        out.push_str(&format!("  {obj} $\n"));
    }
    out.push_str("\ndefault all\n");
    Ok(out)
}

pub fn write_objdiff(config_id: &str, config: &Config, objects: &Objects) -> ObjdiffFile {
    let mut units = Vec::new();
    for (lib_name, lib) in objects {
        let delink_name = lib.delink.clone().unwrap_or_else(|| lib_name.clone());
        for (src, target) in &lib.objects {
            let target_name = match target {
                Some(t) if !t.is_empty() => t.clone(),
                _ => src.clone(),
            };
            units.push(ObjdiffUnit {
                name: strip_last_extension(&to_forward_path(src)).to_string(),
                target_path: get_delink_path(config_id, &delink_name, &target_name),
                base_path: get_target_path(config_id, lib_name, src),
                metadata: ObjdiffUnitMetadata {
                    complete: false,
                    reverse_fn_order: false,
                    source_path: format!("{SOURCE_ROOT}/{}", to_forward_path(src)),
                    progress_categories: vec![lib.progress_category.clone()],
                    auto_generated: false,
                },
            });
        }
    }

    let progress_categories = config
        .progress_categories
        .iter()
        .map(|(id, name)| ObjdiffCategory { id: id.clone(), name: name.clone() })
        .collect();

    ObjdiffFile {
        min_version: "2.0.0-beta.5".to_string(),
        custom_make: "ninja".to_string(),
        build_target: false,
        watch_patterns: ["*.c", "*.cp", "*.cpp", "*.h", "*.hpp", "*.inc", "*.py", "*.yml", "*.txt", "*.json"]
            .into_iter()
            .map(String::from)
            .collect(),
        units,
        progress_categories,
    }
}

pub fn write_compile_commands(config: &Config, objects: &Objects) -> anyhow::Result<Vec<CompileCommandEntry>> {
    let cxx = &config.compiler;
    let cc = config.compiler_c.as_deref().unwrap_or(cxx);
    let is_msvc = cxx.to_lowercase().ends_with("cl.exe");

    let directory = lexical_absolute(Path::new(SOURCE_ROOT))?.to_string_lossy().to_string();
    let mut entries = Vec::new();

    for lib in objects.values() {
        let c_flags: Vec<String> = get_flags(&lib.cflags, &config.cflags, &config.compiler_root, &config.sdk_root)?
            .iter()
            .map(|f| strip_arg_quotes(f))
            .collect();
        let cxx_flags: Vec<String> = get_cxx_flags_merged(&lib.cflags, &config.cflags, config.cxxflags.as_ref(), &config.compiler_root, &config.sdk_root)?
            .iter()
            .map(|f| strip_arg_quotes(f))
            .collect();

        for src in lib.objects.keys() {
            let (compiler, flags): (&str, &Vec<String>) = if is_c_source(src) {
                (cc, &c_flags)
            } else {
                (cxx.as_str(), &cxx_flags)
            };
            let file_path = lexical_absolute(&Path::new(SOURCE_ROOT).join(src))?
                .to_string_lossy()
                .to_string();
            let compile_flag = if is_msvc { "/c" } else { "-c" };

            let mut arguments = vec![compiler.to_string(), compile_flag.to_string()];
            arguments.extend(flags.iter().cloned());
            arguments.push(file_path.clone());

            entries.push(CompileCommandEntry { directory: directory.clone(), file: file_path, arguments });
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flagset_map() -> IndexMap<String, FlagSet> {
        let json = r#"{
            "decomp": { "flags": ["/O2", "\"/I$(SOURCE_ROOT)/src\"", "\"/I$(COMPILER_ROOT)/INCLUDE\""] },
            "x86math.dll": { "base": "decomp", "flags": ["/G6", "/D_DLL"] }
        }"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn flattens_base_chain_and_substitutes_placeholders() {
        let map = flagset_map();
        let flat = flatten_cflags("x86math.dll", &map, "build/msvc6.0", "build/msvc6.0").unwrap();
        assert_eq!(
            flat,
            vec![
                "/O2",
                "\"/I./src\"",
                "\"/Ibuild/msvc6.0/INCLUDE\"",
                "/G6",
                "/D_DLL",
            ]
        );
    }

    #[test]
    fn get_flags_returns_empty_for_unknown_key() {
        let map = flagset_map();
        let flat = get_flags("nonexistent", &map, "build/msvc6.0", "build/msvc6.0").unwrap();
        assert!(flat.is_empty());
    }

    #[test]
    fn builds_target_path_under_obj_scheme() {
        let path = get_target_path("052103", "x86math.dll", "src/x86math.dll.cpp");
        assert_eq!(path, "build/052103/obj/x86math.dll/src/x86math.dll.obj");
    }

    #[test]
    fn builds_delink_path() {
        let path = get_delink_path("052103", "x86math.dll", "x86math.dll.obj");
        assert_eq!(path, "build/052103/delink/x86math.dll/x86math.dll.obj");
    }

    #[test]
    fn strips_matching_quote_pair_only() {
        assert_eq!(strip_arg_quotes("\"/Fofoo.obj\""), "/Fofoo.obj");
        assert_eq!(strip_arg_quotes("/O2"), "/O2");
        assert_eq!(strip_arg_quotes("\""), "\"");
    }

    #[test]
    fn identifies_c_sources_case_insensitively() {
        assert!(is_c_source("foo.C"));
        assert!(is_c_source("foo.c"));
        assert!(!is_c_source("foo.cpp"));
        assert!(!is_c_source("foo.cc"));
    }

    #[test]
    fn lexical_absolute_does_not_require_existence() {
        let cwd = std::env::current_dir().unwrap();
        let resolved = lexical_absolute(Path::new("does/not/exist.cpp")).unwrap();
        assert_eq!(resolved, cwd.join("does").join("not").join("exist.cpp"));
    }

    #[test]
    fn lexical_absolute_handles_dot() {
        let cwd = std::env::current_dir().unwrap();
        let resolved = lexical_absolute(Path::new(".")).unwrap();
        assert_eq!(resolved, cwd);
    }

    fn make_tree(files: &[(&str, &str)]) -> std::path::PathBuf {
        let root = std::env::temp_dir()
            .join(format!("flboot-inc-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::remove_dir_all(&root).ok();
        for (rel, contents) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, contents).unwrap();
        }
        root
    }

    #[test]
    fn scans_direct_transitive_and_relative_includes() {
        let root = make_tree(&[
            ("src/x86math.dll/x86math.dll.cpp", "#include \"quaternion.cpp\"\n#include <windows.h>\n// #include \"skip.cpp\"\n#include \"../shared/util.cpp\"\n#include \"nope.cpp\"\n"),
            ("src/x86math.dll/quaternion.cpp", "#include \"quaternion.h\"\nint q(){return 1;}\n"),
            ("src/x86math.dll/quaternion.h", "int q();\n"),
            ("src/shared/util.cpp", "int u(){return 2;}\n"),
        ]);
        let deps = collect_implicit_deps(&root.join("src/x86math.dll/x86math.dll.cpp"));
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(deps.len(), 3, "got {deps:?}");
        assert!(deps[0].ends_with("src/shared/util.cpp"), "got {:?}", deps[0]);
        assert!(deps[1].ends_with("src/x86math.dll/quaternion.cpp"), "got {:?}", deps[1]);
        assert!(deps[2].ends_with("src/x86math.dll/quaternion.h"), "got {:?}", deps[2]);
    }

    #[test]
    fn ignores_angle_brackets_comments_and_missing() {
        let root = make_tree(&[
            ("src/a/a.cpp", "#include <string.h>\n// #include \"c.cpp\"\n#include \"missing.cpp\"\n"),
        ]);
        let deps = collect_implicit_deps(&root.join("src/a/a.cpp"));
        std::fs::remove_dir_all(&root).ok();
        assert!(deps.is_empty(), "expected no deps, got {deps:?}");
    }

    #[test]
    fn is_cycle_safe() {
        let root = make_tree(&[
            ("src/a/a.cpp", "#include \"b.cpp\"\n"),
            ("src/a/b.cpp", "#include \"a.cpp\"\n"),
        ]);
        let deps = collect_implicit_deps(&root.join("src/a/a.cpp"));
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(deps.len(), 1, "got {deps:?}");
        assert!(deps[0].ends_with("src/a/b.cpp"), "got {:?}", deps[0]);
    }

    #[test]
    fn missing_entrypoint_yields_empty() {
        let deps = collect_implicit_deps(std::path::Path::new("does/not/exist.cpp"));
        assert!(deps.is_empty());
    }
}

#[cfg(test)]
mod generation_tests {
    use super::*;

    fn fixture_config() -> Config {
        let json = r#"{
            "progress_categories": {"A.dll": "A.dll"},
            "compiler": "build/msvc6.0/BIN/CL.EXE",
            "compiler_c": "build/msvc6.0/BIN/CL.EXE",
            "compiler_root": "build/msvc6.0",
            "sdk_root": "build/msvc6.0",
            "cflags": {
                "decomp": {"flags": ["/O2"]},
                "A.dll": {"base": "decomp", "flags": ["/D_DLL"]}
            }
        }"#;
        serde_json::from_str(json).unwrap()
    }

    fn fixture_objects() -> Objects {
        let json = r#"{
            "A.dll": {
                "progress_category": "A.dll",
                "cflags": "A.dll",
                "idapro": "config/x/splits/A.dll.json",
                "objects": {"src/A.dll.cpp": "A.dll.obj"}
            }
        }"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn write_ninja_emits_compile_rule_and_build_edge() {
        let config = fixture_config();
        let objects = fixture_objects();
        let ninja = write_ninja("cfgid", &config, &objects).unwrap();
        assert!(ninja.contains("cc = build/msvc6.0/BIN/CL.EXE"));
        assert!(ninja.contains("rule compile_cxx"));
        assert!(ninja.contains("build build/cfgid/obj/A.dll/src/A.dll.obj: compile_cxx ./src/A.dll.cpp"));
        assert!(ninja.contains("cxxflags = /O2 /D_DLL"));
        assert!(ninja.contains("build all: phony"));
        assert!(ninja.contains("build/cfgid/obj/A.dll/src/A.dll.obj"));
    }

    #[test]
    fn write_objdiff_builds_expected_unit() {
        let config = fixture_config();
        let objects = fixture_objects();
        let file = write_objdiff("cfgid", &config, &objects);
        assert_eq!(file.units.len(), 1);
        le
use crate::model::Objects;

/// Accepts "x86math", "x86math.dll", "src/x86math.dll.cpp", and the object-file
/// forms "x86math.dll.obj" / "src/x86math.dll.obj" -> "x86math.dll".
pub fn norm_unit(objects: &Objects, name: &str) -> anyhow::Result<String> {
    let replaced = name.replace('\\', "/");
    let last = replaced.rsplit('/').next().unwrap_or(&replaced);
    let base = last
        .strip_suffix(".cpp")
        .or_else(|| last.strip_suffix(".obj"))
        .unwrap_or(last);

    if objects.contains_key(base) {
        return Ok(base.to_string());
    }
    for key in objects.keys() {
        if key == base || key.split('.').next() == Some(base) {
            return Ok(key.clone());
        }
    }
    anyhow::bail!("unknown unit: {name}")
}

pub fn cmd_delink(config_id: &str, unit_arg: &str) -> anyhow::Result<()> {
    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;
    let unit = norm_unit(&objects, unit_arg)?;
    let tools = crate::manifest::load_tools_manifest()?;
    let delink_exe = crate::bootstrap::resolve_delink(&tools, None, crate::bootstrap::ToolMiss::RequireBootstrapped)?;
    crate::bootstrap::delink_one(&objects, config_id, &unit, &delink_exe)
}

pub fn obj_target(config_id: &str, unit: &str) -> String {
    format!("build/{config_id}/obj/{unit}/src/{unit}.obj")
}

/// Replicates Python's `shlex.split(line, posix=False)`: splits on unquoted
/// whitespace, keeps quote characters attached to their token (no exact
/// Rust crate equivalent for this specific non-posix quoting mode).
fn shlex_split_non_posix(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut has_token = false;

    for c in line.chars() {
        if c == '"' {
            in_quotes = !in_quotes;
            current.push(c);
            has_token = true;
        } else if c.is_whitespace() && !in_quotes {
            if has_token {
                tokens.push(std::mem::take(&mut current));
                has_token = false;
            }
        } else {
            current.push(c);
            has_token = true;
        }
    }
    if has_token {
        tokens.push(current);
    }
    tokens
}

fn strip_arg_quotes(flag: &str) -> String {
    let bytes = flag.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        flag[1..flag.len() - 1].to_string()
    } else {
        flag.to_string()
    }
}

/// Extracts a compile command's output object path from its (already
/// quote-stripped) tokens: MSVC's `/Fo<path>` or the portable `-o <path>`.
/// Needed because `fl build` runs ninja's command lines directly instead
/// of via ninja, so nothing creates the output's parent directory the way
/// ninja would -- and CL.EXE's /Fo does not create it, failing with C1083.
fn compile_output_path(parts: &[String]) -> Option<std::path::PathBuf> {
    for (i, p) in parts.iter().enumerate() {
        if let Some(rest) = p.strip_prefix("/Fo")
            && !rest.is_empty()
        {
            return Some(std::path::PathBuf::from(rest));
        }
        if p == "-o"
            && let Some(next) = parts.get(i + 1)
        {
            return Some(std::path::PathBuf::from(next));
        }
    }
    None
}

/// Invokes the compiler directly on Windows; wraps it with `wine` everywhere
/// else, since MSVC6 has no non-Windows build. UNVERIFIED: written from
/// documented Wine behavior (`wine <path-to-windows-exe>` with a Unix
/// working directory Wine translates transparently) without access to a
/// real Linux/Wine machine to test against -- every argument passed to the
/// compiler is already relative to the repo root (see codegen.rs's
/// SOURCE_ROOT convention), so only this function's own `exe` argument is
/// ever an absolute path, and it becomes Wine's own target rather than a
/// compiler argument needing translation.
#[cfg(windows)]
fn compiler_command(exe: &str) -> anyhow::Result<std::process::Command> {
    Ok(std::process::Command::new(exe))
}

#[cfg(unix)]
fn compiler_command(exe: &str) -> anyhow::Result<std::process::Command> {
    which::which("wine").map_err(|_| {
        anyhow::anyhow!(
            "MSVC6 compilation requires Wine on non-Windows platforms; install it \
             (e.g. `apt install wine`) and ensure `wine` is on PATH"
        )
    })?;
    let mut cmd = std::process::Command::new("wine");
    cmd.arg(exe);
    Ok(cmd)
}

pub fn cmd_build(config_id: &str, unit_args: &[String]) -> anyhow::Result<()> {
    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;
    let targets: Vec<String> = unit_args
        .iter()
        .map(|a| norm_unit(&objects, a).map(|u| obj_target(config_id, &u)))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let tools = crate::manifest::load_tools_manifest()?;
    let ninja_exe = crate::bootstrap::resolve_ninja(&tools, crate::bootstrap::ToolMiss::RequireBootstrapped)?;
    let output = std::process::Command::new(&ninja_exe)
        .arg("-t")
        .arg("commands")
        .args(&targets)
        .output()
        .map_err(|e| anyhow::anyhow!("running ninja -t commands: {e}"))?;
    if !output.status.success() {
        anyhow::bail!("ninja -t commands failed:\n{}", String::from_utf8_lossy(&output.stderr));
    }

    let repo_root = std::env::current_dir()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut failed = false;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts: Vec<String> = shlex_split_non_posix(line).into_iter().map(|p| strip_arg_quotes(&p)).collect();
        if parts.is_empty() {
            continue;
        }
        // ninja creates each edge's output directory before running it; since
        // we run the command ourselves, we must too, or CL.EXE's /Fo fails
        // with C1083 when build/<id>/obj/.../ doesn't already exist.
        if let Some(out) = compile_output_path(&parts)
            && let Some(parent) = out.parent()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("creating output dir {}: {e}", parent.display()))?;
        }
        let mut command = if parts[0].to_lowercase().ends_with("cl.exe") {
            parts[0] = repo_root.join(&parts[0]).to_string_lossy().to_string();
            compiler_command(&parts[0])?
        } else {
            std::process::Command::new(&parts[0])
        };
        let status = command
            .args(&parts[1..])
            .status()
            .map_err(|e| anyhow::anyhow!("running compile command: {e}"))?;
        if !status.success() {
            eprintln!("FAILED: {line}");
            failed = true;
        }
    }

    if failed {
        anyhow::bail!("one or more compile commands failed");
    }
    Ok(())
}

pub fn cmd_claim(config_id: &str, unit_arg: &str, renames: &[String]) -> anyhow::Result<()> {
    if renames.is_empty() {
        anyhow::bail!("claim needs at least one old=new pair");
    }

    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;
    let unit = norm_unit(&objects, unit_arg)?;

    let mut pairs = Vec::new();
    for r in renames {
        let (old, new) = r.split_once('=').ok_or_else(|| anyhow::anyhow!("expected old=new, got {r:?}"))?;
        pairs.push((old.to_string(), new.to_string()));
    }

    let split_path = format!("config/{config_id}/splits/{unit}.json");
    let delink_path = format!("config/{config_id}/delink/{unit}.delink.json");
    let mut found: Vec<(String, bool)> = pairs.iter().map(|(old, _)| (old.clone(), false)).collect();

    for path in [&split_path, &delink_path] {
        apply_renames_in_file(path, &pairs, &mut found)?;
    }

    let missing: Vec<&str> = found.iter().filter(|(_, ok)| !ok).map(|(old, _)| old.as_str()).collect();
    if !missing.is_empty() {
        anyhow::bail!("symbol(s) not found in split or delink: {}", missing.join(", "));
    }

    crate::log::info(&format!("claimed {} symbol(s) in {unit}; re-delinking...", pairs.len()));
    let tools = crate::manifest::load_tools_manifest()?;
    let delink_exe = crate::bootstrap::resolve_delink(&tools, None, crate::bootstrap::ToolMiss::RequireBootstrapped)?;
    crate::bootstrap::delink_one(&objects, config_id, &unit, &delink_exe)
}

/// Raw text substitution -- never json.load/dump -- so unrelated formatting
/// and ordering in the multi-thousand-line split/delink JSON is untouched.
fn apply_renames_in_file(path: &str, pairs: &[(String, String)], found: &mut [(String, bool)]) -> anyhow::Result<()> {
    let mut text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading {path}: {e}"))?;
    let mut changed = false;
    for (old, new) in pairs {
        let needle = format!("\"{old}\"");
        if text.contains(&needle) {
            text = text.replace(&needle, &format!("\"{new}\""));
            changed = true;
            if let Some(entry) = found.iter_mut().find(|(o, _)| o == old) {
                entry.1 = true;
            }
        }
    }
    if changed {
        std::fs::write(path, text).map_err(|e| anyhow::anyhow!("writing {path}: {e}"))?;
    }
    Ok(())
}

/// Loose symbol key for matching a requested symbol against objdiff's symbol list.
///
/// C++ MSVC-mangled names start with `?` and use `@` as a *structural* separator
/// (`?method@Class@@signature`), so two different classes' identically-named
/// methods share the `?method` prefix. Truncating at the first `@` (correct only
/// for C decoration) would collapse them -- e.g. `?get_base@IObjInspectImpl@@...`
/// and `?get_base@CEqObj@@...` both to `?get_base` -- making dis/diff resolve to
/// whichever the report lists first (usually a smaller, unrelated twin). Keep such
/// names whole so each resolves to exactly its own function.
///
/// Only C-style stdcall/fastcall names (`_name@N`, `@name@N`) get the leading
/// `_`/`@` and trailing `@N` byte-count decoration stripped.
pub fn symbol_key(name: &str) -> String {
    if name.starts_with('?') {
        return name.to_string();
    }
    let trimmed = name.trim_start_matches(['_', '@']);
    match trimmed.find('@') {
        Some(idx) => trimmed[..idx].to_string(),
        None => trimmed.to_string(),
    }
}

const COL: usize = 44;
const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";

/// Wraps `s` in `escape` only when colorizing, so callers can build a line
/// without branching at every field.
fn paint(s: &str, escape: &str, color: bool) -> String {
    if color && !escape.is_empty() {
        format!("{escape}{s}{RESET}")
    } else {
        s.to_string()
    }
}

/// A match percentage, green at 100% and red below, so a fully-matched
/// function is recognizable without reading the number.
fn paint_pct(pct: Option<f32>, color: bool) -> String {
    let text = crate::objdiff::fmt_pct(pct);
    let escape = match pct {
        Some(p) if p >= 100.0 => GREEN,
        Some(_) => RED,
        None => "",
    };
    paint(&text, escape, color)
}

/// Renders one function's target-vs-ours instruction table, headed by a
/// separator line naming the symbol and its match percentages.
fn render_one(view: &crate::objdiff::FnView, color: bool) -> String {
    let empty: Vec<crate::objdiff::Row> = Vec::new();
    let lp = paint_pct(view.target.as_ref().and_then(|s| s.match_percent), color);
    let rp = paint_pct(view.base.as_ref().and_then(|s| s.match_percent), color);

    let mut out = String::new();
    out.push_str(&format!("==== {}  TARGET {lp}%  OURS {rp}% ====\n", view.name));
    out.push_str(&format!("{:<COL$} | OURS\n", "TARGET"));
    out.push_str(&"-".repeat(90));
    out.push('\n');

    let lr = view.target.as_ref().map(|s| &s.rows).unwrap_or(&empty);
    let rr = view.base.as_ref().map(|s| &s.rows).unwrap_or(&empty);
    for i in 0..lr.len().max(rr.len()) {
        let (left, right) = (lr.get(i), rr.get(i));
        let changed = |r: Option<&crate::objdiff::Row>| r.is_some_and(|r| r.changed);
        let mark = if changed(left) || changed(right) {
            paint("<>", RED, color)
        } else {
            "  ".to_string()
        };

        // Pad against the row's printed width: `colored` carries escapes that
        // cost bytes but no columns, so `{:<44}` on it would misalign.
        let la = left.map(|r| r.display(color)).unwrap_or_default();
        let pad = COL.saturating_sub(left.map(|r| r.width()).unwrap_or(0));
        let ra = right.map(|r| r.display(color)).unwrap_or_default();

        out.push_str(format!("{la}{:pad$} |{mark}| {ra}", "").trim_end());
        out.push('\n');
    }
    out
}

/// Renders the diff for a single named function, or -- when `symbol` is `None`
/// -- every function in the target, each under its own separator header.
fn render_diff(views: &[crate::objdiff::FnView], symbol: Option<&str>, color: bool) -> String {
    if views.is_empty() {
        return match symbol {
            // A requested symbol that matched nothing still gets a header, so
            // the caller sees the dashes rather than silence.
            Some(sym) => render_one(
                &crate::objdiff::FnView {
                    name: sym.to_string(),
                    size: 0,
                    target: None,
                    base: None,
                },
                color,
            ),
            None => "no functions found in target\n".to_string(),
        };
    }

    let mut out = String::new();
    for (i, view) in views.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&render_one(view, color));
    }
    out
}

/// Diff one function (`symbol = Some`) or every function in the unit
/// (`symbol = None`). `unit_arg` accepts any form `norm_unit` understands,
/// including the object-file name (e.g. "x86math.dll.obj").
pub fn cmd_diff(config_id: &str, unit_arg: &str, symbol: Option<&str>, color: bool) -> anyhow::Result<()> {
    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;
    let unit = norm_unit(&objects, unit_arg)?;

    let unit_diff = crate::objdiff::diff_unit(config_id, &objects, &unit)?;
    let views = unit_diff.function_views(symbol)?;

    print!("{}", render_diff(&views, symbol, color));
    Ok(())
}

pub fn cmd_dis(config_id: &str, unit_arg: &str, symbols: &[String], color: bool) -> anyhow::Result<()> {
    if symbols.is_empty() {
        anyhow::bail!("dis needs at least one symbol");
    }
    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;
    let unit = norm_unit(&objects, unit_arg)?;

    let unit_diff = crate::objdiff::diff_unit(config_id, &objects, &unit)?;
    let views = unit_diff.function_views(None)?;
    let by_key: std::collections::HashMap<String, &crate::objdiff::FnView> =
        views.iter().map(|v| (symbol_key(&v.name), v)).collect();

    for sym in symbols {
        let found = by_key.get(&symbol_key(sym));
        let size = found.map(|v| v.size.to_string()).unwrap_or_else(|| "?".to_string());
        println!("==== {sym} (size {size}) ====");
        let Some(target) = found.and_then(|v| v.target.as_ref()) else {
            println!("  <not found in target>");
            continue;
        };
        for row in &target.rows {
            println!("  {}", row.display(color));
        }
    }
    Ok(())
}

pub fn cmd_progress(config_id: &str, unit_args: &[String]) -> anyhow::Result<()> {
    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;

    let tools = crate::manifest::load_tools_manifest()?;
    let objdiff_cli = crate::bootstrap::resolve_objdiff_cli(&tools, None, crate::bootstrap::ToolMiss::RequireBootstrapped)?;
    let output = std::process::Command::new(&objdiff_cli)
        .args(["report", "generate", "-o", "-", "--format", "json"])
        .output()
        .map_err(|e| anyhow::anyhow!("running objdiff-cli report: {e}"))?;
    if !output.status.success() {
        anyhow::bail!("objdiff-cli report failed:\n{}", String::from_utf8_lossy(&output.stderr));
    }
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    let m = &report["measures"];
    println!(
        "OVERALL: {}/{} functions, code {:.4}%",
        m["matched_functions"].as_u64().unwrap_or(0),
        m["total_functions"].as_u64().unwrap_or(0),
        m["matched_code_percent"].as_f64().unwrap_or(0.0),
    );

    let wanted: std::collections::HashSet<String> = unit_args
        .iter()
        .map(|a| norm_unit(&objects, a).map(|u| format!("src/{u}")))
        .collect::<anyhow::Result<_>>()?;

    let empty_units = Vec::new();
    for u in report["units"].as_array().unwrap_or(&empty_units) {
        let name = u["name"].as_str().unwrap_or("");
        if !wanted.is_empty() && !wanted.contains(name) {
            continue;
        }
        let um = &u["measures"];
        let matched_functions = um["matched_functions"].as_u64().unwrap_or(0);
        if wanted.is_empty() && matched_functions == 0 {
            continue;
        }
        println!(
            "  {:<20} {:>3}/{:<4}  code {:>6.2}%",
            name,
            matched_functions,
            um["total_functions"].as_u64().unwrap_or(0),
            um["matched_code_percent"].as_f64().unwrap_or(0.0),
        );
        if !wanted.is_empty() {
            let empty_fns = Vec::new();
            for f in u["functions"].as_array().unwrap_or(&empty_fns) {
                let pct = f["fuzzy_match_percent"].as_f64().unwrap_or(0.0);
                let mark = if pct >= 100.0 { "OK " } else { "   " };
                let size = f["size"].as_u64().map(|s| s.to_string()).unwrap_or_else(|| "?".to_string());
                println!("      {mark} {pct:>6.1}% {size:>5}  {}", f["name"].as_str().unwrap_or(""));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_objects() -> Objects {
        let json = r#"{
            "x86math.dll": {
                "progress_category": "x86math.dll",
                "cflags": "x86math.dll",
                "idapro": "config/x/splits/x86math.dll.json",
                "objects": {"src/x86math.dll.cpp": "x86math.dll.obj"}
            }
        }"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn resolves_bare_name_via_dot_split() {
        let objects = fixture_objects();
        assert_eq!(norm_unit(&objects, "x86math").unwrap(), "x86math.dll");
    }

    #[test]
    fn resolves_exact_key() {
        let objects = fixture_objects();
        assert_eq!(norm_unit(&objects, "x86math.dll").unwrap(), "x86math.dll");
    }

    #[test]
    fn resolves_source_path_with_backslashes() {
        let objects = fixture_objects();
        assert_eq!(norm_unit(&objects, r"src\x86math.dll.cpp").unwrap(), "x86math.dll");
    }

    #[test]
    fn errors_on_unknown_unit() {
        let objects = fixture_objects();
        assert!(norm_unit(&objects, "nonexistent").is_err());
    }

    #[test]
    fn obj_target_matches_obj_scheme() {
        assert_eq!(obj_target("052103", "x86math.dll"), "build/052103/obj/x86math.dll/src/x86math.dll.obj");
    }

    #[test]
    fn shlex_split_keeps_quotes_and_splits_unquoted_whitespace() {
        let line = r#"cl.exe /O2 /Gs "/Fobuild/052103/obj/x86math.dll/src/x86math.dll.obj" ./src/x86math.dll.cpp"#;
        let tokens = shlex_split_non_posix(line);
        assert_eq!(
            tokens,
            vec![
                "cl.exe",
                "/O2",
                "/Gs",
                "\"/Fobuild/052103/obj/x86math.dll/src/x86math.dll.obj\"",
                "./src/x86math.dll.cpp",
            ]
        );
    }

    #[test]
    fn shlex_split_preserves_whitespace_inside_quotes() {
        let line = r#"cl.exe "/I C:/path with space/include""#;
        let tokens = shlex_split_non_posix(line);
        assert_eq!(tokens, vec!["cl.exe", "\"/I C:/path with space/include\""]);
    }

    #[test]
    fn strip_arg_quotes_removes_one_matching_pair() {
        assert_eq!(strip_arg_quotes("\"/Fofoo.obj\""), "/Fofoo.obj");
        assert_eq!(strip_arg_quotes("/O2"), "/O2");
    }

    #[test]
    fn extracts_msvc_fo_output_path() {
        let parts: Vec<String> = ["cl.exe", "/c", "src/x.cpp", "/Fobuild/052103/obj/x.dll/src/x.dll.obj"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            compile_output_path(&parts),
            Some(std::path::PathBuf::from("build/052103/obj/x.dll/src/x.dll.obj"))
        );
    }

    #[test]
    fn extracts_portable_dash_o_output_path() {
        let parts: Vec<String> = ["gcc", "-c", "x.c", "-o", "build/obj/x.o"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(compile_output_path(&parts), Some(std::path::PathBuf::from("build/obj/x.o")));
    }

    #[test]
    fn returns_none_when_no_output_flag() {
        let parts: Vec<String> = ["cl.exe", "src/x.cpp"].iter().map(|s| s.to_string()).collect();
        assert_eq!(compile_output_path(&parts), None);
    }

    #[test]
    fn apply_renames_replaces_only_quoted_key_occurrences() {
        let dir = std::env::temp_dir().join(format!("flboot-claim-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("split.json");
        std::fs::write(&path, "{\n  \"sub_6F71DE0\": {\n    \"address\": 1\n  }\n}").unwrap();

        let pairs = vec![("sub_6F71DE0".to_string(), "@sub_6F71DE0@12".to_string())];
        let mut found = vec![("sub_6F71DE0".to_string(), false)];
        apply_renames_in_file(path.to_str().unwrap(), &pairs, &mut found).unwrap();

        let updated = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(updated.contains("\"@sub_6F71DE0@12\""));
        assert!(!updated.contains("\"sub_6F71DE0\""));
        assert!(found[0].1);
    }

    #[test]
    fn apply_renames_leaves_found_false_when_symbol_absent() {
        let dir = std::env::temp_dir().join(format!("flboot-claim-test-absent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("split.json");
        std::fs::write(&path, "{\n  \"other_symbol\": {}\n}").unwrap();

        let pairs = vec![("sub_6F71DE0".to_string(), "@sub_6F71DE0@12".to_string())];
        let mut found = vec![("sub_6F71DE0".to_string(), false)];
        apply_renames_in_file(path.to_str().unwrap(), &pairs, &mut found).unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!found[0].1);
    }

    #[test]
    fn symbol_key_strips_stdcall_decoration() {
        assert_eq!(symbol_key("_DllMain@12"), "DllMain");
    }

    #[test]
    fn symbol_key_strips_fastcall_decoration() {
        assert_eq!(symbol_key("@x86MathEngine_QueryInterface@12"), "x86MathEngine_QueryInterface");
    }

    #[test]
    fn symbol_key_leaves_undecorated_names_alone() {
        assert_eq!(symbol_key("sub_6F71DE0"), "sub_6F71DE0");
    }

    #[test]
    fn symbol_key_keeps_cpp_mangled_name_whole() {
        assert_eq!(
            symbol_key("?get_base@IObjInspectImpl@@UBEHAAI@Z"),
            "?get_base@IObjInspectImpl@@UBEHAAI@Z"
        );
    }

    #[test]
    fn symbol_key_distinguishes_same_method_on_different_classes() {
        // The bug this guards against: both would collapse to "?get_base".
        assert_ne!(
            symbol_key("?get_base@IObjInspectImpl@@UBEHAAI@Z"),
            symbol_key("?get_base@CEqObj@@QBEIXZ")
        );
    }

    #[test]
    fn resolves_object_file_name() {
        let objects = fixture_objects();
        assert_eq!(norm_unit(&objects, "x86math.dll.obj").unwrap(), "x86math.dll");
        assert_eq!(norm_unit(&objects, "src/x86math.dll.obj").unwrap(), "x86math.dll");
    }

    /// A row whose colored form carries an escape, so tests can tell the two
    /// representations apart.
    fn row(text: &str, changed: bool) -> crate::objdiff::Row {
        crate::objdiff::Row {
            text: text.to_string(),
            colored: format!("\x1b[36m{text}\x1b[0m"),
            changed,
        }
    }

    fn side(pct: f32, rows: Vec<crate::objdiff::Row>) -> crate::objdiff::SideView {
        crate::objdiff::SideView { match_percent: Some(pct), rows }
    }

    /// `foo` exists on both sides; `bar` only in the target.
    ///
    /// Symbol filtering now lives in `UnitDiff::function_views`, which needs
    /// real objects, so these fixtures are the already-filtered set the
    /// renderer receives.
    fn foo_view() -> crate::objdiff::FnView {
        crate::objdiff::FnView {
            name: "?foo@@YAXXZ".to_string(),
            size: 8,
            target: Some(side(100.0, vec![row("0: push ebp", false)])),
            base: Some(side(87.5, vec![row("0: mov eax, 1", true)])),
        }
    }

    fn bar_view() -> crate::objdiff::FnView {
        crate::objdiff::FnView {
            name: "?bar@@YAXXZ".to_string(),
            size: 4,
            target: Some(side(50.0, vec![row("4: ret", true)])),
            base: None,
        }
    }

    #[test]
    fn render_diff_single_symbol_shows_only_that_function() {
        let out = render_diff(&[foo_view()], Some("?foo@@YAXXZ"), false);
        assert!(out.contains("==== ?foo@@YAXXZ"), "{out}");
        assert!(out.contains("TARGET 100.00%"), "{out}");
        assert!(out.contains("OURS 87.50%"), "{out}");
        assert!(out.contains("push ebp"), "{out}");
        assert!(!out.contains("?bar@@YAXXZ"), "should not include other functions: {out}");
    }

    /// A row changed on either side is marked, so an instruction that differs
    /// only in our build still flags.
    #[test]
    fn render_diff_marks_row_changed_on_either_side() {
        let out = render_diff(&[foo_view()], None, false);
        let line = out.lines().find(|l| l.contains("push ebp")).unwrap();
        assert!(line.contains("<>"), "changed row must be marked: {line}");
    }

    #[test]
    fn render_diff_leaves_unchanged_rows_unmarked() {
        let view = crate::objdiff::FnView {
            name: "?same@@YAXXZ".to_string(),
            size: 4,
            target: Some(side(100.0, vec![row("0: ret", false)])),
            base: Some(side(100.0, vec![row("0: ret", false)])),
        };
        let line = render_diff(&[view], None, false).lines().find(|l| l.contains("ret")).unwrap().to_string();
        assert!(!line.contains("<>"), "unchanged row must not be marked: {line}");
    }

    #[test]
    fn render_diff_all_shows_every_target_function_with_separators() {
        let out = render_diff(&[foo_view(), bar_view()], None, false);
        assert_eq!(out.matches("==== ").count(), 2, "one separator per function: {out}");
        assert!(out.contains("==== ?foo@@YAXXZ"), "{out}");
        assert!(out.contains("==== ?bar@@YAXXZ"), "{out}");
        // bar exists only in the target, so OURS has no percentage.
        let bar_header = out.lines().find(|l| l.contains("?bar@@YAXXZ")).unwrap();
        assert!(bar_header.contains("OURS -%"), "bar header: {bar_header}");
    }

    #[test]
    fn render_diff_missing_symbol_renders_dashes() {
        let out = render_diff(&[], Some("?nope@@YAXXZ"), false);
        assert!(out.contains("==== ?nope@@YAXXZ"), "{out}");
        assert!(out.contains("TARGET -%"), "{out}");
        assert!(out.contains("OURS -%"), "{out}");
    }

    #[test]
    fn render_diff_all_with_no_target_functions_reports_empty() {
        assert!(render_diff(&[], None, false).contains("no functions found"));
    }

    #[test]
    fn render_diff_emits_no_escapes_when_color_disabled() {
        let out = render_diff(&[foo_view()], None, false);
        assert!(!out.contains('\x1b'), "plain output must have no escapes: {out:?}");
    }

    #[test]
    fn render_diff_emits_escapes_when_color_enabled() {
        let out = render_diff(&[foo_view()], None, true);
        assert!(out.contains('\x1b'), "colored output must carry escapes");
        // Row content is styled by objdiff's own segment colors; the fixture
        // wraps the whole row, real rows are styled per segment.
        assert!(out.contains("\x1b[36m0: push ebp\x1b[0m"), "{out:?}");
    }

    /// The alignment invariant: escapes cost bytes but no columns, so the
    /// separator must land in the same column with and without color.
    #[test]
    fn color_does_not_shift_the_column_separator() {
        let strip = |s: &str| {
            let mut out = String::new();
            let mut chars = s.chars();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    for c in chars.by_ref() {
                        if c == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };

        let plain = render_diff(&[foo_view()], None, false);
        let colored = render_diff(&[foo_view()], None, true);
        assert_eq!(strip(&colored), plain, "stripping escapes must recover the plain rendering");
    }

    /// A 100% match reads green, anything less reads red.
    #[test]
    fn percentages_are_painted_by_match_state() {
        let out = render_diff(&[foo_view()], None, true);
        assert!(out.contains("\x1b[32m100.00\x1b[0m"), "100% should be green: {out:?}");
        assert!(out.contains("\x1b[31m87.50\x1b[0m"), "sub-100% should be red: {out:?}");
    }

    /// An absent side has no percentage to paint, so `-` stays unstyled.
    #[test]
    fn absent_side_percentage_is_unpainted() {
        let out = render_diff(&[bar_view()], None, true);
        assert!(out.contains("OURS -%"), "absent side should render bare dash: {out:?}");
    }
}

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

/// Minimum width of the TARGET column. Functions whose widest target row
/// exceeds this get a wider column instead, so the separator never drifts.
const MIN_COL: usize = 44;
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

/// Paints an already-formatted percentage green at 100%, red below.
fn paint_pct_text(text: &str, value: f32, color: bool) -> String {
    paint(text, if value >= 100.0 { GREEN } else { RED }, color)
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

/// A branch drawn in the gutter: the rows it spans, which end is the
/// destination, and objdiff's color index.
struct Branch {
    lo: usize,
    hi: usize,
    dest: usize,
    src: usize,
    branch_idx: u32,
    lane: usize,
}

/// Assigns each branch a lane, innermost first, so a lane is reused only by
/// branches whose spans do not overlap. Nested loops therefore nest visually
/// instead of overwriting one another.
fn assign_lanes(rows: &[crate::objdiff::Row]) -> Vec<Branch> {
    let mut branches: Vec<Branch> = rows
        .iter()
        .enumerate()
        .filter_map(|(src, r)| r.branch_to.map(|(dest, idx)| (src, dest, idx)))
        .filter(|(src, dest, _)| *dest < rows.len() && src != dest)
        .map(|(src, dest, branch_idx)| Branch {
            lo: src.min(dest),
            hi: src.max(dest),
            dest,
            src,
            branch_idx,
            lane: 0,
        })
        .collect();

    // Shorter spans take inner lanes (nearest the text).
    branches.sort_by_key(|b| (b.hi - b.lo, b.lo));

    let mut lane_ends: Vec<usize> = Vec::new();
    for b in branches.iter_mut() {
        // A lane is free when every branch already in it ends before this one
        // begins. Track only the furthest extent, since spans are sorted.
        let lane = lane_ends.iter().position(|end| *end < b.lo);
        match lane {
            Some(l) => {
                lane_ends[l] = b.hi;
                b.lane = l;
            }
            None => {
                lane_ends.push(b.hi);
                b.lane = lane_ends.len() - 1;
            }
        }
    }

    // Lane 0 is drawn rightmost (closest to the instruction text), so invert.
    let lanes = lane_ends.len();
    for b in branches.iter_mut() {
        b.lane = lanes - 1 - b.lane;
    }
    branches
}

/// Renders the branch gutter for one side: a per-row string plus its printed
/// width. Returns empty strings and width 0 when there is nothing to draw.
fn branch_gutter(rows: &[crate::objdiff::Row], color: bool) -> (Vec<String>, usize) {
    let branches = assign_lanes(rows);
    if branches.is_empty() {
        return (vec![String::new(); rows.len()], 0);
    }
    let lanes = branches.iter().map(|b| b.lane).max().unwrap_or(0) + 1;
    let width = lanes + 3; // lanes, then a two-char head, then a space

    let mut out = Vec::with_capacity(rows.len());
    for i in 0..rows.len() {
        // Each lane cell, and the branch that owns it (for color).
        let mut cells: Vec<(char, Option<u32>)> = vec![(' ', None); lanes];
        // Track the head's lane so the innermost branch wins when two terminate
        // on the same row; lanes increase toward the text.
        let mut head: Option<(&str, u32, usize)> = None;

        for b in &branches {
            if i < b.lo || i > b.hi {
                continue;
            }
            let ch = if i == b.lo {
                ','
            } else if i == b.hi {
                '`'
            } else {
                '|'
            };
            cells[b.lane] = (ch, Some(b.branch_idx));

            if i == b.dest || i == b.src {
                let arrow = if i == b.dest { "->" } else { "--" };
                if head.is_none_or(|(_, _, lane)| b.lane > lane) {
                    head = Some((arrow, b.branch_idx, b.lane));
                }
            }
        }

        // Run a horizontal from the endpoint's lane out to the head, so the
        // arrow reads as one connected line. Only blank cells are filled: an
        // inner lane's vertical must stay unbroken, since it belongs to a
        // different branch that merely passes through this row.
        if let Some((_, idx, lane)) = head {
            for cell in cells.iter_mut().skip(lane + 1) {
                if cell.0 == ' ' {
                    *cell = ('-', Some(idx));
                }
            }
        }

        let mut s = String::new();
        for (ch, idx) in &cells {
            match (color, idx) {
                (true, Some(idx)) if *ch != ' ' => {
                    s.push_str(crate::objdiff::branch_color(*idx));
                    s.push(*ch);
                    s.push_str(RESET);
                }
                _ => s.push(*ch),
            }
        }
        match head {
            Some((arrow, idx, _)) => {
                s.push_str(&paint(arrow, crate::objdiff::branch_color(idx), color))
            }
            None => s.push_str("  "),
        }
        s.push(' ');
        out.push(s);
    }
    (out, width)
}

/// Renders one function's target-vs-ours instruction table, headed by a
/// separator line naming the symbol and its match percentages.
fn render_one(view: &crate::objdiff::FnView, color: bool, branches: bool) -> String {
    let empty: Vec<crate::objdiff::Row> = Vec::new();
    let lp = paint_pct(view.target.as_ref().and_then(|s| s.match_percent), color);
    let rp = paint_pct(view.base.as_ref().and_then(|s| s.match_percent), color);

    let lr = view.target.as_ref().map(|s| &s.rows).unwrap_or(&empty);
    let rr = view.base.as_ref().map(|s| &s.rows).unwrap_or(&empty);

    let (lg, lgw) = if branches { branch_gutter(lr, color) } else { (vec![], 0) };
    let (rg, rgw) = if branches { branch_gutter(rr, color) } else { (vec![], 0) };
    let gutter = |g: &[String], i: usize, w: usize| {
        g.get(i).cloned().unwrap_or_else(|| " ".repeat(w))
    };

    // A single overlong target row would otherwise push the separator right on
    // that row alone -- and those are the changed rows, the ones worth reading.
    // Size the column to the function's widest row instead.
    let col = lr.iter().map(|r| r.width()).max().unwrap_or(0).max(MIN_COL) + lgw;

    let mut out = String::new();
    out.push_str(&format!("==== {}  TARGET {lp}%  OURS {rp}% ====\n", view.name));
    out.push_str(&format!("{:<col$} | OURS\n", "TARGET"));
    out.push_str(&"-".repeat(col + 6));
    out.push('\n');

    for i in 0..lr.len().max(rr.len()) {
        let (left, right) = (lr.get(i), rr.get(i));
        let changed = |r: Option<&crate::objdiff::Row>| r.is_some_and(|r| r.changed);
        let mark = if changed(left) || changed(right) {
            paint("<>", RED, color)
        } else {
            "  ".to_string()
        };

        // Pad against the row's printed width: `colored` and the gutter carry
        // escapes that cost bytes but no columns, so formatting them with
        // `{:<col$}` would misalign.
        let la = format!("{}{}", gutter(&lg, i, lgw), left.map(|r| r.display(color)).unwrap_or_default());
        let lw = lgw + left.map(|r| r.width()).unwrap_or(0);
        let pad = col.saturating_sub(lw);
        let ra = format!("{}{}", gutter(&rg, i, rgw), right.map(|r| r.display(color)).unwrap_or_default());

        out.push_str(format!("{la}{:pad$} |{mark}| {ra}", "").trim_end());
        out.push('\n');
    }
    out
}

/// Renders the diff for a single named function, or -- when `symbol` is `None`
/// -- every function in the target, each under its own separator header.
fn render_diff(views: &[crate::objdiff::FnView], symbol: Option<&str>, color: bool, branches: bool) -> String {
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
                branches,
            ),
            None => "no functions found in target\n".to_string(),
        };
    }

    let mut out = String::new();
    for (i, view) in views.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&render_one(view, color, branches));
    }
    out
}

/// Diff one function (`symbol = Some`) or every function in the unit
/// (`symbol = None`). `unit_arg` accepts any form `norm_unit` understands,
/// including the object-file name (e.g. "x86math.dll.obj").
pub fn cmd_diff(config_id: &str, unit_arg: &str, symbol: Option<&str>, color: bool, branches: bool) -> anyhow::Result<()> {
    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;
    let unit = norm_unit(&objects, unit_arg)?;

    let unit_diff = crate::objdiff::diff_unit(config_id, &objects, &unit)?;
    let views = unit_diff.function_views(symbol)?;

    print!("{}", render_diff(&views, symbol, color, branches));
    Ok(())
}

pub fn cmd_dis(config_id: &str, unit_arg: &str, symbols: &[String], color: bool, branches: bool) -> anyhow::Result<()> {
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
        let (gutter, width) = if branches {
            branch_gutter(&target.rows, color)
        } else {
            (vec![], 0)
        };
        for (i, row) in target.rows.iter().enumerate() {
            let g = gutter.get(i).cloned().unwrap_or_else(|| " ".repeat(width));
            println!("  {g}{}", row.display(color));
        }
    }
    Ok(())
}

type UnitMeasure = Option<(crate::objdiff::Measures, Vec<crate::objdiff::FnItem>)>;

/// Diffs and measures every unit, spreading the work across threads.
///
/// Units are independent, and one of them (Common.dll, ~8600 functions) dwarfs
/// the rest, so a serial pass is dominated by it. Results are returned in the
/// input order, and `None` marks a unit with no delinked target. Only measures
/// cross the thread boundary; the loaded objects stay inside their worker.
fn diff_units_parallel(units: &[crate::codegen::ObjdiffUnit]) -> anyhow::Result<Vec<UnitMeasure>> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(units.len().max(1));

    let measure_one = |unit: &crate::codegen::ObjdiffUnit| -> anyhow::Result<UnitMeasure> {
        let diff = crate::objdiff::diff_paths_with(
            &unit.target_path,
            &unit.base_path,
            crate::objdiff::report_config(),
        )?;
        if diff.target.is_none() {
            return Ok(None);
        }
        Ok(Some(crate::objdiff::unit_measures(&diff)))
    };

    if threads <= 1 {
        return units.iter().map(measure_one).collect();
    }

    // Chunk rather than spawn per unit: 28-ish units, and a chunk keeps each
    // worker's peak memory to one loaded object pair at a time.
    let chunk = units.len().div_ceil(threads);
    let mut out: Vec<UnitMeasure> = Vec::with_capacity(units.len());
    std::thread::scope(|scope| -> anyhow::Result<()> {
        let handles: Vec<_> = units
            .chunks(chunk)
            .map(|slice| scope.spawn(move || slice.iter().map(measure_one).collect::<anyhow::Result<Vec<_>>>()))
            .collect();
        for handle in handles {
            let chunk_result = handle
                .join()
                .map_err(|_| anyhow::anyhow!("progress worker panicked"))?;
            out.extend(chunk_result?);
        }
        Ok(())
    })?;
    Ok(out)
}

/// Match percentage report. Diffs every unit in-process and aggregates the
/// measures the way `objdiff-cli report generate` does.
///
/// Naming a unit expands it: the per-function breakdown is printed too, and
/// the unit is shown even at 0%. With no unit named, units that have matched
/// nothing are omitted to keep the summary short.
pub fn cmd_progress(config_id: &str, unit_args: &[String], color: bool) -> anyhow::Result<()> {
    let config: crate::model::Config =
        crate::model::load_jsonc(&crate::model::config_path(config_id, None))?;
    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;

    let wanted: std::collections::HashSet<String> = unit_args
        .iter()
        .map(|a| norm_unit(&objects, a).map(|u| format!("src/{u}")))
        .collect::<anyhow::Result<_>>()?;

    // The same unit list objdiff.json declares, so the report covers exactly
    // what the objdiff GUI would.
    let objdiff_file = crate::codegen::write_objdiff(config_id, &config, &objects);

    let measured = diff_units_parallel(&objdiff_file.units)?;

    let mut all = Vec::new();
    let mut rows = Vec::new();
    for (unit, result) in objdiff_file.units.iter().zip(measured) {
        // A unit with no delinked target has nothing to measure against;
        // objdiff-cli skips it too, since our units are never `complete`.
        let Some((measures, functions)) = result else {
            crate::log::warn(&format!("skipping unit without target: {}", unit.name));
            continue;
        };
        all.push(measures);
        if wanted.is_empty() || wanted.contains(&unit.name) {
            rows.push((unit.name.clone(), measures, functions));
        }
    }

    let total = crate::objdiff::Measures::total(all);
    println!(
        "OVERALL: {}/{} functions, code {}%",
        total.matched_functions,
        total.total_functions,
        paint_pct_text(&format!("{:.4}", total.matched_code_percent), total.matched_code_percent, color),
    );

    for (name, m, functions) in &rows {
        if wanted.is_empty() && m.matched_functions == 0 {
            continue;
        }
        println!(
            "  {name:<20} {:>3}/{:<4}  code {}%",
            m.matched_functions,
            m.total_functions,
            paint_pct_text(&format!("{:>6.2}", m.matched_code_percent), m.matched_code_percent, color),
        );
        if wanted.is_empty() {
            continue;
        }
        for f in functions {
            let done = f.match_percent >= 100.0;
            let mark = paint(if done { "OK " } else { "   " }, GREEN, color && done);
            let pct = paint_pct_text(&format!("{:>6.1}", f.match_percent), f.match_percent, color);
            println!("      {mark} {pct}% {:>5}  {}", f.size, f.name);
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
            branch_to: None,
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
        let out = render_diff(&[foo_view()], Some("?foo@@YAXXZ"), false, false);
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
        let out = render_diff(&[foo_view()], None, false, false);
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
        let line = render_diff(&[view], None, false, false).lines().find(|l| l.contains("ret")).unwrap().to_string();
        assert!(!line.contains("<>"), "unchanged row must not be marked: {line}");
    }

    #[test]
    fn render_diff_all_shows_every_target_function_with_separators() {
        let out = render_diff(&[foo_view(), bar_view()], None, false, false);
        assert_eq!(out.matches("==== ").count(), 2, "one separator per function: {out}");
        assert!(out.contains("==== ?foo@@YAXXZ"), "{out}");
        assert!(out.contains("==== ?bar@@YAXXZ"), "{out}");
        // bar exists only in the target, so OURS has no percentage.
        let bar_header = out.lines().find(|l| l.contains("?bar@@YAXXZ")).unwrap();
        assert!(bar_header.contains("OURS -%"), "bar header: {bar_header}");
    }

    #[test]
    fn render_diff_missing_symbol_renders_dashes() {
        let out = render_diff(&[], Some("?nope@@YAXXZ"), false, false);
        assert!(out.contains("==== ?nope@@YAXXZ"), "{out}");
        assert!(out.contains("TARGET -%"), "{out}");
        assert!(out.contains("OURS -%"), "{out}");
    }

    #[test]
    fn render_diff_all_with_no_target_functions_reports_empty() {
        assert!(render_diff(&[], None, false, false).contains("no functions found"));
    }

    #[test]
    fn render_diff_emits_no_escapes_when_color_disabled() {
        let out = render_diff(&[foo_view()], None, false, false);
        assert!(!out.contains('\x1b'), "plain output must have no escapes: {out:?}");
    }

    #[test]
    fn render_diff_emits_escapes_when_color_enabled() {
        let out = render_diff(&[foo_view()], None, true, false);
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

        let plain = render_diff(&[foo_view()], None, false, false);
        let colored = render_diff(&[foo_view()], None, true, false);
        assert_eq!(strip(&colored), plain, "stripping escapes must recover the plain rendering");
    }

    /// A 100% match reads green, anything less reads red.
    #[test]
    fn percentages_are_painted_by_match_state() {
        let out = render_diff(&[foo_view()], None, true, false);
        assert!(out.contains("\x1b[32m100.00\x1b[0m"), "100% should be green: {out:?}");
        assert!(out.contains("\x1b[31m87.50\x1b[0m"), "sub-100% should be red: {out:?}");
    }

    /// An absent side has no percentage to paint, so `-` stays unstyled.
    #[test]
    fn absent_side_percentage_is_unpainted() {
        let out = render_diff(&[bar_view()], None, true, false);
        assert!(out.contains("OURS -%"), "absent side should render bare dash: {out:?}");
    }

    fn branch_row(text: &str, branch_to: Option<(usize, u32)>) -> crate::objdiff::Row {
        crate::objdiff::Row {
            text: text.to_string(),
            colored: text.to_string(),
            changed: false,
            branch_to,
        }
    }

    /// A backward branch: the destination gets the arrowhead, the source closes
    /// the span, and the rows between carry the vertical.
    #[test]
    fn gutter_draws_backward_branch_from_source_to_destination() {
        let rows = vec![
            branch_row("0: sub", None),
            branch_row("5: mov", None),      // destination
            branch_row("7: or", None),
            branch_row("3a: jl", Some((1, 0))), // source -> row 1
            branch_row("43: add", None),
        ];
        let (g, w) = branch_gutter(&rows, false);
        assert_eq!(w, 4, "one lane plus a two-char head plus a space");
        assert_eq!(g[0], "    ");
        assert_eq!(g[1], ",-> ", "destination takes the arrowhead");
        assert_eq!(g[2], "|   ", "row inside the span carries the vertical");
        assert_eq!(g[3], "`-- ", "source closes the span");
        assert_eq!(g[4], "    ");
    }

    /// A forward branch points down: the source opens the span, the destination
    /// below it takes the arrowhead.
    #[test]
    fn gutter_draws_forward_branch() {
        let rows = vec![
            branch_row("0: je", Some((2, 0))),
            branch_row("2: mov", None),
            branch_row("4: ret", None),
        ];
        let (g, _) = branch_gutter(&rows, false);
        assert_eq!(g[0], ",-- ", "source opens the span");
        assert_eq!(g[1], "|   ");
        assert_eq!(g[2], "`-> ", "destination takes the arrowhead");
    }

    /// Nested branches occupy separate lanes, innermost nearest the text, so a
    /// short branch inside a long one does not overwrite it.
    #[test]
    fn gutter_nests_overlapping_branches_in_separate_lanes() {
        let rows = vec![
            branch_row("0: outer-dest", None),
            branch_row("1: inner-dest", None),
            branch_row("2: body", None),
            branch_row("3: inner-src", Some((1, 1))),
            branch_row("4: outer-src", Some((0, 0))),
        ];
        let (g, w) = branch_gutter(&rows, false);
        assert_eq!(w, 5, "two lanes plus head plus space");
        // Outer branch owns the left lane; inner owns the right.
        assert_eq!(g[0], ",--> ");
        assert_eq!(g[1], "|,-> ");
        assert_eq!(g[2], "||   ");
        assert_eq!(g[3], "|`-- ");
        assert_eq!(g[4], "`--- ");
    }

    /// Disjoint branches reuse one lane rather than widening the gutter.
    #[test]
    fn gutter_reuses_a_lane_for_disjoint_branches() {
        let rows = vec![
            branch_row("0: a-dest", None),
            branch_row("1: a-src", Some((0, 0))),
            branch_row("2: b-dest", None),
            branch_row("3: b-src", Some((2, 1))),
        ];
        let (_, w) = branch_gutter(&rows, false);
        assert_eq!(w, 4, "non-overlapping spans share a lane");
    }

    #[test]
    fn gutter_is_empty_without_branches() {
        let rows = vec![branch_row("0: nop", None), branch_row("1: ret", None)];
        let (g, w) = branch_gutter(&rows, false);
        assert_eq!(w, 0);
        assert!(g.iter().all(|s| s.is_empty()));
    }

    /// A branch whose destination row does not exist is ignored rather than
    /// panicking on the out-of-range index.
    #[test]
    fn gutter_ignores_out_of_range_destination() {
        let rows = vec![branch_row("0: jmp", Some((99, 0)))];
        let (_, w) = branch_gutter(&rows, false);
        assert_eq!(w, 0);
    }

    /// Colored gutters must occupy the same columns as plain ones.
    #[test]
    fn gutter_color_and_plain_have_equal_width() {
        let rows = vec![
            branch_row("0: dest", None),
            branch_row("1: src", Some((0, 3))),
        ];
        let (plain, wp) = branch_gutter(&rows, false);
        let (colored, wc) = branch_gutter(&rows, true);
        assert_eq!(wp, wc);
        for (p, c) in plain.iter().zip(colored.iter()) {
            assert!(c.contains('\x1b'), "colored gutter should carry escapes: {c:?}");
            let stripped: String = {
                let mut out = String::new();
                let mut it = c.chars();
                while let Some(ch) = it.next() {
                    if ch == '\x1b' {
                        for ch in it.by_ref() {
                            if ch == 'm' {
                                break;
                            }
                        }
                    } else {
                        out.push(ch);
                    }
                }
                out
            };
            assert_eq!(&stripped, p);
        }
    }

    /// Separator column, taken from rows that actually have an OURS side.
    fn separator_columns(out: &str) -> Vec<usize> {
        out.lines()
            .filter(|l| l.contains(" |") && !l.starts_with("===="))
            .map(|l| l.find(" |").unwrap())
            .collect()
    }

    /// Regression: an overlong target row used to push the separator right on
    /// that row alone, so the OURS column stopped lining up -- and overlong
    /// rows are typically the changed ones.
    #[test]
    fn overlong_target_row_does_not_shift_the_separator() {
        let long = "17:      fdivr     st, qword ptr [__delink_ida_const_start+0x44]";
        assert!(long.len() > MIN_COL, "fixture must exceed the minimum column");
        let view = crate::objdiff::FnView {
            name: "?wide@@YAXXZ".to_string(),
            size: 8,
            target: Some(side(50.0, vec![row("0: push ebp", false), row(long, true)])),
            base: Some(side(50.0, vec![row("0: push ebp", false), row("1a: fdivr", true)])),
        };
        let out = render_diff(&[view], None, false, false);
        let cols = separator_columns(&out);
        assert!(cols.len() >= 3, "expected header plus two rows: {out}");
        assert_eq!(
            cols.iter().collect::<std::collections::HashSet<_>>().len(),
            1,
            "all separators must share one column, got {cols:?}\n{out}"
        );
    }

    /// Short functions keep the historical 44-column layout.
    #[test]
    fn narrow_function_keeps_minimum_column() {
        let out = render_diff(&[foo_view()], None, false, false);
        for c in separator_columns(&out) {
            assert_eq!(c, MIN_COL, "narrow rows should pad to MIN_COL:\n{out}");
        }
    }

    /// Widening the column must not disturb the color/plain equivalence.
    #[test]
    fn overlong_row_keeps_color_and_plain_aligned() {
        let long = "17:      fdivr     st, qword ptr [__delink_ida_const_start+0x44]";
        let view = || crate::objdiff::FnView {
            name: "?wide@@YAXXZ".to_string(),
            size: 8,
            target: Some(side(50.0, vec![row(long, true)])),
            base: Some(side(50.0, vec![row("1a: fdivr", true)])),
        };
        let plain = render_diff(&[view()], None, false, false);
        let colored = render_diff(&[view()], None, true, false);
        let strip = |s: &str| {
            let mut out = String::new();
            let mut it = s.chars();
            while let Some(c) = it.next() {
                if c == '\x1b' {
                    for c in it.by_ref() {
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
        assert_eq!(strip(&colored), plain);
    }
}

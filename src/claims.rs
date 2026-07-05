use regex::Regex;
use std::process::Command;

fn key_regex() -> Regex {
    Regex::new(r#"^([-+])\s*"([^"]+)":\s*\{\s*$"#).unwrap()
}

/// Reconstructs a unit's claims (original -> claimed) from git history: diffs
/// the split config from the commit that first added it against the working
/// tree, and pairs up adjacent `-"old": {` / `+"new": {` lines. Returns
/// `None` if the split config isn't tracked in git at all.
fn unit_claims(config_id: &str, unit: &str) -> anyhow::Result<Option<Vec<(String, String)>>> {
    let split_rel = format!("config/{config_id}/splits/{unit}.json");

    let add = Command::new("git")
        .args(["log", "--diff-filter=A", "--format=%H", "--", &split_rel])
        .output()
        .map_err(|e| anyhow::anyhow!("running git log: {e}"))?;
    let stdout = String::from_utf8_lossy(&add.stdout).into_owned();
    let commits: Vec<&str> = stdout.split_whitespace().collect();
    let Some(&base) = commits.last() else {
        return Ok(None);
    };

    let diff_out = Command::new("git")
        .args(["diff", base, "--", &split_rel])
        .output()
        .map_err(|e| anyhow::anyhow!("running git diff: {e}"))?;
    let diff = String::from_utf8_lossy(&diff_out.stdout).into_owned();

    let re = key_regex();
    let lines: Vec<&str> = diff.lines().collect();
    let mut pairs = Vec::new();
    for i in 0..lines.len().saturating_sub(1) {
        if let (Some(c1), Some(c2)) = (re.captures(lines[i]), re.captures(lines[i + 1])) {
            if &c1[1] == "-" && &c2[1] == "+" {
                pairs.push((c1[2].to_string(), c2[2].to_string()));
            }
        }
    }
    Ok(Some(pairs))
}

pub fn cmd_claims(config_id: &str, unit_args: &[String]) -> anyhow::Result<()> {
    let objects = crate::model::load_objects(&crate::model::objects_path(config_id))?;
    let units: Vec<String> = if unit_args.is_empty() {
        objects.keys().cloned().collect()
    } else {
        unit_args
            .iter()
            .map(|a| crate::dev::norm_unit(&objects, a))
            .collect::<anyhow::Result<Vec<_>>>()?
    };

    let mut total = 0usize;
    for unit in &units {
        let pairs = unit_claims(config_id, unit)?;
        let Some(pairs) = pairs else {
            if !unit_args.is_empty() {
                println!("{unit}: split config not tracked in git");
            }
            continue;
        };
        if pairs.is_empty() && unit_args.is_empty() {
            continue;
        }
        if pairs.is_empty() {
            println!("{unit}: 0 claim(s)");
        } else {
            println!("{unit}: {} claim(s) (original -> claimed):", pairs.len());
            for (old, new) in &pairs {
                println!("  {old}  ->  {new}");
            }
        }
        total += pairs.len();
    }

    if unit_args.is_empty() {
        println!("total: {total} claim(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::key_regex;

    #[test]
    fn pairs_adjacent_removed_and_added_keys() {
        let diff = "@@ -1,6 +1,6 @@\n {\n   \"x86math.dll.obj\": {\n-    \"sub_6F71DE0\": {\n+    \"_inv_sqrt_dynamic_init\": {\n       \"address\": 116854784,\n";
        let re = key_regex();
        let lines: Vec<&str> = diff.lines().collect();
        let mut pairs = Vec::new();
        for i in 0..lines.len().saturating_sub(1) {
            if let (Some(c1), Some(c2)) = (re.captures(lines[i]), re.captures(lines[i + 1])) {
                if &c1[1] == "-" && &c2[1] == "+" {
                    pairs.push((c1[2].to_string(), c2[2].to_string()));
                }
            }
        }
        assert_eq!(pairs, vec![("sub_6F71DE0".to_string(), "_inv_sqrt_dynamic_init".to_string())]);
    }

    #[test]
    fn ignores_unrelated_lines() {
        let diff = "@@ -1,3 +1,3 @@\n {\n-  \"address\": 1,\n+  \"address\": 2,\n }\n";
        let re = key_regex();
        let lines: Vec<&str> = diff.lines().collect();
        let mut pairs = Vec::new();
        for i in 0..lines.len().saturating_sub(1) {
            if let (Some(c1), Some(c2)) = (re.captures(lines[i]), re.captures(lines[i + 1])) {
                if &c1[1] == "-" && &c2[1] == "+" {
                    pairs.push((c1[2].to_string(), c2[2].to_string()));
                }
            }
        }
        assert!(pairs.is_empty());
    }
}

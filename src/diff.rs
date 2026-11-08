use crate::domain::{ToolError, validate_repo_path};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Diff {
    pub text: String,
    pub truncated: bool,
    pub inline_comments_supported: bool,
}
impl Diff {
    pub fn new(text: String, truncated: bool) -> Self {
        let inline_comments_supported = !truncated && parse(&text, None).unwrap_or(false);
        Self {
            text,
            truncated,
            inline_comments_supported,
        }
    }

    pub fn has_position(&self, path: &str, side: &str, line: u64) -> bool {
        self.inline_comments_supported
            && parse(&self.text, Some(&Location { path, side, line })).unwrap_or(false)
    }
}

struct Location<'a> {
    path: &'a str,
    side: &'a str,
    line: u64,
}
struct Hunk {
    old: u64,
    new: u64,
    old_left: u64,
    new_left: u64,
}

fn parse(text: &str, target: Option<&Location<'_>>) -> Result<bool, ToolError> {
    let mut supported_position = false;
    let mut file_path: Option<String> = None;
    let (mut old_header, mut new_header) = (false, false);
    let mut hunk: Option<Hunk> = None;
    for line in text.lines() {
        if line != "\\ No newline at end of file"
            && hunk
                .as_ref()
                .is_some_and(|active| active.old_left == 0 && active.new_left == 0)
        {
            hunk = None;
        }
        if line.starts_with("diff --git ") {
            if hunk.is_some() {
                return Err(ToolError::Malformed);
            }
            let Some((_, path)) = line.split_once(" b/") else {
                return Err(ToolError::Malformed);
            };
            if path.contains(['"', '\\']) || validate_repo_path(path).is_err() {
                return Err(ToolError::Malformed);
            }
            file_path = Some(path.into());
            old_header = false;
            new_header = false;
            continue;
        }
        if line.starts_with("rename from ")
            || line.starts_with("rename to ")
            || line.starts_with("Binary files ")
            || line.starts_with("GIT binary patch")
        {
            return Err(ToolError::Malformed);
        }
        if let Some(active) = hunk.as_mut() {
            let path = file_path.as_deref().ok_or(ToolError::Malformed)?;
            match line.as_bytes().first() {
                Some(b' ') if active.old_left > 0 && active.new_left > 0 => {
                    supported_position |= matches(target, path, "old", active.old)
                        || matches(target, path, "new", active.new);
                    active.old = active.old.checked_add(1).ok_or(ToolError::Malformed)?;
                    active.new = active.new.checked_add(1).ok_or(ToolError::Malformed)?;
                    active.old_left -= 1;
                    active.new_left -= 1;
                }
                Some(b'-') if active.old_left > 0 => {
                    supported_position |= matches(target, path, "old", active.old);
                    active.old = active.old.checked_add(1).ok_or(ToolError::Malformed)?;
                    active.old_left -= 1;
                }
                Some(b'+') if active.new_left > 0 => {
                    supported_position |= matches(target, path, "new", active.new);
                    active.new = active.new.checked_add(1).ok_or(ToolError::Malformed)?;
                    active.new_left -= 1;
                }
                _ if line == "\\ No newline at end of file" => {}
                _ => return Err(ToolError::Malformed),
            }
            continue;
        }
        if !old_header
            && !new_header
            && (line.starts_with("index ")
                || line.starts_with("old mode ")
                || line.starts_with("new mode ")
                || line.starts_with("new file mode ")
                || line.starts_with("deleted file mode "))
        {
            continue;
        }
        if line.starts_with("--- ") {
            let path = line.strip_prefix("--- a/");
            let is_new_file = line == "--- /dev/null";
            if old_header || file_path.is_none() || (!is_new_file && path != file_path.as_deref()) {
                return Err(ToolError::Malformed);
            }
            old_header = true;
            continue;
        }
        if line.starts_with("+++ ") {
            let path = line.strip_prefix("+++ b/");
            let is_deleted_file = line == "+++ /dev/null";
            if !old_header
                || new_header
                || file_path.is_none()
                || (!is_deleted_file && path != file_path.as_deref())
            {
                return Err(ToolError::Malformed);
            }
            new_header = true;
            continue;
        }
        if line.starts_with("@@ ") {
            if !old_header || !new_header || hunk.is_some() {
                return Err(ToolError::Malformed);
            }
            hunk = Some(parse_hunk(line)?);
            continue;
        }
        if line.is_empty() && file_path.is_none() && text.is_empty() {
            continue;
        }
        return Err(ToolError::Malformed);
    }
    if hunk
        .as_ref()
        .is_some_and(|active| active.old_left != 0 || active.new_left != 0)
    {
        return Err(ToolError::Malformed);
    }
    Ok(supported_position)
}

fn matches(target: Option<&Location<'_>>, path: &str, side: &str, line: u64) -> bool {
    target.is_none_or(|target| target.path == path && target.side == side && target.line == line)
}

fn parse_hunk(line: &str) -> Result<Hunk, ToolError> {
    let Some(spec) = line
        .strip_prefix("@@ ")
        .and_then(|s| s.split_once(" @@").map(|(v, _)| v))
    else {
        return Err(ToolError::Malformed);
    };
    let mut ranges = spec.split_whitespace();
    let Some((old, old_left)) = ranges.next().and_then(|v| parse_range(v, '-')) else {
        return Err(ToolError::Malformed);
    };
    let Some((new, new_left)) = ranges.next().and_then(|v| parse_range(v, '+')) else {
        return Err(ToolError::Malformed);
    };
    if ranges.next().is_some() || (old_left > 0 && old == 0) || (new_left > 0 && new == 0) {
        return Err(ToolError::Malformed);
    }
    Ok(Hunk {
        old,
        new,
        old_left,
        new_left,
    })
}

fn parse_range(value: &str, prefix: char) -> Option<(u64, u64)> {
    let value = value.strip_prefix(prefix)?;
    let (start, count) = value.split_once(',').map_or((value, "1"), |(a, b)| (a, b));
    Some((start.parse().ok()?, count.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_follow_declared_hunk_counts_and_sql_comments_are_removed_lines() {
        let diff = Diff::new("diff --git a/f b/f\nindex abc..def 100644\n--- a/f\n+++ b/f\n@@ -10,1 +20,1 @@\n--- query comment\n+select 1;\n".into(), false);
        assert!(diff.has_position("f", "old", 10));
        assert!(diff.has_position("f", "new", 20));
        assert!(!diff.has_position("f", "old", 11));
        assert!(diff.inline_comments_supported);
        assert!(
            serde_json::to_value(&diff)
                .unwrap()
                .get("positions")
                .is_none()
        );
    }

    #[test]
    fn malformed_counts_overflow_and_unsupported_paths_never_offer_inline_positions() {
        for text in [
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,1 @@\n-a\n+b\n",
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -18446744073709551615,1 +1,1 @@\n-a\n+b\n",
            "diff --git \"a/a b\" \"b/a b\"\n--- a/a b\n+++ b/a b\n@@ -1 +1 @@\n-a\n+b\n",
        ] {
            assert!(!Diff::new(text.into(), false).inline_comments_supported);
        }
        assert!(!Diff::new(String::new(), false).inline_comments_supported);
    }

    #[test]
    fn no_newline_markers_follow_completed_hunks_without_positions() {
        for suffix in [
            "-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n",
            "+new\n\\ No newline at end of file\n-old\n\\ No newline at end of file\n",
        ] {
            let text = format!(
                "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -7,1 +9,1 @@\n{suffix}diff --git a/g b/g\n--- a/g\n+++ b/g\n@@ -2,1 +4,1 @@\n-gone\n+here\n"
            );
            let diff = Diff::new(text, false);
            assert!(diff.inline_comments_supported);
            assert!(diff.has_position("f", "old", 7));
            assert!(diff.has_position("f", "new", 9));
            assert!(diff.has_position("g", "old", 2));
            assert!(diff.has_position("g", "new", 4));
            assert!(!diff.has_position("f", "old", 8));
            assert!(!diff.has_position("f", "new", 10));
        }
        let eof = Diff::new("diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file".into(), false);
        assert!(eof.inline_comments_supported);
        assert!(eof.has_position("f", "old", 1));
        assert!(eof.has_position("f", "new", 1));
    }

    #[test]
    fn hunk_body_cannot_precede_hunk_header() {
        let diff = Diff::new(
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n-removed\n".into(),
            false,
        );
        assert!(!diff.inline_comments_supported);
    }
}

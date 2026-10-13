//! One changed file's diff, parsed into hunks and served a page at a time.

use sha2::{Digest, Sha256};

use crate::cli::serve::contract::{GitDiffHunk, GitDiffLine, GitDiffLineKind};

/// Most raw diff output read for one file. A diff longer than this is called
/// too large to show on a phone rather than read and paged.
pub(crate) const MAX_DIFF_BYTES: usize = 4 * 1024 * 1024;

/// Serialized size of the hunks one page carries. Together with the rest of
/// the response this stays far under the phone's 512 KiB limit.
pub(crate) const DIFF_PAGE_BUDGET: usize = 192 * 1024;

/// Longest single line sent. A minified file can be one multi-megabyte line;
/// past this it is cut and marked.
pub(crate) const MAX_LINE_BYTES: usize = 2048;

const MAX_CURSOR_BYTES: usize = 160;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ParsedDiff {
    pub binary: bool,
    pub notes: Vec<String>,
    pub hunks: Vec<GitDiffHunk>,
    pub additions: u64,
    pub deletions: u64,
}

/// Parses unified diff output for one file.
pub(crate) fn parse(output: &[u8]) -> ParsedDiff {
    let mut parsed = ParsedDiff::default();
    let mut current: Option<GitDiffHunk> = None;
    let (mut old_line, mut new_line) = (0u32, 0u32);
    let (mut old_left, mut new_left) = (0u32, 0u32);
    let lines = output.strip_suffix(b"\n").unwrap_or(output);
    for line in lines.split(|byte| *byte == b'\n') {
        let in_body = current.is_some() && (old_left > 0 || new_left > 0);
        if in_body {
            let hunk = current.as_mut().expect("in a hunk");
            let (kind, old, new) = match line.first() {
                Some(b'+') => {
                    new_left = new_left.saturating_sub(1);
                    new_line += 1;
                    parsed.additions += 1;
                    (GitDiffLineKind::Added, None, Some(new_line))
                }
                Some(b'-') => {
                    old_left = old_left.saturating_sub(1);
                    old_line += 1;
                    parsed.deletions += 1;
                    (GitDiffLineKind::Removed, Some(old_line), None)
                }
                Some(b'\\') => (GitDiffLineKind::Note, None, None),
                // Context, including a context line that is itself empty.
                _ => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                    old_line += 1;
                    new_line += 1;
                    (GitDiffLineKind::Context, Some(old_line), Some(new_line))
                }
            };
            let body = if kind == GitDiffLineKind::Note {
                line
            } else {
                line.get(1..).unwrap_or_default()
            };
            hunk.lines.push(diff_line(kind, old, new, body));
            continue;
        }
        if line.starts_with(b"\\") {
            // "\ No newline at end of file" after the hunk's last line.
            if let Some(hunk) = current.as_mut() {
                hunk.lines
                    .push(diff_line(GitDiffLineKind::Note, None, None, line));
            }
            continue;
        }
        if let Some(header) = hunk_header(line) {
            if let Some(hunk) = current.take() {
                parsed.hunks.push(hunk);
            }
            old_line = header.old_start.saturating_sub(1);
            new_line = header.new_start.saturating_sub(1);
            old_left = header.old_lines;
            new_left = header.new_lines;
            current = Some(GitDiffHunk {
                index: parsed.hunks.len() as u32,
                header: String::from_utf8_lossy(line).into_owned(),
                old_start: header.old_start,
                old_lines: header.old_lines,
                new_start: header.new_start,
                new_lines: header.new_lines,
                lines: Vec::new(),
                omitted_lines: 0,
            });
            continue;
        }
        // File header territory.
        if line.starts_with(b"Binary files ") || line == b"GIT binary patch" {
            parsed.binary = true;
            continue;
        }
        for prefix in [
            "old mode ",
            "new mode ",
            "new file mode ",
            "deleted file mode ",
            "similarity index ",
            "copy from ",
            "copy to ",
            "rename from ",
            "rename to ",
        ] {
            if line.starts_with(prefix.as_bytes()) {
                parsed
                    .notes
                    .push(String::from_utf8_lossy(line).into_owned());
                break;
            }
        }
    }
    if let Some(hunk) = current.take() {
        parsed.hunks.push(hunk);
    }
    parsed
}

struct HunkHeader {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
}

/// `@@ -a[,b] +c[,d] @@ optional section`
fn hunk_header(line: &[u8]) -> Option<HunkHeader> {
    let rest = line.strip_prefix(b"@@ -")?;
    let text = std::str::from_utf8(&rest[..rest.windows(3).position(|w| w == b" @@")?]).ok()?;
    let (old, new) = text.split_once(" +")?;
    let range = |part: &str| -> Option<(u32, u32)> {
        match part.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((part.parse().ok()?, 1)),
        }
    };
    let (old_start, old_lines) = range(old)?;
    let (new_start, new_lines) = range(new)?;
    Some(HunkHeader {
        old_start,
        old_lines,
        new_start,
        new_lines,
    })
}

fn diff_line(
    kind: GitDiffLineKind,
    old: Option<u32>,
    new: Option<u32>,
    body: &[u8],
) -> GitDiffLine {
    let truncated = body.len() > MAX_LINE_BYTES;
    let body = if truncated {
        &body[..MAX_LINE_BYTES]
    } else {
        body
    };
    let mut text = String::from_utf8_lossy(body).into_owned();
    if truncated {
        // A cut can land inside a multi-byte character; lossy decoding has
        // already turned that fragment into U+FFFD, so nothing invalid is sent.
        text.push('…');
    }
    GitDiffLine {
        kind,
        old_line: old,
        new_line: new,
        text,
        truncated,
    }
}

/// Short fingerprint of the diff a cursor was issued against. A cursor that
/// arrives after the file changed names hunks that no longer exist.
pub(crate) fn fingerprint(output: &[u8]) -> String {
    let digest = Sha256::digest(output);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn cursor(fingerprint: &str, index: usize) -> String {
    format!("v1-{fingerprint}-{index}")
}

/// The hunk index a cursor resumes at, or `None` when it is stale or forged.
pub(crate) fn parse_cursor(cursor: &str, fingerprint: &str, hunks: usize) -> Option<usize> {
    if cursor.len() > MAX_CURSOR_BYTES {
        return None;
    }
    let (observed, index) = cursor.strip_prefix("v1-")?.rsplit_once('-')?;
    let index = index.parse::<usize>().ok()?;
    (observed == fingerprint && index > 0 && index < hunks).then_some(index)
}

/// Takes hunks from `start` while they fit the page budget. The first hunk is
/// always included, cut to the budget if it alone exceeds it, so a page never
/// comes back empty while hunks remain.
pub(crate) fn page(hunks: &[GitDiffHunk], start: usize) -> (Vec<GitDiffHunk>, Option<usize>) {
    let mut page = Vec::new();
    let mut used = 0usize;
    let mut index = start;
    while index < hunks.len() {
        let hunk = &hunks[index];
        let size = serialized_len(hunk);
        if used + size > DIFF_PAGE_BUDGET {
            if page.is_empty() {
                page.push(cut(hunk, DIFF_PAGE_BUDGET));
                index += 1;
            }
            break;
        }
        used += size;
        page.push(hunk.clone());
        index += 1;
    }
    let next = (index < hunks.len()).then_some(index);
    (page, next)
}

fn cut(hunk: &GitDiffHunk, budget: usize) -> GitDiffHunk {
    let mut kept = GitDiffHunk {
        lines: Vec::new(),
        ..hunk.clone()
    };
    let mut used = serialized_len(&kept);
    for line in &hunk.lines {
        let size = serialized_len(line) + 1;
        if used + size > budget {
            break;
        }
        used += size;
        kept.lines.push(line.clone());
    }
    kept.omitted_lines = (hunk.lines.len() - kept.lines.len()) as u32;
    kept
}

fn serialized_len<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX / 2, |bytes| bytes.len() + 1)
}

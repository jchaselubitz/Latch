//! `git status --porcelain=v2 -z --branch`, parsed as data.
//!
//! Paths stay bytes until the very end: a name may contain spaces, newlines,
//! quotes, a leading dash, or bytes that are not UTF-8, and the phone must be
//! able to send back exactly the file it was shown. Each entry therefore
//! carries its bytes hex-encoded as `pathKey` beside a display string.

use std::collections::HashMap;

use crate::cli::serve::contract::{
    GitChangeArea, GitChangeKind, GitHead, GitOperation, GitStatusEntry, GitUpstream,
};

/// Most entries one status response carries. Past this the phone is told how
/// many there were and that the list is cut, rather than sent a list it
/// cannot draw.
pub(crate) const MAX_STATUS_ENTRIES: usize = 2000;

/// Serialized size of the entries one response may carry. With the header
/// fields and sessions this keeps a status far below the phone's 512 KiB
/// response limit however long the paths are.
pub(crate) const STATUS_ENTRY_BUDGET: usize = 320 * 1024;

/// One porcelain record, before it is split into per-area entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub path: Vec<u8>,
    pub orig_path: Option<Vec<u8>>,
    pub kind: RecordKind,
    /// Git's `<sub>` field said this path is a submodule.
    pub submodule: bool,
    /// The record's space-separated fields before the path (type, `XY`,
    /// `<sub>`, modes, object ids, score), kept for fingerprints.
    pub fields: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecordKind {
    /// Ordinary or renamed/copied: index (`X`) and worktree (`Y`) letters.
    Changed {
        x: u8,
        y: u8,
    },
    Unmerged {
        xy: [u8; 2],
    },
    Untracked,
    Ignored,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Branch {
    pub oid: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead_behind: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Porcelain {
    pub branch: Branch,
    pub records: Vec<Record>,
}

/// Parses porcelain v2 `-z` output. A record that does not parse is skipped
/// rather than failing the whole status: one odd line from a future Git must
/// not hide every other change. `complete` is false when the output was cut
/// at the read limit, in which case the trailing partial record is dropped.
pub(crate) fn parse(output: &[u8], complete: bool) -> Porcelain {
    let mut fields = output.split(|byte| *byte == 0).peekable();
    let mut porcelain = Porcelain::default();
    while let Some(field) = fields.next() {
        // The final split after a trailing NUL is empty; so is the last field
        // of a truncated stream, which is never trusted.
        if fields.peek().is_none() && (field.is_empty() || !complete) {
            break;
        }
        match field.first() {
            Some(b'#') => parse_header(field, &mut porcelain.branch),
            Some(b'1') => {
                if let Some(record) = ordinary(field) {
                    porcelain.records.push(record);
                }
            }
            Some(b'2') => {
                // A rename or copy carries its original path as the next field.
                let Some(original) = fields.next() else { break };
                if fields.peek().is_none() && !complete {
                    break;
                }
                if let Some(mut record) = renamed(field) {
                    record.orig_path = Some(original.to_vec());
                    porcelain.records.push(record);
                }
            }
            Some(b'u') => {
                if let Some(record) = unmerged(field) {
                    porcelain.records.push(record);
                }
            }
            Some(b'?') => {
                if let Some(path) = field.strip_prefix(b"? ") {
                    porcelain.records.push(Record {
                        path: path.to_vec(),
                        orig_path: None,
                        kind: RecordKind::Untracked,
                        submodule: false,
                        fields: Vec::new(),
                    });
                }
            }
            Some(b'!') => {
                if let Some(path) = field.strip_prefix(b"! ") {
                    porcelain.records.push(Record {
                        path: path.to_vec(),
                        orig_path: None,
                        kind: RecordKind::Ignored,
                        submodule: false,
                        fields: Vec::new(),
                    });
                }
            }
            _ => {}
        }
    }
    porcelain
}

fn parse_header(field: &[u8], branch: &mut Branch) {
    let text = String::from_utf8_lossy(field);
    let Some(rest) = text.strip_prefix("# ") else {
        return;
    };
    let Some((key, value)) = rest.split_once(' ') else {
        return;
    };
    match key {
        "branch.oid" => {
            branch.oid = (value != "(initial)" && is_object_id(value)).then(|| value.to_owned())
        }
        "branch.head" => branch.head = (value != "(detached)").then(|| value.to_owned()),
        "branch.upstream" => branch.upstream = Some(value.to_owned()),
        "branch.ab" => {
            let mut parts = value.split(' ');
            let ahead = parts
                .next()
                .and_then(|part| part.strip_prefix('+'))
                .and_then(|part| part.parse().ok());
            let behind = parts
                .next()
                .and_then(|part| part.strip_prefix('-'))
                .and_then(|part| part.parse().ok());
            if let (Some(ahead), Some(behind)) = (ahead, behind) {
                branch.ahead_behind = Some((ahead, behind));
            }
        }
        _ => {}
    }
}

fn is_object_id(value: &str) -> bool {
    (40..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Splits off `count` space-separated fields and returns them with the rest,
/// which is the path and may itself contain spaces.
fn fields(record: &[u8], count: usize) -> Option<(Vec<&[u8]>, &[u8])> {
    let mut rest = record;
    let mut taken = Vec::with_capacity(count);
    for _ in 0..count {
        let space = rest.iter().position(|byte| *byte == b' ')?;
        taken.push(&rest[..space]);
        rest = &rest[space + 1..];
    }
    (!rest.is_empty()).then_some((taken, rest))
}

fn xy(field: &[u8]) -> Option<(u8, u8)> {
    match field {
        [x, y] => Some((*x, *y)),
        _ => None,
    }
}

/// `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>`
fn ordinary(record: &[u8]) -> Option<Record> {
    let (taken, path) = fields(record, 8)?;
    let (x, y) = xy(taken[1])?;
    Some(Record {
        path: path.to_vec(),
        orig_path: None,
        kind: RecordKind::Changed { x, y },
        submodule: taken[2].first() == Some(&b'S'),
        fields: taken.iter().map(|field| field.to_vec()).collect(),
    })
}

/// `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>`
fn renamed(record: &[u8]) -> Option<Record> {
    let (taken, path) = fields(record, 9)?;
    let (x, y) = xy(taken[1])?;
    Some(Record {
        path: path.to_vec(),
        orig_path: None,
        kind: RecordKind::Changed { x, y },
        submodule: taken[2].first() == Some(&b'S'),
        fields: taken.iter().map(|field| field.to_vec()).collect(),
    })
}

/// `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>`
fn unmerged(record: &[u8]) -> Option<Record> {
    let (taken, path) = fields(record, 10)?;
    let (x, y) = xy(taken[1])?;
    Some(Record {
        path: path.to_vec(),
        orig_path: None,
        kind: RecordKind::Unmerged { xy: [x, y] },
        submodule: taken[2].first() == Some(&b'S'),
        fields: taken.iter().map(|field| field.to_vec()).collect(),
    })
}

/// Line counts for one path from `git diff --numstat -z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Counts {
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub binary: bool,
}

/// Parses `--numstat -z`: `<add>\t<del>\t<path>\0`, or for a rename
/// `<add>\t<del>\t\0<from>\0<to>\0`. Keyed by the (new) path's bytes.
pub(crate) fn parse_numstat(output: &[u8]) -> HashMap<Vec<u8>, Counts> {
    let mut counts = HashMap::new();
    let mut fields = output.split(|byte| *byte == 0);
    while let Some(field) = fields.next() {
        let mut parts = field.splitn(3, |byte| *byte == b'\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            let _from = fields.next();
            match fields.next() {
                Some(to) => to,
                None => break,
            }
        } else {
            path
        };
        let binary = added == b"-" && deleted == b"-";
        let number = |bytes: &[u8]| std::str::from_utf8(bytes).ok()?.parse().ok();
        counts.insert(
            path.to_vec(),
            Counts {
                additions: number(added),
                deletions: number(deleted),
                binary,
            },
        );
    }
    counts
}

/// Splits records into the per-area entries the phone lists.
pub(crate) fn entries(
    records: &[Record],
    staged: &HashMap<Vec<u8>, Counts>,
    unstaged: &HashMap<Vec<u8>, Counts>,
) -> Vec<GitStatusEntry> {
    let mut entries = Vec::new();
    for record in records {
        match &record.kind {
            RecordKind::Changed { x, y } => {
                if let Some(change) = change_kind(*x) {
                    entries.push(entry(record, GitChangeArea::Staged, change, staged, true));
                }
                if let Some(change) = change_kind(*y) {
                    entries.push(entry(
                        record,
                        GitChangeArea::Unstaged,
                        change,
                        unstaged,
                        false,
                    ));
                }
            }
            RecordKind::Unmerged { xy } => {
                let mut conflicted = entry(
                    record,
                    GitChangeArea::Conflicted,
                    GitChangeKind::Conflicted,
                    unstaged,
                    false,
                );
                conflicted.conflict = Some(String::from_utf8_lossy(xy).into_owned());
                entries.push(conflicted);
            }
            RecordKind::Untracked => entries.push(entry(
                record,
                GitChangeArea::Untracked,
                GitChangeKind::Untracked,
                unstaged,
                false,
            )),
            RecordKind::Ignored => {}
        }
    }
    entries
}

pub(crate) fn change_kind(letter: u8) -> Option<GitChangeKind> {
    match letter {
        b'M' => Some(GitChangeKind::Modified),
        b'A' => Some(GitChangeKind::Added),
        b'D' => Some(GitChangeKind::Deleted),
        b'R' => Some(GitChangeKind::Renamed),
        b'C' => Some(GitChangeKind::Copied),
        b'T' => Some(GitChangeKind::TypeChanged),
        _ => None,
    }
}

fn entry(
    record: &Record,
    area: GitChangeArea,
    change: GitChangeKind,
    counts: &HashMap<Vec<u8>, Counts>,
    keeps_original: bool,
) -> GitStatusEntry {
    let counts = counts.get(&record.path);
    // The original path belongs to the index side of a rename; the worktree
    // side of the same record is a change to the new path only.
    let orig = keeps_original
        .then_some(record.orig_path.as_deref())
        .flatten();
    GitStatusEntry {
        path: display(&record.path),
        path_key: hex(&record.path),
        orig_path: orig.map(display),
        orig_path_key: orig.map(hex),
        area,
        change,
        conflict: None,
        submodule: record.submodule,
        additions: counts.and_then(|counts| counts.additions),
        deletions: counts.and_then(|counts| counts.deletions),
        binary: counts.is_some_and(|counts| counts.binary),
        // Stamped by `snapshot`, which also reads the file on disk.
        fingerprint: String::new(),
    }
}

/// Keeps entries in order until the count or byte budget is reached.
/// Returns whether anything was cut.
pub(crate) fn bound(entries: &mut Vec<GitStatusEntry>) -> bool {
    let mut used = 0usize;
    let mut keep = 0usize;
    for entry in entries.iter() {
        let size = serde_json::to_vec(entry).map_or(usize::MAX, |bytes| bytes.len() + 1);
        if keep == MAX_STATUS_ENTRIES || used.saturating_add(size) > STATUS_ENTRY_BUDGET {
            break;
        }
        used += size;
        keep += 1;
    }
    let cut = keep < entries.len();
    entries.truncate(keep);
    cut
}

pub(crate) fn head(branch: &Branch) -> GitHead {
    GitHead {
        branch: branch.head.clone(),
        oid: branch.oid.clone(),
        detached: branch.head.is_none(),
    }
}

pub(crate) fn upstream(branch: &Branch) -> Option<GitUpstream> {
    let name = branch.upstream.clone()?;
    Some(GitUpstream {
        name,
        ahead: branch.ahead_behind.map(|(ahead, _)| ahead),
        behind: branch.ahead_behind.map(|(_, behind)| behind),
    })
}

/// A merge, rebase, cherry-pick, or revert that stopped part-way, read from
/// the marker files Git leaves in the worktree's own git directory.
pub(crate) fn operation(git_dir: &std::path::Path) -> Option<GitOperation> {
    use crate::cli::serve::contract::GitOperationKind;
    let exists = |name: &str| git_dir.join(name).exists();
    let first_line = |name: &str| {
        let text = std::fs::read(git_dir.join(name)).ok()?;
        let line = text.split(|byte| *byte == b'\n').next()?;
        let line = String::from_utf8_lossy(line).trim().to_owned();
        (!line.is_empty()).then_some(line)
    };
    let number = |name: &str| first_line(name).and_then(|text| text.parse::<u32>().ok());
    let oid = |name: &str| {
        first_line(name).filter(|text| {
            (4..=64).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    };
    let branch = |name: &str| {
        first_line(name)
            .and_then(|text| text.strip_prefix("refs/heads/").map(str::to_owned))
            .filter(|text| text.len() <= 1024)
    };
    let empty = |kind| GitOperation {
        kind,
        step: None,
        total: None,
        head_name: None,
        onto: None,
        stopped_at: None,
        merge_head: None,
        summary: None,
    };
    for (dir, step, total) in [
        ("rebase-merge", "msgnum", "end"),
        ("rebase-apply", "next", "last"),
    ] {
        if exists(dir) {
            return Some(GitOperation {
                step: number(&format!("{dir}/{step}")),
                total: number(&format!("{dir}/{total}")),
                head_name: branch(&format!("{dir}/head-name")),
                onto: oid(&format!("{dir}/onto")),
                stopped_at: oid(&format!("{dir}/stopped-sha")),
                ..empty(GitOperationKind::Rebase)
            });
        }
    }
    if exists("MERGE_HEAD") {
        return Some(GitOperation {
            merge_head: oid("MERGE_HEAD"),
            summary: first_line("MERGE_MSG").map(|text| text.chars().take(200).collect()),
            ..empty(GitOperationKind::Merge)
        });
    }
    if exists("CHERRY_PICK_HEAD") {
        return Some(empty(GitOperationKind::CherryPick));
    }
    if exists("REVERT_HEAD") {
        return Some(empty(GitOperationKind::Revert));
    }
    None
}

/// Bytes shown as text: valid UTF-8 as itself, anything else with U+FFFD.
pub(crate) fn display(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    text
}

pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || text.len() % 2 != 0 {
        return None;
    }
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    text.as_bytes()
        .chunks(2)
        .map(|pair| Some(digit(pair[0])? << 4 | digit(pair[1])?))
        .collect()
}

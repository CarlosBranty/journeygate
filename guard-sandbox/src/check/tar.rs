//! A minimal tar writer and reader for check sandboxes: the candidate tree
//! goes in as a tar on `docker exec -i ... tar -x` stdin, and the results
//! (screenshots) come out as a tar on `tar -c` stdout.
//!
//! The writer emits POSIX ustar with a PAX `path` / `linkpath` record for
//! names that do not fit. The reader understands ustar, PAX (`x`) and GNU
//! long names (`L`/`K`, GNU tar's default `-c` format); it returns raw entry
//! paths, so callers must pass every path through [`safe_relative`] before
//! writing it anywhere (the archive comes out of an untrusted container).

use std::path::PathBuf;

const BLOCK: usize = 512;

/// One entry of [`write`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A regular file (`mode` = permission bits, e.g. `0o644`).
    File {
        path: String,
        mode: u32,
        bytes: Vec<u8>,
    },
    Symlink {
        path: String,
        target: String,
    },
}

/// A regular file read back by [`read`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub mode: u32,
    pub bytes: Vec<u8>,
}

fn octal(field: &mut [u8], value: u64) -> Result<(), String> {
    let digits = field.len() - 1;
    let s = format!("{value:0digits$o}");
    if s.len() > digits {
        return Err(format!("{value} does not fit a {digits}-digit tar field"));
    }
    field[..digits].copy_from_slice(s.as_bytes());
    field[digits] = 0;
    Ok(())
}

/// One PAX record: `"<len> <key>=<value>\n"`, where `<len>` counts itself.
fn pax_record(key: &str, value: &str) -> Vec<u8> {
    let rest = key.len() + value.len() + 3; // ' ', '=', '\n'
    let mut len = rest + 1;
    while len != rest + len.to_string().len() {
        len = rest + len.to_string().len();
    }
    format!("{len} {key}={value}\n").into_bytes()
}

/// `(prefix, name)` of a ustar header for `path`, or `None` when it needs PAX.
fn split_ustar(path: &str) -> Option<(&str, &str)> {
    if path.len() <= 100 {
        return Some(("", path));
    }
    // The prefix ends at a '/', is at most 155 bytes, the rest at most 100.
    path.char_indices()
        .filter(|(_, c)| *c == '/')
        .map(|(i, _)| i)
        .find(|&i| i <= 155 && path.len() - i - 1 <= 100 && path.len() - i > 1)
        .map(|i| (&path[..i], &path[i + 1..]))
}

fn header(
    name: &str,
    prefix: &str,
    mode: u32,
    size: u64,
    kind: u8,
    link: &str,
) -> Result<[u8; BLOCK], String> {
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name.as_bytes());
    octal(&mut h[100..108], u64::from(mode))?;
    octal(&mut h[108..116], 1000)?;
    octal(&mut h[116..124], 1000)?;
    octal(&mut h[124..136], size)?;
    octal(&mut h[136..148], 0)?;
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    h[148..156].copy_from_slice(b"        ");
    let sum: u64 = h.iter().map(|b| u64::from(*b)).sum();
    let s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(s.as_bytes());
    Ok(h)
}

fn push_padded(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes);
    let pad = (BLOCK - bytes.len() % BLOCK) % BLOCK;
    out.extend(std::iter::repeat_n(0u8, pad));
}

/// A POSIX relative path of plain components (the container side; any
/// byte but `/` and NUL is fine in a component, `:` and `\` included).
fn check_path(path: &str) -> Result<(), String> {
    let clean = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\0')
        && path.split('/').all(|c| !matches!(c, "" | "." | ".."));
    if clean {
        Ok(())
    } else {
        Err(format!("tar path {path:?} is not a clean relative path"))
    }
}

/// A tar archive of `entries` (owner 1000:1000, mtime 0).
pub fn write(entries: &[Entry]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for e in entries {
        let (path, mode, size, kind, link, data): (&str, u32, u64, u8, &str, &[u8]) = match e {
            Entry::File { path, mode, bytes } => {
                (path, *mode & 0o7777, bytes.len() as u64, b'0', "", bytes)
            }
            Entry::Symlink { path, target } => (path, 0o777, 0, b'2', target.as_str(), &[]),
        };
        check_path(path)?;
        if link.contains('\0') {
            return Err(format!("symlink target of {path:?} contains NUL"));
        }
        let ustar = split_ustar(path);
        let link_fits = link.len() <= 100;
        if ustar.is_none() || !link_fits {
            let mut pax = Vec::new();
            if ustar.is_none() {
                pax.extend(pax_record("path", path));
            }
            if !link_fits {
                pax.extend(pax_record("linkpath", link));
            }
            out.extend_from_slice(&header("PaxHeader", "", 0o644, pax.len() as u64, b'x', "")?);
            push_padded(&mut out, &pax);
        }
        let (prefix, name) = ustar.unwrap_or_else(|| {
            // The last <= 100 bytes, cut at a character boundary.
            let mut start = path.len().saturating_sub(100);
            while !path.is_char_boundary(start) {
                start += 1;
            }
            ("", &path[start..])
        });
        let name = if ustar.is_none() {
            // Only a fallback for readers without PAX: ASCII-safe tail.
            name.trim_start_matches(|c: char| !c.is_ascii())
        } else {
            name
        };
        let link = if link_fits { link } else { "" };
        out.extend_from_slice(&header(name, prefix, mode, size, kind, link)?);
        push_padded(&mut out, data);
    }
    out.extend(std::iter::repeat_n(0u8, BLOCK * 2));
    Ok(out)
}

fn field_str(b: &[u8]) -> String {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

fn parse_octal(b: &[u8]) -> Result<u64, String> {
    if b.first().is_some_and(|c| c & 0x80 != 0) {
        return Err("base-256 tar sizes are not supported".into());
    }
    let s = field_str(b);
    let s = s.trim_matches(|c: char| c == ' ' || c == '\0');
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|e| format!("bad tar number {s:?}: {e}"))
}

fn parse_pax(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let Some(sp) = rest.iter().position(|c| *c == b' ') else {
            break;
        };
        let Ok(len) = std::str::from_utf8(&rest[..sp])
            .unwrap_or("")
            .parse::<usize>()
        else {
            break;
        };
        if len <= sp || len > rest.len() {
            break;
        }
        let record = &rest[sp + 1..len];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(eq) = record.iter().position(|c| *c == b'=') {
            out.push((
                String::from_utf8_lossy(&record[..eq]).into_owned(),
                String::from_utf8_lossy(&record[eq + 1..]).into_owned(),
            ));
        }
        rest = &rest[len..];
    }
    out
}

/// The regular files of a tar archive (directories, links and devices are
/// skipped). Fails when the archive is malformed or its regular files hold
/// more than `max_total` bytes.
pub fn read(archive: &[u8], max_total: usize) -> Result<Vec<File>, String> {
    let mut files = Vec::new();
    let mut total = 0usize;
    let mut at = 0usize;
    let mut long_name: Option<String> = None;
    while at + BLOCK <= archive.len() {
        let h = &archive[at..at + BLOCK];
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let size = usize::try_from(parse_octal(&h[124..136])?)
            .map_err(|_| "tar entry too large".to_string())?;
        let data_start = at + BLOCK;
        let data_end = data_start
            .checked_add(size)
            .filter(|end| *end <= archive.len())
            .ok_or_else(|| "truncated tar entry".to_string())?;
        let data = &archive[data_start..data_end];
        at = data_start + size.div_ceil(BLOCK) * BLOCK;
        match h[156] {
            b'x' => {
                for (k, v) in parse_pax(data) {
                    if k == "path" {
                        long_name = Some(v);
                    }
                }
            }
            b'L' => long_name = Some(field_str(data)),
            b'g' | b'K' => {}
            b'0' | 0 | b'7' => {
                let path = long_name.take().unwrap_or_else(|| {
                    let prefix = field_str(&h[345..500]);
                    let name = field_str(&h[..100]);
                    if prefix.is_empty() {
                        name
                    } else {
                        format!("{prefix}/{name}")
                    }
                });
                total = total.saturating_add(size);
                if total > max_total {
                    return Err(format!("tar holds more than {max_total} bytes"));
                }
                files.push(File {
                    path,
                    mode: u32::try_from(parse_octal(&h[100..108])?).unwrap_or(0o644) & 0o7777,
                    bytes: data.to_vec(),
                });
            }
            _ => {
                long_name = None;
            }
        }
    }
    Ok(files)
}

/// `path` as a relative path of plain components (no `..`, no root, no
/// drive, no backslash), or `None`.
pub fn safe_relative(path: &str) -> Option<PathBuf> {
    let path = path.strip_prefix("./").unwrap_or(path);
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', '\0', ':']) {
        return None;
    }
    let mut out = PathBuf::new();
    for part in path.split('/') {
        match part {
            "" | "." | ".." => return None,
            p => out.push(p),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, bytes: &[u8]) -> Entry {
        Entry::File {
            path: path.into(),
            mode: 0o644,
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn files_round_trip_with_modes() {
        let tar = write(&[
            file("a.txt", b"hello"),
            Entry::File {
                path: "bin/run.sh".into(),
                mode: 0o755,
                bytes: b"#!/bin/sh\n".to_vec(),
            },
            file("empty", b""),
        ])
        .unwrap();
        assert_eq!(tar.len() % BLOCK, 0);
        let files = read(&tar, 1 << 20).unwrap();
        let got: Vec<(&str, u32, &[u8])> = files
            .iter()
            .map(|f| (f.path.as_str(), f.mode, f.bytes.as_slice()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a.txt", 0o644, b"hello".as_slice()),
                ("bin/run.sh", 0o755, b"#!/bin/sh\n".as_slice()),
                ("empty", 0o644, b"".as_slice()),
            ]
        );
    }

    #[test]
    fn long_paths_use_the_ustar_prefix_or_pax() {
        let mid = format!("{}/{}.ts", "d".repeat(120), "f".repeat(60));
        let huge = format!("{}/{}.ts", "x".repeat(200), "y".repeat(150));
        let tar = write(&[file(&mid, b"1"), file(&huge, b"2")]).unwrap();
        let files = read(&tar, 1 << 20).unwrap();
        assert_eq!(files[0].path, mid);
        assert_eq!(files[1].path, huge);
        assert_eq!(files[1].bytes, b"2");
    }

    #[test]
    fn long_multibyte_paths_never_split_inside_a_character() {
        // Every byte offset relative to `len - 100` falls inside a 3-byte
        // character for one of these, both with a ustar split and with PAX.
        for pad in 0..3 {
            let split = format!("{}/{}.md", "日".repeat(30), "a".repeat(50 + pad));
            assert_eq!(split_ustar(&split).map(|(p, _)| p.len()), Some(90));
            let pax = format!("{}{}", "x".repeat(pad), "語".repeat(90));
            assert!(split.len() > 100 && pax.len() > 100);
            let tar = write(&[file(&split, b"1"), file(&pax, b"2")]).unwrap();
            let files = read(&tar, 1 << 20).unwrap();
            assert_eq!(files[0].path, split);
            assert_eq!(files[1].path, pax);
        }
    }

    #[test]
    fn symlinks_are_written_and_skipped_on_read() {
        let tar = write(&[
            Entry::Symlink {
                path: "link".into(),
                target: "/etc/passwd".into(),
            },
            file("real", b"r"),
        ])
        .unwrap();
        assert_eq!(tar[156], b'2');
        let files = read(&tar, 1 << 20).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "real");
    }

    #[test]
    fn writer_refuses_unclean_paths() {
        for bad in ["/abs", "../up", "a/../b", "a//b", "./a", ""] {
            assert!(write(&[file(bad, b"x")]).is_err(), "{bad:?}");
        }
        // Legal POSIX names (a git path may hold them) are kept as they are.
        assert!(write(&[file("c:/x", b"x"), file("a\\b", b"y")]).is_ok());
    }

    #[test]
    fn reader_caps_the_total_size_and_rejects_truncation() {
        let tar = write(&[file("a", &[1u8; 1000]), file("b", &[2u8; 1000])]).unwrap();
        assert!(read(&tar, 1500).unwrap_err().contains("more than"));
        assert!(read(&tar[..BLOCK + 10], 1 << 20).is_err());
    }

    #[test]
    fn reader_understands_gnu_long_names() {
        // GNU tar -c: an 'L' entry named ././@LongLink carries the next name.
        let long = format!("test-results/{}/shot.png", "n".repeat(150));
        let mut tar = Vec::new();
        let mut name = long.clone().into_bytes();
        name.push(0);
        tar.extend_from_slice(
            &header("././@LongLink", "", 0o644, name.len() as u64, b'L', "").unwrap(),
        );
        push_padded(&mut tar, &name);
        tar.extend_from_slice(&header(&long[..100], "", 0o644, 3, b'0', "").unwrap());
        push_padded(&mut tar, b"png");
        tar.extend(std::iter::repeat_n(0u8, BLOCK * 2));
        let files = read(&tar, 1 << 20).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, long);
        assert_eq!(files[0].bytes, b"png");
    }

    #[test]
    fn safe_relative_accepts_only_plain_components() {
        assert_eq!(
            safe_relative("test-results/a/b.png"),
            Some(PathBuf::from("test-results").join("a").join("b.png"))
        );
        assert_eq!(safe_relative("./x"), Some(PathBuf::from("x")));
        for bad in [
            "/etc/x",
            "../x",
            "a/../../x",
            "C:/x",
            "a\\..\\b",
            "",
            "a//b",
        ] {
            assert_eq!(safe_relative(bad), None, "{bad:?}");
        }
    }
}

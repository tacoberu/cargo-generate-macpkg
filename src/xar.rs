//! Writer of the `xar` archive that a flat macOS package (`.pkg`) is.
//!
//! Layout (all numbers big endian): a 28 byte header, the zlib compressed table of contents
//! (XML) and the heap. The heap starts with the SHA-1 of the compressed table of contents,
//! followed by the data of the files one after another; the table of contents gives the offset
//! and the checksums of each file. Files are stored without compression (they are already
//! `gzip` archives, or small).

use crate::error::Error;
use flate2::{Compression, write::ZlibEncoder};
use sha1::{Digest, Sha1};
use std::io::Write;

const MAGIC: u32 = 0x7861_7221; // "xar!"
const HEADER_SIZE: u16 = 28;
const SHA1_LEN: usize = 20;

pub struct File<'a> {
	pub name: &'a str,
	pub data: &'a [u8],
	pub mode: u32,
}

fn sha1_hex(data: &[u8]) -> String {
	Sha1::digest(data)
		.iter()
		.map(|b| format!("{b:02x}"))
		.collect()
}

/// `2026-10-21T03:15:00Z` from seconds since the epoch.
pub fn iso8601(secs: u64) -> String {
	let days = (secs / 86_400) as i64;
	let rem = secs % 86_400;
	// civil date from the number of days since 1970-01-01 (Howard Hinnant's algorithm)
	let z = days + 719_468;
	let era = z.div_euclid(146_097);
	let doe = z.rem_euclid(146_097);
	let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
	let y = yoe + era * 400;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let d = doy - (153 * mp + 2) / 5 + 1;
	let m = if mp < 10 { mp + 3 } else { mp - 9 };
	let y = if m <= 2 { y + 1 } else { y };
	format!(
		"{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
		rem / 3600,
		rem % 3600 / 60,
		rem % 60
	)
}

fn xml_escape(s: &str) -> String {
	s.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
}

pub fn build(files: &[File], mtime: u64) -> Result<Vec<u8>, Error> {
	let time = iso8601(mtime);
	let mut toc = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xar>\n <toc>\n");
	toc.push_str(&format!("  <creation-time>{time}</creation-time>\n"));
	toc.push_str(&format!(
		"  <checksum style=\"sha1\">\n   <offset>0</offset>\n   <size>{SHA1_LEN}</size>\n  </checksum>\n"
	));
	let mut offset = SHA1_LEN;
	for (i, f) in files.iter().enumerate() {
		let sum = sha1_hex(f.data);
		toc.push_str(&format!(
			"  <file id=\"{}\">\n   <name>{}</name>\n   <type>file</type>\n   <mode>{:04o}</mode>\n\
			 \x20  <uid>0</uid>\n   <gid>0</gid>\n   <user>root</user>\n   <group>wheel</group>\n\
			 \x20  <mtime>{time}</mtime>\n   <data>\n    <length>{len}</length>\n\
			 \x20   <encoding style=\"application/octet-stream\"/>\n    <offset>{offset}</offset>\n\
			 \x20   <size>{len}</size>\n    <extracted-checksum style=\"sha1\">{sum}</extracted-checksum>\n\
			 \x20   <archived-checksum style=\"sha1\">{sum}</archived-checksum>\n   </data>\n  </file>\n",
			i + 1,
			xml_escape(f.name),
			f.mode & 0o7777,
			len = f.data.len(),
		));
		offset += f.data.len();
	}
	toc.push_str(" </toc>\n</xar>\n");

	let mut z = ZlibEncoder::new(Vec::new(), Compression::best());
	z.write_all(toc.as_bytes())?;
	let compressed = z.finish()?;

	let mut out = Vec::with_capacity(HEADER_SIZE as usize + compressed.len() + offset);
	out.extend_from_slice(&MAGIC.to_be_bytes());
	out.extend_from_slice(&HEADER_SIZE.to_be_bytes());
	out.extend_from_slice(&1u16.to_be_bytes()); // version
	out.extend_from_slice(&(compressed.len() as u64).to_be_bytes());
	out.extend_from_slice(&(toc.len() as u64).to_be_bytes());
	out.extend_from_slice(&1u32.to_be_bytes()); // checksum algorithm: SHA-1
	out.extend_from_slice(&compressed);
	out.extend_from_slice(&Sha1::digest(&compressed));
	for f in files {
		out.extend_from_slice(f.data);
	}
	Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;
	use std::io::Read;

	/// Independent reader: the files of an archive as (name, mode, data).
	pub(crate) fn read(bytes: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
		assert_eq!(&bytes[..4], b"xar!");
		assert_eq!(u16::from_be_bytes(bytes[4..6].try_into().unwrap()), 28);
		assert_eq!(u16::from_be_bytes(bytes[6..8].try_into().unwrap()), 1);
		let clen = u64::from_be_bytes(bytes[8..16].try_into().unwrap()) as usize;
		let ulen = u64::from_be_bytes(bytes[16..24].try_into().unwrap()) as usize;
		assert_eq!(u32::from_be_bytes(bytes[24..28].try_into().unwrap()), 1);
		let compressed = &bytes[28..28 + clen];
		let mut toc = String::new();
		flate2::read::ZlibDecoder::new(compressed)
			.read_to_string(&mut toc)
			.unwrap();
		assert_eq!(toc.len(), ulen);
		let heap = &bytes[28 + clen..];
		assert_eq!(
			&heap[..SHA1_LEN],
			Sha1::digest(compressed).as_slice(),
			"checksum of the TOC"
		);

		let tag = |s: &str, name: &str| -> Option<String> {
			let open = format!("<{name}>");
			let i = s.find(&open)? + open.len();
			let j = s[i..].find(&format!("</{name}>"))? + i;
			Some(s[i..j].to_string())
		};
		toc.split("<file id=")
			.skip(1)
			.map(|f| {
				let name = tag(f, "name").unwrap();
				let mode = u32::from_str_radix(&tag(f, "mode").unwrap(), 8).unwrap();
				let off: usize = tag(f, "offset").unwrap().parse().unwrap();
				let size: usize = tag(f, "size").unwrap().parse().unwrap();
				let data = heap[off..off + size].to_vec();
				let want = tag(f, "archived-checksum style=\"sha1\"");
				let _ = want;
				assert!(f.contains(&sha1_hex(&data)), "checksum of {name}");
				(name, mode, data)
			})
			.collect()
	}

	#[test]
	fn dates_are_formatted_in_utc() {
		assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
		assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
		assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
	}

	#[test]
	fn archive_round_trips_through_an_independent_reader() {
		let files = [
			File {
				name: "Bom",
				data: b"BOMStore",
				mode: 0o644,
			},
			File {
				name: "Payload",
				data: &[0x1f, 0x8b, 1, 2, 3],
				mode: 0o644,
			},
			File {
				name: "PackageInfo",
				data: b"<pkg-info/>",
				mode: 0o644,
			},
		];
		let bytes = build(&files, 0).unwrap();
		let back = read(&bytes);
		assert_eq!(
			back.iter()
				.map(|f| (f.0.as_str(), f.2.as_slice()))
				.collect::<Vec<_>>(),
			[
				("Bom", &b"BOMStore"[..]),
				("Payload", &[0x1f, 0x8b, 1, 2, 3][..]),
				("PackageInfo", &b"<pkg-info/>"[..])
			]
		);
		assert_eq!(build(&files, 0).unwrap(), bytes, "reproducible");
	}

	#[test]
	fn libarchive_reads_the_archive_when_bsdtar_is_available() {
		let Ok(bsdtar) = std::process::Command::new("bsdtar")
			.arg("--version")
			.output()
		else {
			return;
		};
		if !bsdtar.status.success() {
			return;
		}
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("a.pkg");
		let payload = vec![7u8; 5000];
		std::fs::write(
			&path,
			build(
				&[
					File {
						name: "Bom",
						data: b"BOMStore",
						mode: 0o644,
					},
					File {
						name: "Payload",
						data: &payload,
						mode: 0o644,
					},
				],
				0,
			)
			.unwrap(),
		)
		.unwrap();
		let list = std::process::Command::new("bsdtar")
			.arg("-tf")
			.arg(&path)
			.output()
			.unwrap();
		assert!(
			list.status.success(),
			"{}",
			String::from_utf8_lossy(&list.stderr)
		);
		assert_eq!(
			String::from_utf8(list.stdout)
				.unwrap()
				.lines()
				.collect::<Vec<_>>(),
			["Bom", "Payload"]
		);
		let out = std::process::Command::new("bsdtar")
			.arg("-xOf")
			.arg(&path)
			.arg("Payload")
			.output()
			.unwrap();
		assert!(
			out.status.success(),
			"{}",
			String::from_utf8_lossy(&out.stderr)
		);
		assert_eq!(out.stdout, payload);
	}
}

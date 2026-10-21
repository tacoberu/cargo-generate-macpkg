//! Writer of the `Bom` file (bill of materials) that a macOS installer package contains.
//!
//! The format is not documented by Apple. The layout here follows `bom.h` and `mkbom` of
//! [bomutils](https://github.com/hogliux/bomutils) (GPL, read as a format description; this
//! code is written independently): a 512 byte header, the named variables, numbered blocks, the
//! block table and the free list, all big endian. The paths are stored in a tree with leaves of
//! at most 256 entries, ordered breadth first and by name inside a directory.

use crate::error::Error;
use crate::pkg::{Node, Tree};
use std::collections::{BTreeMap, VecDeque};

const HEADER_SIZE: usize = 512;
const LEAF_CAPACITY: usize = 256;

const TYPE_FILE: u8 = 1;
const TYPE_DIR: u8 = 2;

/// POSIX `cksum` (CRC-32, polynomial 0x04C11DB7, with the length appended): the checksum of
/// files in a `Bom`.
pub fn cksum(data: &[u8]) -> u32 {
	fn table() -> [u32; 256] {
		let mut t = [0u32; 256];
		for (i, slot) in t.iter_mut().enumerate() {
			let mut c = (i as u32) << 24;
			for _ in 0..8 {
				c = if c & 0x8000_0000 != 0 {
					(c << 1) ^ 0x04C1_1DB7
				} else {
					c << 1
				};
			}
			*slot = c;
		}
		t
	}
	let t = table();
	let mut crc = 0u32;
	for &b in data {
		crc = t[((crc >> 24) as u8 ^ b) as usize] ^ (crc << 8);
	}
	let mut len = data.len() as u64;
	while len > 0 {
		crc = t[((crc >> 24) as u8 ^ (len & 0xff) as u8) as usize] ^ (crc << 8);
		len >>= 8;
	}
	!crc
}

fn be32(out: &mut Vec<u8>, v: u32) {
	out.extend_from_slice(&v.to_be_bytes());
}

fn be16(out: &mut Vec<u8>, v: u16) {
	out.extend_from_slice(&v.to_be_bytes());
}

/// The numbered blocks and variables of the file.
struct Store {
	/// `blocks[0]` is the null block.
	blocks: Vec<Vec<u8>>,
	vars: Vec<(&'static str, u32)>,
}

impl Store {
	fn new() -> Self {
		Store {
			blocks: vec![Vec::new()],
			vars: Vec::new(),
		}
	}

	fn add_block(&mut self, data: Vec<u8>) -> u32 {
		self.blocks.push(data);
		(self.blocks.len() - 1) as u32
	}

	fn add_var(&mut self, name: &'static str, data: Vec<u8>) {
		let index = self.add_block(data);
		self.vars.push((name, index));
	}

	fn finish(&self) -> Vec<u8> {
		let mut vars = Vec::new();
		be32(&mut vars, self.vars.len() as u32);
		for (name, index) in &self.vars {
			be32(&mut vars, *index);
			vars.push(name.len() as u8);
			vars.extend_from_slice(name.as_bytes());
		}
		let data_len: usize = self.blocks.iter().map(Vec::len).sum();
		let blocks_at = HEADER_SIZE + vars.len();

		let mut table = Vec::new();
		be32(&mut table, self.blocks.len() as u32);
		let mut address = blocks_at;
		for (i, b) in self.blocks.iter().enumerate() {
			if i == 0 {
				be32(&mut table, 0);
				be32(&mut table, 0);
			} else {
				be32(&mut table, address as u32);
				be32(&mut table, b.len() as u32);
				address += b.len();
			}
		}
		// free list: no entries, two empty pointers (as `mkbom` writes it)
		let mut free = Vec::new();
		be32(&mut free, 0);
		free.extend_from_slice(&[0u8; 16]);

		let mut out =
			Vec::with_capacity(HEADER_SIZE + vars.len() + data_len + table.len() + free.len());
		out.extend_from_slice(b"BOMStore");
		be32(&mut out, 1);
		be32(&mut out, (self.blocks.len() - 1) as u32);
		be32(&mut out, (blocks_at + data_len) as u32);
		be32(&mut out, (table.len() + free.len()) as u32);
		be32(&mut out, HEADER_SIZE as u32);
		be32(&mut out, vars.len() as u32);
		out.resize(HEADER_SIZE, 0);
		out.extend_from_slice(&vars);
		for b in self.blocks.iter().skip(1) {
			out.extend_from_slice(b);
		}
		out.extend_from_slice(&table);
		out.extend_from_slice(&free);
		out
	}
}

/// One path of the bill.
struct Item<'a> {
	name: &'a str,
	/// `st_mode` including the file type.
	mode: u32,
	size: u32,
	checksum: u32,
	children: Vec<usize>,
}

fn tree_block(child: u32, block_size: u32, path_count: u32) -> Vec<u8> {
	let mut b = Vec::with_capacity(21);
	b.extend_from_slice(b"tree");
	be32(&mut b, 1);
	be32(&mut b, child);
	be32(&mut b, block_size);
	be32(&mut b, path_count);
	b.push(0);
	b
}

fn paths_block(is_leaf: bool, entries: &[(u32, u32)], forward: u32, backward: u32) -> Vec<u8> {
	let mut b = Vec::with_capacity(12 + entries.len() * 8);
	be16(&mut b, u16::from(is_leaf));
	be16(&mut b, entries.len() as u16);
	be32(&mut b, forward);
	be32(&mut b, backward);
	for (i0, i1) in entries {
		be32(&mut b, *i0);
		be32(&mut b, *i1);
	}
	b
}

/// `Bom` for the files of `tree`, all owned by `uid:gid` and with the time `mtime`.
pub fn build(tree: &Tree, uid: u32, gid: u32, mtime: u32) -> Result<Vec<u8>, Error> {
	// the items: [0] is the root ".", the others follow in the order of the tree
	let mut items = vec![Item {
		name: ".",
		mode: 0o040755,
		size: 0,
		checksum: 0,
		children: Vec::new(),
	}];
	let mut index_of: BTreeMap<&str, usize> = BTreeMap::new();
	for (path, entry) in tree {
		let (size, checksum, mode) = match &entry.node {
			Node::Dir => (0, 0, 0o040000 | (entry.mode & 0o7777)),
			Node::File(data) => {
				let size = u32::try_from(data.len()).map_err(|_| {
					Error::Config(format!(
						"{path:?} is larger than 4 GiB, a package cannot hold it"
					))
				})?;
				(size, cksum(data), 0o100000 | (entry.mode & 0o7777))
			}
		};
		let (parent, name) = match path.rsplit_once('/') {
			Some((parent, name)) => (index_of.get(parent).copied(), name),
			None => (Some(0), path.as_str()),
		};
		let parent = parent.ok_or_else(|| {
			Error::Config(format!("{path:?} has no parent directory in the package"))
		})?;
		let idx = items.len();
		items.push(Item {
			name,
			mode,
			size,
			checksum,
			children: Vec::new(),
		});
		items[parent].children.push(idx);
		index_of.insert(path.as_str(), idx);
	}
	for i in 0..items.len() {
		let mut kids = std::mem::take(&mut items[i].children);
		kids.sort_by(|a, b| items[*a].name.as_bytes().cmp(items[*b].name.as_bytes()));
		items[i].children = kids;
	}
	let num = items.len() as u32;

	let mut store = Store::new();

	// BomInfo: version, number of paths + 1, one (zeroed) info entry
	let mut info = Vec::new();
	be32(&mut info, 1);
	be32(&mut info, num + 1);
	be32(&mut info, 1);
	info.extend_from_slice(&[0u8; 16]);
	store.add_var("BomInfo", info);

	// Paths: breadth first, children of a directory ordered by name, leaves of 256 paths
	let leaves = (num as usize).div_ceil(LEAF_CAPACITY);
	let mut root_entries: Vec<(u32, u32)> = vec![(0, 0); leaves];
	let mut leaf: Option<Vec<(u32, u32)>> = None;
	let (mut leaf_backward, mut last_paths_id, mut last_file_info) = (0u32, 0u32, 0u32);
	let mut current_leaf = 0usize;
	let mut k = 0usize;
	let mut j = 0u32;
	let mut queue: VecDeque<(u32, usize)> = VecDeque::new();
	queue.push_back((0, usize::MAX)); // virtual root whose only child is "."
	while let Some((parent, dir)) = queue.pop_front() {
		let kids: Vec<usize> = if dir == usize::MAX {
			vec![0]
		} else {
			items[dir].children.clone()
		};
		for idx in kids {
			let item = &items[idx];
			if k == 0 {
				let mut new_paths_id = 0;
				if let Some(entries) = leaf.take() {
					new_paths_id = store.add_block(paths_block(true, &entries, 0, leaf_backward));
					root_entries[current_leaf].0 = new_paths_id;
					if last_paths_id != 0 {
						let prev = &mut store.blocks[last_paths_id as usize];
						prev[4..8].copy_from_slice(&new_paths_id.to_be_bytes());
					}
					root_entries[current_leaf].1 = last_file_info;
					current_leaf += 1;
				}
				leaf = Some(Vec::new());
				leaf_backward = new_paths_id;
				last_paths_id = new_paths_id;
			}

			let mut info2 = Vec::with_capacity(31);
			info2.push(if item.mode & 0o170000 == 0o040000 {
				TYPE_DIR
			} else {
				TYPE_FILE
			});
			info2.push(1);
			be16(&mut info2, 3);
			be16(&mut info2, item.mode as u16);
			be32(&mut info2, uid);
			be32(&mut info2, gid);
			be32(&mut info2, mtime);
			be32(&mut info2, item.size);
			info2.push(1);
			be32(&mut info2, item.checksum);
			be32(&mut info2, 0); // length of the link name
			let info2_id = store.add_block(info2);

			let mut info1 = Vec::with_capacity(8);
			be32(&mut info1, j + 1);
			be32(&mut info1, info2_id);
			let info1_id = store.add_block(info1);

			let mut file = Vec::with_capacity(5 + item.name.len());
			be32(&mut file, parent);
			file.extend_from_slice(item.name.as_bytes());
			file.push(0);
			let file_id = store.add_block(file);
			last_file_info = file_id;

			leaf.as_mut()
				.expect("leaf is open")
				.push((info1_id, file_id));
			queue.push_back((j + 1, idx));
			j += 1;
			k = (k + 1) % LEAF_CAPACITY;
		}
	}
	let last = leaf.take().expect("at least the root exists");
	let child = if leaves > 1 {
		let last_id = store.add_block(paths_block(true, &last, 0, leaf_backward));
		store.blocks[last_paths_id as usize][4..8].copy_from_slice(&last_id.to_be_bytes());
		root_entries[current_leaf] = (last_id, last_file_info);
		store.add_block(paths_block(false, &root_entries, 0, 0))
	} else {
		store.add_block(paths_block(true, &last, 0, 0))
	};
	store.add_var("Paths", tree_block(child, 4096, num));

	// empty trees: hard links, version index, 64-bit sizes
	let empty = paths_block(true, &[], 0, 0);
	let child = store.add_block(empty.clone());
	store.add_var("HLIndex", tree_block(child, 4096, 0));

	let child = store.add_block(empty.clone());
	let vtree = store.add_block(tree_block(child, 128, 0));
	let mut vindex = Vec::with_capacity(13);
	be32(&mut vindex, 1);
	be32(&mut vindex, vtree);
	be32(&mut vindex, 0);
	vindex.push(0);
	store.add_var("VIndex", vindex);

	let child = store.add_block(empty);
	store.add_var("Size64", tree_block(child, 4096, 0));

	Ok(store.finish())
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;
	use crate::pkg::Entry;
	use std::collections::BTreeMap;

	fn u32_at(b: &[u8], at: usize) -> u32 {
		u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
	}

	fn u16_at(b: &[u8], at: usize) -> u16 {
		u16::from_be_bytes(b[at..at + 2].try_into().unwrap())
	}

	/// Independent reader (written from the format description, not from `build`):
	/// the paths with their `(mode, uid, gid, size, checksum)`.
	pub(crate) fn read(bom: &[u8]) -> BTreeMap<String, (u16, u32, u32, u32, u32)> {
		assert_eq!(&bom[..8], b"BOMStore");
		assert_eq!(u32_at(bom, 8), 1, "version");
		let blocks = u32_at(bom, 12) as usize;
		let (index_off, index_len) = (u32_at(bom, 16) as usize, u32_at(bom, 20) as usize);
		let (vars_off, vars_len) = (u32_at(bom, 24) as usize, u32_at(bom, 28) as usize);
		assert_eq!(index_off + index_len, bom.len(), "the index ends the file");
		assert_eq!(vars_off, HEADER_SIZE);

		// block table
		let count = u32_at(bom, index_off) as usize;
		assert_eq!(count, blocks + 1, "number of blocks");
		let block = |id: u32| -> &[u8] {
			let at = index_off + 4 + 8 * id as usize;
			let (addr, len) = (u32_at(bom, at) as usize, u32_at(bom, at + 4) as usize);
			assert!(
				id != 0 && addr + len <= index_off,
				"block {id} is inside the data"
			);
			&bom[addr..addr + len]
		};
		// variables
		let mut vars = BTreeMap::new();
		let n = u32_at(bom, vars_off) as usize;
		let mut at = vars_off + 4;
		for _ in 0..n {
			let idx = u32_at(bom, at);
			let len = bom[at + 4] as usize;
			let name = String::from_utf8(bom[at + 5..at + 5 + len].to_vec()).unwrap();
			vars.insert(name, idx);
			at += 5 + len;
		}
		assert_eq!(at, vars_off + vars_len);
		assert_eq!(
			vars.keys().map(String::as_str).collect::<Vec<_>>(),
			["BomInfo", "HLIndex", "Paths", "Size64", "VIndex"]
		);

		// the tree of paths
		let tree = block(vars["Paths"]);
		assert_eq!(&tree[..4], b"tree");
		assert_eq!(u32_at(tree, 12), 4096);
		let path_count = u32_at(tree, 16) as usize;
		let mut leaves = Vec::new();
		let mut node = u32_at(tree, 8);
		let first = loop {
			let b = block(node);
			if u16_at(b, 0) == 1 {
				break node;
			}
			node = u32_at(b, 12); // first child of a branch
		};
		// walk the leaves by their `forward` links
		let mut id = first;
		let mut backward = 0;
		while id != 0 {
			let b = block(id);
			assert_eq!(u16_at(b, 0), 1);
			assert_eq!(u32_at(b, 8), backward, "backward link");
			let count = u16_at(b, 2) as usize;
			assert!(count <= 256);
			for e in 0..count {
				leaves.push((u32_at(b, 12 + 8 * e), u32_at(b, 16 + 8 * e)));
			}
			backward = id;
			id = u32_at(b, 4);
		}
		assert_eq!(leaves.len(), path_count);
		assert_eq!(u32_at(block(vars["BomInfo"]), 4) as usize, path_count + 1);

		// paths: id -> (parent, name, info)
		let mut by_id = BTreeMap::new();
		for (i0, i1) in leaves {
			let info1 = block(i0);
			let (pid, info2) = (u32_at(info1, 0), block(u32_at(info1, 4)));
			let file = block(i1);
			let parent = u32_at(file, 0);
			let name = String::from_utf8(file[4..file.len() - 1].to_vec()).unwrap();
			assert_eq!(file[file.len() - 1], 0);
			assert_eq!(info2[1], 1);
			assert_eq!(info2[22], 1);
			assert_eq!(u32_at(info2, 27), 0, "no link name");
			let kind = if u16_at(info2, 4) & 0o170000 == 0o040000 {
				2
			} else {
				1
			};
			assert_eq!(info2[0], kind, "type of {name}");
			by_id.insert(
				pid,
				(
					parent,
					name,
					(
						u16_at(info2, 4),
						u32_at(info2, 6),
						u32_at(info2, 10),
						u32_at(info2, 18),
						u32_at(info2, 23),
					),
				),
			);
		}
		let full = |mut id: u32| {
			let mut parts = Vec::new();
			while id != 0 {
				let (parent, name, _) = &by_id[&id];
				parts.push(name.clone());
				id = *parent;
			}
			parts.reverse();
			parts.join("/")
		};
		by_id
			.iter()
			.map(|(id, (_, _, info))| {
				let (mode, uid, gid, size, sum) = *info;
				(full(*id), (mode, uid, gid, size, sum))
			})
			.collect()
	}

	fn tree_of(files: &[(&str, &str, u32)]) -> Tree {
		let mut t = Tree::new();
		for (path, data, mode) in files {
			let mut at = 0;
			while let Some(i) = path[at..].find('/') {
				at += i;
				t.entry(path[..at].to_string()).or_insert(Entry {
					node: Node::Dir,
					mode: 0o755,
				});
				at += 1;
			}
			t.insert(
				path.to_string(),
				Entry {
					node: Node::File(data.as_bytes().to_vec()),
					mode: *mode,
				},
			);
		}
		t
	}

	#[test]
	fn cksum_matches_the_posix_tool() {
		assert_eq!(cksum(b"123456789"), 930766865);
		assert_eq!(cksum(b""), 4294967295);
		if let Ok(out) = std::process::Command::new("cksum")
			.stdin(std::process::Stdio::piped())
			.stdout(std::process::Stdio::piped())
			.spawn()
			.and_then(|mut c| {
				use std::io::Write;
				c.stdin.take().unwrap().write_all(b"hello world\n")?;
				c.wait_with_output()
			}) {
			let text = String::from_utf8(out.stdout).unwrap();
			assert_eq!(
				text.split_whitespace().next().unwrap(),
				cksum(b"hello world\n").to_string()
			);
		}
	}

	#[test]
	fn small_bom_describes_every_path() {
		let t = tree_of(&[
			("usr/local/bin/tool", "#!/bin/sh\n", 0o755),
			("usr/local/share/doc/README", "hello", 0o644),
			("Library/LaunchDaemons/a.plist", "<plist/>", 0o644),
		]);
		let bom = build(&t, 0, 80, 1_000_000).unwrap();
		let paths = read(&bom);
		assert_eq!(paths.len(), t.len() + 1);
		assert_eq!(paths["."], (0o040755, 0, 80, 0, 0));
		assert_eq!(paths["./usr/local/bin"], (0o040755, 0, 80, 0, 0));
		let tool = paths["./usr/local/bin/tool"];
		assert_eq!((tool.0, tool.1, tool.2, tool.3), (0o100755, 0, 80, 10));
		assert_eq!(tool.4, cksum(b"#!/bin/sh\n"));
		assert_eq!(paths["./Library/LaunchDaemons/a.plist"].0, 0o100644);
	}

	#[test]
	fn many_paths_are_split_into_leaves_of_256() {
		let files: Vec<(String, String)> = (0..700)
			.map(|i| (format!("data/f{i:04}"), format!("content {i}")))
			.collect();
		let refs: Vec<(&str, &str, u32)> = files
			.iter()
			.map(|(p, d)| (p.as_str(), d.as_str(), 0o644))
			.collect();
		let t = tree_of(&refs);
		let bom = build(&t, 0, 80, 0).unwrap();
		let paths = read(&bom);
		assert_eq!(paths.len(), 700 + 2); // files + "data" + "."
		assert_eq!(paths["./data/f0699"].3, "content 699".len() as u32);
		// the tree of paths has a branch with three leaves
		let index_off = u32_at(&bom, 16) as usize;
		let block = |id: u32| {
			let at = index_off + 4 + 8 * id as usize;
			let (a, l) = (u32_at(&bom, at) as usize, u32_at(&bom, at + 4) as usize);
			bom[a..a + l].to_vec()
		};
		let paths_var = (0..u32_at(&bom, 512))
			.scan(512 + 4, |at, _| {
				let (i, len) = (u32_at(&bom, *at), bom[*at + 4] as usize);
				let name = bom[*at + 5..*at + 5 + len].to_vec();
				*at += 5 + len;
				Some((name, i))
			})
			.find(|(n, _)| n == b"Paths")
			.unwrap()
			.1;
		let root = block(u32_at(&block(paths_var), 8));
		assert_eq!((u16_at(&root, 0), u16_at(&root, 2)), (0, 3));
	}

	#[test]
	fn output_is_deterministic_and_directories_sorted_by_name() {
		let t = tree_of(&[
			("b/x", "1", 0o644),
			("a/y", "2", 0o644),
			("a-b", "3", 0o644),
		]);
		let one = build(&t, 0, 80, 0).unwrap();
		assert_eq!(one, build(&t, 0, 80, 0).unwrap());
		// ids are assigned breadth first: ".", then a, a-b, b, then the files inside
		let paths = read(&one);
		assert_eq!(
			paths.keys().map(String::as_str).collect::<Vec<_>>(),
			[".", "./a", "./a-b", "./a/y", "./b", "./b/x"]
		);
	}

	#[test]
	fn missing_parent_is_an_error() {
		let mut t = Tree::new();
		t.insert(
			"a/b".into(),
			Entry {
				node: Node::File(vec![1]),
				mode: 0o644,
			},
		);
		assert!(matches!(build(&t, 0, 80, 0), Err(Error::Config(m)) if m.contains("parent")));
	}
}

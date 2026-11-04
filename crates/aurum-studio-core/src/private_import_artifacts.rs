//! Verified new importer outputs which remain private during source publication.
//! This reader never evaluates Godot Variant syntax or changes snapshot policy.
use crate::{hash::Sha256, Project};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_FILES: usize = 20_000;
const MAX_ENTRIES: usize = 40_000;
const MAX_ARTIFACTS: usize = 256;
const MAX_PATH: usize = 1024;
const SOURCE_BYTES: u64 = 512 * 1024 * 1024;
const READ_BYTES: u64 = 1024 * 1024 * 1024;
const REMAP_BYTES: u64 = 64 * 1024;
const GLB_BYTES: u64 = 64 * 1024 * 1024;
const IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const CACHE_BYTES: u64 = 128 * 1024 * 1024;

fn key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.into()
    }
}

pub(crate) fn refusal(reason: &str, path: &str) -> String {
    let safe: String = path
        .chars()
        .filter(|character| !character.is_control())
        .take(240)
        .collect();
    format!("{reason}; publication refused: {safe}")
}

fn relative(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > MAX_PATH
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | "..") || part.ends_with(['.', ' ']))
    {
        return Err("private import artifact path is invalid".into());
    }
    Ok(())
}

struct Budget {
    deadline: Instant,
    bytes: u64,
}
impl Budget {
    fn new() -> Result<Self, String> {
        let local = Duration::from_secs(10);
        let duration = crate::operation_context::OperationContext::current()
            .map(|context| context.budget(local))
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or(local);
        let budget = Self {
            deadline: Instant::now() + duration,
            bytes: 0,
        };
        budget.check()?;
        Ok(budget)
    }
    fn check(&self) -> Result<(), String> {
        if let Some(context) = crate::operation_context::OperationContext::current() {
            context.check().map_err(|error| error.to_string())?;
        }
        if Instant::now() >= self.deadline {
            return Err("private import inspection deadline expired".into());
        }
        Ok(())
    }
}

#[derive(Clone)]
struct FileProof {
    path: String,
    sha256: String,
    bytes: u64,
}

fn read_file(
    root: &Path,
    path: &str,
    limit: u64,
    budget: &mut Budget,
    keep: bool,
) -> Result<(FileProof, Vec<u8>), String> {
    relative(path)?;
    budget.check()?;
    let absolute = root.join(path);
    let mut file = crate::snapshot::open_checked_file(&absolute, budget.deadline)
        .map_err(|_| refusal("private import file is inaccessible or linked", path))?;
    let before = file
        .metadata()
        .map_err(|_| "private import file metadata unavailable")?;
    if !before.is_file()
        || before.len() > limit
        || budget.bytes.saturating_add(before.len()) > READ_BYTES
    {
        return Err(refusal(
            "private import file exceeds inspection bounds",
            path,
        ));
    }
    let mut hasher = Sha256::new();
    let mut output = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut length = 0u64;
    loop {
        budget.check()?;
        let take = buffer
            .len()
            .min(before.len().saturating_sub(length).saturating_add(1) as usize);
        let count = file
            .read(&mut buffer[..take])
            .map_err(|_| "private import file read failed")?;
        budget.check()?;
        if count == 0 {
            break;
        }
        length = length.saturating_add(count as u64);
        budget.bytes = budget.bytes.saturating_add(count as u64);
        if length > before.len() || budget.bytes > READ_BYTES {
            return Err(refusal("private import file grew during inspection", path));
        }
        hasher.update(&buffer[..count]);
        if keep {
            output.extend_from_slice(&buffer[..count]);
        }
    }
    crate::snapshot::check_opened_path(&file, &absolute, budget.deadline)
        .map_err(|_| refusal("private import file path changed during inspection", path))?;
    let after = file
        .metadata()
        .map_err(|_| "private import file metadata unavailable")?;
    let before_modified = before
        .modified()
        .map_err(|_| "private import file timestamp unavailable")?;
    let after_modified = after
        .modified()
        .map_err(|_| "private import file timestamp unavailable")?;
    if length != before.len() || after.len() != length || before_modified != after_modified {
        return Err(refusal(
            "private import file changed during inspection",
            path,
        ));
    }
    budget.check()?;
    Ok((
        FileProof {
            path: path.into(),
            sha256: hasher.hex(),
            bytes: length,
        },
        output,
    ))
}

fn inventory(root: &Path, budget: &mut Budget) -> Result<BTreeMap<String, FileProof>, String> {
    struct Walk {
        entries: usize,
        bytes: u64,
        paths: usize,
        files: BTreeMap<String, FileProof>,
    }
    fn visit(
        root: &Path,
        prefix: &str,
        depth: usize,
        walk: &mut Walk,
        budget: &mut Budget,
    ) -> Result<(), String> {
        budget.check()?;
        if depth > 24 {
            return Err("private import source nesting exceeds its bound".into());
        }
        let directory = if prefix.is_empty() {
            root.to_path_buf()
        } else {
            root.join(prefix)
        };
        let guard = crate::snapshot::open_checked_directory(&directory, budget.deadline)?;
        let mut names = Vec::new();
        for entry in
            std::fs::read_dir(&directory).map_err(|_| "private import source enumeration failed")?
        {
            budget.check()?;
            walk.entries += 1;
            if walk.entries > MAX_ENTRIES {
                return Err("private import source entry bound exceeded".into());
            }
            names.push(
                entry
                    .map_err(|_| "private import source entry unavailable")?
                    .file_name(),
            );
        }
        names.sort();
        for name in names {
            budget.check()?;
            let name = name
                .to_str()
                .ok_or("private import source paths must be UTF-8")?;
            let path = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            if !crate::snapshot::includes(&path) {
                continue;
            }
            relative(&path)?;
            let metadata = std::fs::symlink_metadata(root.join(&path))
                .map_err(|_| "private import source metadata unavailable")?;
            if metadata.is_dir() {
                visit(root, &path, depth + 1, walk, budget)?;
            } else if metadata.is_file() {
                walk.bytes = walk.bytes.saturating_add(metadata.len());
                walk.paths = walk.paths.saturating_add(path.len());
                if walk.files.len() >= MAX_FILES
                    || walk.bytes > SOURCE_BYTES
                    || walk.paths > 2 * 1024 * 1024
                {
                    return Err("private import source inventory exceeds inspection bounds".into());
                }
                let (proof, _) = read_file(root, &path, SOURCE_BYTES, budget, false)?;
                if walk.files.insert(key(&path), proof).is_some() {
                    return Err("private import source paths alias".into());
                }
            } else {
                return Err(refusal(
                    "private import source is linked or nonregular",
                    &path,
                ));
            }
        }
        crate::snapshot::check_opened_path(&guard, &directory, budget.deadline)?;
        budget.check()
    }
    let mut walk = Walk {
        entries: 0,
        bytes: 0,
        paths: 0,
        files: BTreeMap::new(),
    };
    visit(root, "", 0, &mut walk, budget)?;
    Ok(walk.files)
}

pub(crate) struct Baseline {
    files: BTreeMap<String, FileProof>,
    godot: String,
}

#[derive(Clone)]
enum Binding {
    Remap {
        source: FileProof,
        destinations: Vec<FileProof>,
    },
    EmbeddedPng {
        model: FileProof,
        image: usize,
    },
}
#[derive(Clone)]
struct Artifact {
    file: FileProof,
    binding: Binding,
}

#[derive(Default)]
pub(crate) struct PrivateArtifacts {
    rows: Vec<Artifact>,
    godot: String,
}

fn godot_prefix(project: &Project) -> Result<String, String> {
    let path = project
        .godot_project_dir()
        .ok_or("private import inspection requires project.godot")?
        .strip_prefix(&project.root)
        .map_err(|_| "Godot directory escaped candidate")?
        .to_str()
        .ok_or("Godot directory must be UTF-8")?
        .replace('\\', "/");
    if !path.is_empty() {
        relative(&path)?;
    }
    Ok(path)
}

fn requested(checked: &Value) -> Result<BTreeSet<String>, String> {
    checked["changes"]
        .as_array()
        .filter(|rows| rows.len() <= 256)
        .ok_or("invalid checked changes")?
        .iter()
        .map(|row| {
            row["path"]
                .as_str()
                .ok_or_else(|| "missing checked path".to_string())
                .map(key)
        })
        .collect()
}

fn under_godot<'a>(path: &'a str, prefix: &str) -> Result<&'a str, String> {
    if prefix.is_empty() {
        Ok(path)
    } else {
        path.strip_prefix(prefix)
            .and_then(|path| path.strip_prefix('/'))
            .ok_or_else(|| refusal("private import artifact escaped Godot directory", path))
    }
}

impl Baseline {
    pub(crate) fn capture(candidate: &Project) -> Result<Self, String> {
        let mut budget = Budget::new()?;
        Ok(Self {
            godot: godot_prefix(candidate)?,
            files: inventory(&candidate.root, &mut budget)?,
        })
    }
    pub(crate) fn classify(
        &self,
        candidate: &Project,
        checked: &Value,
    ) -> Result<PrivateArtifacts, String> {
        let mut budget = Budget::new()?;
        if godot_prefix(candidate)? != self.godot {
            return Err("Godot layout changed during validation".into());
        }
        let requested = requested(checked)?;
        let current = inventory(&candidate.root, &mut budget)?;
        let added: Vec<&FileProof> = current
            .iter()
            .filter(|(path, _)| !self.files.contains_key(*path) && !requested.contains(*path))
            .map(|(_, proof)| proof)
            .collect();
        let mut result = PrivateArtifacts {
            rows: Vec::new(),
            godot: self.godot.clone(),
        };
        // Derive only filenames declared by embedded PNG images in prior GLBs.
        for model in self
            .files
            .values()
            .filter(|proof| proof.path.ends_with(".glb"))
        {
            budget.check()?;
            let stem = model.path.strip_suffix(".glb").unwrap_or_default();
            if !added.iter().any(|proof| {
                proof.path.starts_with(&format!("{stem}_")) && proof.path.ends_with(".png")
            }) {
                continue;
            }
            under_godot(&model.path, &self.godot)?;
            let (actual, bytes) =
                read_file(&candidate.root, &model.path, GLB_BYTES, &mut budget, true)?;
            if actual.sha256 != model.sha256 {
                return Err(refusal("imported GLB source changed", &model.path));
            }
            for image in embedded_images(&bytes)? {
                let path = format!("{stem}_{}.png", image.name);
                if let Some(proof) = added.iter().find(|proof| key(&proof.path) == key(&path)) {
                    if proof.sha256 != crate::sha256_hex(image.bytes)
                        || proof.bytes != image.bytes.len() as u64
                    {
                        return Err(refusal(
                            "generated PNG does not match embedded GLB bytes",
                            &proof.path,
                        ));
                    }
                    result.push(Artifact {
                        file: (*proof).clone(),
                        binding: Binding::EmbeddedPng {
                            model: model.clone(),
                            image: image.index,
                        },
                    })?;
                }
            }
        }
        for proof in added.iter().filter(|proof| proof.path.ends_with(".import")) {
            budget.check()?;
            let source_path = proof.path.strip_suffix(".import").unwrap_or_default();
            let source = self
                .files
                .get(&key(source_path))
                .or_else(|| {
                    result
                        .rows
                        .iter()
                        .find(|row| key(&row.file.path) == key(source_path))
                        .map(|row| &row.file)
                })
                .ok_or_else(|| refusal("new import remap has no verified source", &proof.path))?;
            if current
                .get(&key(source_path))
                .is_none_or(|actual| actual.sha256 != source.sha256)
            {
                return Err(refusal("import remap source changed", source_path));
            }
            let (actual, bytes) =
                read_file(&candidate.root, &proof.path, REMAP_BYTES, &mut budget, true)?;
            if actual.sha256 != proof.sha256 {
                return Err(refusal(
                    "new import remap changed during inspection",
                    &proof.path,
                ));
            }
            let destinations = remap(
                &bytes,
                &format!("res://{}", under_godot(source_path, &self.godot)?),
            )
            .map_err(|error| refusal(&error, &proof.path))?;
            let mut files = Vec::new();
            for destination in destinations {
                let path = if self.godot.is_empty() {
                    destination
                } else {
                    format!("{}/{destination}", self.godot)
                };
                let (file, _) = read_file(&candidate.root, &path, CACHE_BYTES, &mut budget, false)?;
                files.push(file);
            }
            result.push(Artifact {
                file: (*proof).clone(),
                binding: Binding::Remap {
                    source: source.clone(),
                    destinations: files,
                },
            })?;
        }
        budget.check()?;
        Ok(result)
    }
}

impl PrivateArtifacts {
    fn push(&mut self, row: Artifact) -> Result<(), String> {
        if self.rows.len() >= MAX_ARTIFACTS || self.contains(&row.file.path) {
            return Err("private import artifact registration bound exceeded or duplicate".into());
        }
        self.rows.push(row);
        Ok(())
    }
    pub(crate) fn contains(&self, path: &str) -> bool {
        self.rows.iter().any(|row| key(&row.file.path) == key(path))
    }
    pub(crate) fn count(&self) -> usize {
        self.rows.len()
    }
    pub(crate) fn verify(
        &self,
        candidate: &Project,
        original: &Project,
        checked: &Value,
    ) -> Result<(), String> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let mut budget = Budget::new()?;
        let requests = requested(checked)?;
        if self.rows.len() > MAX_ARTIFACTS || godot_prefix(candidate)? != self.godot {
            return Err("invalid private import registrations".into());
        }
        let originals = inventory(&original.root, &mut budget)?;
        let mut seen = BTreeSet::new();
        for row in &self.rows {
            budget.check()?;
            if originals.contains_key(&key(&row.file.path))
                || requests.contains(&key(&row.file.path))
                || !seen.insert(key(&row.file.path))
            {
                return Err(refusal(
                    "private artifact exemption targets original or requested source",
                    &row.file.path,
                ));
            }
            let limit = match &row.binding {
                Binding::Remap { .. } => REMAP_BYTES,
                Binding::EmbeddedPng { .. } => IMAGE_BYTES,
            };
            let (actual, bytes) =
                read_file(&candidate.root, &row.file.path, limit, &mut budget, true)?;
            if actual.sha256 != row.file.sha256 || actual.bytes != row.file.bytes {
                return Err(refusal(
                    "registered private import artifact changed",
                    &row.file.path,
                ));
            }
            match &row.binding {
                Binding::EmbeddedPng { model, image } => {
                    let (actual, glb) =
                        read_file(&candidate.root, &model.path, GLB_BYTES, &mut budget, true)?;
                    let images = embedded_images(&glb)?;
                    let embedded = images
                        .iter()
                        .find(|row| row.index == *image)
                        .ok_or("registered GLB image disappeared")?;
                    if actual.sha256 != model.sha256
                        || embedded.bytes != bytes
                        || row.file.path
                            != format!(
                                "{}_{}.png",
                                model
                                    .path
                                    .strip_suffix(".glb")
                                    .ok_or("invalid GLB binding")?,
                                embedded.name
                            )
                    {
                        return Err(refusal("registered GLB derivation changed", &row.file.path));
                    }
                }
                Binding::Remap {
                    source,
                    destinations,
                } => {
                    let (actual, _) = read_file(
                        &candidate.root,
                        &source.path,
                        SOURCE_BYTES,
                        &mut budget,
                        false,
                    )?;
                    if actual.sha256 != source.sha256 {
                        return Err(refusal("registered import source changed", &source.path));
                    }
                    let parsed = remap(
                        &bytes,
                        &format!("res://{}", under_godot(&source.path, &self.godot)?),
                    )?;
                    let expected: Vec<String> = destinations
                        .iter()
                        .map(|file| under_godot(&file.path, &self.godot).map(str::to_string))
                        .collect::<Result<_, _>>()?;
                    if parsed != expected {
                        return Err("registered remap destinations changed".into());
                    }
                    for destination in destinations {
                        let (actual, _) = read_file(
                            &candidate.root,
                            &destination.path,
                            CACHE_BYTES,
                            &mut budget,
                            false,
                        )?;
                        if actual.sha256 != destination.sha256 || actual.bytes != destination.bytes
                        {
                            return Err("registered private import cache changed".into());
                        }
                    }
                }
            }
        }
        budget.check()
    }
}

fn literal_string(value: &str) -> Result<String, String> {
    serde_json::from_str::<String>(value)
        .map_err(|_| "import remap requires literal JSON strings".into())
}

fn remap(bytes: &[u8], expected_source: &str) -> Result<Vec<String>, String> {
    if bytes.is_empty() || bytes.len() as u64 > REMAP_BYTES {
        return Err("import remap byte bound exceeded".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "import remap must be UTF-8")?;
    let mut section = "";
    let mut sections = BTreeSet::new();
    let mut fields = BTreeMap::new();
    let mut paths = Vec::new();
    let mut targets = None;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with('[') {
            let name = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
                .ok_or("invalid import remap section")?;
            if !sections.insert(name) {
                return Err("duplicate import remap section".into());
            }
            section = name;
            continue;
        }
        if !matches!(section, "remap" | "deps") {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if section == "remap"
            && (matches!(name, "importer" | "type" | "uid" | "path") || name.starts_with("path."))
        {
            let value = literal_string(value)?;
            if fields.insert(name, value.clone()).is_some() {
                return Err("duplicate import remap field".into());
            }
            if name == "path" || name.starts_with("path.") {
                paths.push(value);
            }
        } else if section == "deps" && name == "source_file" {
            if fields.insert(name, literal_string(value)?).is_some() {
                return Err("duplicate import source field".into());
            }
        } else if section == "deps" && name == "dest_files" {
            if targets.is_some() {
                return Err("duplicate import destination field".into());
            }
            targets = Some(
                serde_json::from_str::<Vec<String>>(value)
                    .map_err(|_| "import destinations require a literal JSON string array")?,
            );
        }
    }
    for name in ["importer", "type"] {
        if fields.get(name).is_none_or(|value| {
            value.is_empty()
                || value.len() > 128
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }) {
            return Err("invalid import remap kind".into());
        }
    }
    if fields
        .get("uid")
        .and_then(|value| value.strip_prefix("uid://"))
        .is_none_or(|value| {
            value.is_empty()
                || value.len() > 64
                || !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        || fields.get("source_file").map(String::as_str) != Some(expected_source)
    {
        return Err("import remap source or UID binding is invalid".into());
    }
    let targets = targets.ok_or("missing import destinations")?;
    if targets.is_empty()
        || targets.len() > 8
        || paths.is_empty()
        || paths.len() > 8
        || paths.iter().any(|path| !targets.contains(path))
    {
        return Err("import remap destination bounds or bindings are invalid".into());
    }
    let mut result: Vec<String> = Vec::new();
    for target in targets {
        let target = target
            .strip_prefix("res://")
            .ok_or("import destination must be a resource path")?;
        relative(target)?;
        if !target.starts_with(".godot/imported/")
            || result.iter().any(|path| key(path) == key(target))
        {
            return Err("import destinations must be unique private cache files".into());
        }
        result.push(target.to_string());
    }
    Ok(result)
}

// Image names borrow the JSON document, so return owned names with borrowed bytes.
struct Image<'a> {
    index: usize,
    name: String,
    bytes: &'a [u8],
}
fn embedded_images(bytes: &[u8]) -> Result<Vec<Image<'_>>, String> {
    fn word(bytes: &[u8], offset: usize) -> Result<usize, String> {
        bytes
            .get(offset..offset + 4)
            .and_then(|word| word.try_into().ok())
            .map(u32::from_le_bytes)
            .map(|word| word as usize)
            .ok_or("truncated GLB header".into())
    }
    if bytes.len() < 28
        || bytes.len() as u64 > GLB_BYTES
        || word(bytes, 0)? != 0x46546c67
        || word(bytes, 4)? != 2
        || word(bytes, 8)? != bytes.len()
    {
        return Err("invalid bounded GLB container".into());
    }
    let json_length = word(bytes, 12)?;
    if json_length > 1024 * 1024 || json_length % 4 != 0 || word(bytes, 16)? != 0x4e4f534a {
        return Err("invalid bounded GLB JSON chunk".into());
    }
    let bin_header = 20usize
        .checked_add(json_length)
        .filter(|end| *end <= bytes.len())
        .ok_or("invalid GLB JSON range")?;
    let document: Value =
        serde_json::from_slice(bytes.get(20..bin_header).ok_or("invalid GLB JSON range")?)
            .map_err(|_| "invalid GLB JSON document")?;
    let bin_length = word(bytes, bin_header)?;
    let start = bin_header
        .checked_add(8)
        .ok_or("invalid GLB binary range")?;
    if bin_length % 4 != 0
        || word(bytes, bin_header + 4)? != 0x004e4942
        || start.checked_add(bin_length) != Some(bytes.len())
    {
        return Err("invalid GLB binary chunk".into());
    }
    let buffers = document["buffers"]
        .as_array()
        .filter(|rows| rows.len() == 1)
        .ok_or("GLB requires one embedded buffer")?;
    let declared = buffers[0]["byteLength"]
        .as_u64()
        .ok_or("GLB buffer length unavailable")?;
    if buffers[0].get("uri").is_some()
        || declared > bin_length as u64
        || (bin_length as u64).saturating_sub(declared) > 3
    {
        return Err("invalid GLB embedded buffer binding".into());
    }
    let views = document["bufferViews"]
        .as_array()
        .filter(|rows| rows.len() <= 4096)
        .ok_or("GLB buffer view bound exceeded")?;
    let images = document["images"]
        .as_array()
        .filter(|rows| rows.len() <= 256)
        .ok_or("GLB image bound exceeded")?;
    let mut output = Vec::new();
    let mut names = BTreeSet::new();
    for (index, image) in images.iter().enumerate() {
        if image["mimeType"] != "image/png" || image.get("uri").is_some() {
            continue;
        }
        let name = image["name"]
            .as_str()
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= 128
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
            .ok_or("unsupported embedded PNG name")?;
        if !names.insert(key(name)) {
            return Err("duplicate embedded PNG name".into());
        }
        let view = image["bufferView"]
            .as_u64()
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| views.get(index))
            .ok_or("invalid embedded PNG view")?;
        let offset = match view.get("byteOffset") {
            None => 0,
            Some(value) => value.as_u64().ok_or("invalid PNG buffer offset")?,
        };
        let length = view["byteLength"]
            .as_u64()
            .filter(|length| *length > 0 && *length <= IMAGE_BYTES)
            .ok_or("invalid embedded PNG length")?;
        if view["buffer"].as_u64() != Some(0)
            || view.get("byteStride").is_some()
            || offset.checked_add(length).is_none_or(|end| end > declared)
        {
            return Err("embedded PNG escaped declared GLB buffer".into());
        }
        let data = bytes
            .get(start + offset as usize..start + (offset + length) as usize)
            .ok_or("invalid embedded PNG byte range")?;
        if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("embedded image is not declared PNG bytes".into());
        }
        output.push(Image {
            index,
            name: name.to_string(),
            bytes: data,
        });
    }
    Ok(output)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    pub(crate) const PNG: &[u8] = b"\x89PNG\r\n\x1a\nprivate-embedded-png";
    pub(crate) fn glb() -> Vec<u8> {
        let mut json = serde_json::to_vec(&serde_json::json!({"asset":{"version":"2.0"},
            "buffers":[{"byteLength":PNG.len()}],"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":PNG.len()}],
            "images":[{"name":"colormap","mimeType":"image/png","bufferView":0}]})).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bin = PNG.to_vec();
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let mut bytes = Vec::new();
        for word in [
            0x46546c67u32,
            2,
            (28 + json.len() + bin.len()) as u32,
            json.len() as u32,
            0x4e4f534a,
        ] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes.extend_from_slice(&json);
        bytes.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0x004e4942u32.to_le_bytes());
        bytes.extend_from_slice(&bin);
        bytes
    }
    pub(crate) fn remap_text(source: &str, cache: &str, importer: &str, kind: &str) -> String {
        format!("[remap]\nimporter=\"{importer}\"\ntype=\"{kind}\"\nuid=\"uid://abc123\"\npath=\"res://.godot/imported/{cache}\"\nmetadata={{\n\"vram_texture\": false\n}}\n\n[deps]\nsource_file=\"res://{source}\"\ndest_files=[\"res://.godot/imported/{cache}\"]\n\n[params]\nnodes/root_script=null\n")
    }
    pub(crate) fn assets(root: &Path) {
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/model.glb"), glb()).unwrap();
        std::fs::write(
            root.join("assets/authored.import"),
            "authored import settings",
        )
        .unwrap();
    }
    pub(crate) fn outputs(root: &Path) {
        std::fs::create_dir_all(root.join(".godot/imported")).unwrap();
        std::fs::write(
            root.join(".godot/imported/model.scn"),
            "private scene cache",
        )
        .unwrap();
        std::fs::write(
            root.join(".godot/imported/model.ctex"),
            "private texture cache",
        )
        .unwrap();
        std::fs::write(root.join("assets/model_colormap.png"), PNG).unwrap();
        std::fs::write(
            root.join("assets/model.glb.import"),
            remap_text("assets/model.glb", "model.scn", "scene", "PackedScene"),
        )
        .unwrap();
        std::fs::write(
            root.join("assets/model_colormap.png.import"),
            remap_text(
                "assets/model_colormap.png",
                "model.ctex",
                "texture",
                "CompressedTexture2D",
            ),
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::*;
    struct Fixture(Project);
    impl Fixture {
        fn new(nested: bool) -> Self {
            let root = std::env::temp_dir().join(format!(
                "aurum-private-import-test-{}",
                crate::random::session_id()
            ));
            let godot = if nested {
                root.join("godot")
            } else {
                root.clone()
            };
            std::fs::create_dir_all(&godot).unwrap();
            std::fs::write(
                root.join("aurum.toml"),
                "schema_version=1\nname=\"private-import\"\nmodules=[]\n",
            )
            .unwrap();
            std::fs::write(
                godot.join("project.godot"),
                "[application]\nconfig/name=\"Private import fixture\"\n",
            )
            .unwrap();
            assets(&godot);
            Self(Project::open(&root).unwrap())
        }
        fn godot(&self) -> &Path {
            self.0.godot_project_dir().unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0.root);
        }
    }
    fn checked() -> Value {
        serde_json::json!({"changes":[]})
    }

    fn sparse_fixture_file(path: &Path, logical_bytes: u64) -> std::fs::File {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        #[cfg(windows)]
        {
            use std::ffi::c_void;
            use std::os::windows::io::{AsRawHandle, RawHandle};
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn DeviceIoControl(
                    file: RawHandle,
                    code: u32,
                    input: *mut c_void,
                    input_bytes: u32,
                    output: *mut c_void,
                    output_bytes: u32,
                    returned: *mut u32,
                    overlapped: *mut c_void,
                ) -> i32;
            }
            let mut returned = 0;
            // A null input sets sparse=true before extending this new file.
            let marked = unsafe {
                DeviceIoControl(
                    file.as_raw_handle(),
                    0x0009_00c4, // FSCTL_SET_SPARSE
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(
                marked,
                0,
                "sparse fixture: {}",
                std::io::Error::last_os_error()
            );
        }
        file.set_len(logical_bytes).unwrap();
        assert_eq!(file.metadata().unwrap().len(), logical_bytes);
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            use std::os::windows::fs::MetadataExt;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetCompressedFileSizeW(path: *const u16, high: *mut u32) -> u32;
            }
            assert_ne!(file.metadata().unwrap().file_attributes() & 0x200, 0);
            let canonical = path.canonicalize().unwrap();
            let wide: Vec<u16> = canonical
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let mut high = 0;
            let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
            assert_ne!(
                low,
                u32::MAX,
                "allocation query: {}",
                std::io::Error::last_os_error()
            );
            let allocated = (u64::from(high) << 32) | u64::from(low);
            assert!(
                allocated <= 1024 * 1024,
                "sparse fixture allocated {allocated} bytes"
            );
        }
        file
    }

    #[test]
    fn exact_embedded_png_and_new_remaps_are_registered_for_nested_godot() {
        let original = Fixture::new(true);
        let candidate = Fixture::new(true);
        let before = crate::snapshot::fingerprint_all(&original.0.root).unwrap();
        let baseline = Baseline::capture(&candidate.0).unwrap();
        outputs(candidate.godot());
        let records = baseline.classify(&candidate.0, &checked()).unwrap();
        assert_eq!(records.count(), 3);
        for path in [
            "godot/assets/model_colormap.png",
            "godot/assets/model_colormap.png.import",
            "godot/assets/model.glb.import",
        ] {
            assert!(records.contains(path));
            assert!(crate::snapshot::includes(path));
        }
        assert!(!records.contains("godot/assets/authored.import"));
        records
            .verify(&candidate.0, &original.0, &checked())
            .unwrap();
        assert_eq!(
            crate::snapshot::fingerprint_all(&original.0.root).unwrap(),
            before
        );
    }

    #[test]
    fn publication_rechecks_artifact_source_and_private_cache_hashes() {
        for target in [
            "assets/model_colormap.png",
            "assets/model.glb.import",
            "assets/model.glb",
            ".godot/imported/model.scn",
        ] {
            let original = Fixture::new(false);
            let candidate = Fixture::new(false);
            let baseline = Baseline::capture(&candidate.0).unwrap();
            outputs(candidate.godot());
            let records = baseline.classify(&candidate.0, &checked()).unwrap();
            std::fs::write(candidate.0.root.join(target), b"modified after validation").unwrap();
            assert!(
                records
                    .verify(&candidate.0, &original.0, &checked())
                    .is_err(),
                "{target}"
            );
        }
    }

    #[test]
    fn exemption_never_targets_existing_or_requested_source() {
        let original = Fixture::new(false);
        let candidate = Fixture::new(false);
        let baseline = Baseline::capture(&candidate.0).unwrap();
        outputs(candidate.godot());
        let records = baseline.classify(&candidate.0, &checked()).unwrap();
        let request = serde_json::json!({"changes":[{"path":"assets/model.glb.import"}]});
        assert!(records.verify(&candidate.0, &original.0, &request).is_err());
        std::fs::write(original.0.root.join("assets/model_colormap.png"), PNG).unwrap();
        assert!(records
            .verify(&candidate.0, &original.0, &checked())
            .is_err());
        let filtered = baseline.classify(&candidate.0, &request).unwrap();
        assert_eq!(filtered.count(), 2);
        assert!(!filtered.contains("assets/model.glb.import"));
    }

    #[test]
    fn a_filename_convention_does_not_qualify_unbound_png_bytes() {
        let fixture = Fixture::new(false);
        let baseline = Baseline::capture(&fixture.0).unwrap();
        outputs(fixture.godot());
        std::fs::write(
            fixture.0.root.join("assets/model_colormap.png"),
            b"unrelated PNG bytes",
        )
        .unwrap();
        let error = baseline.classify(&fixture.0, &checked()).err().unwrap();
        assert!(error.contains("embedded GLB bytes"));
        assert!(!error.contains("unrelated PNG bytes"));
    }

    #[test]
    fn remap_requires_literal_source_and_confined_regular_destinations() {
        let good = remap_text("assets/model.glb", "model.scn", "scene", "PackedScene");
        assert_eq!(
            remap(good.as_bytes(), "res://assets/model.glb").unwrap(),
            [".godot/imported/model.scn"]
        );
        for bad in [
            good.replace("res://assets/model.glb", "res://other.glb"),
            good.replace(".godot/imported/model.scn", ".godot/imported/../secret"),
            good.replace(".godot/imported/model.scn", "assets/public.scn"),
            good.replace("dest_files=[", "dest_files=PackedStringArray("),
            good.replace("importer=\"scene\"", "importer=Object(RefCounted)"),
            good.replace("[deps]", "[deps]\nsource_file=\"res://assets/model.glb\""),
            good.replace(
                "path=\"res://.godot/imported/model.scn\"",
                "path=\"res://.godot/imported/other.scn\"",
            ),
        ] {
            assert!(remap(bad.as_bytes(), "res://assets/model.glb").is_err());
        }
        assert!(remap(
            &vec![b'x'; REMAP_BYTES as usize + 1],
            "res://assets/model.glb"
        )
        .is_err());
        let fixture = Fixture::new(false);
        let baseline = Baseline::capture(&fixture.0).unwrap();
        outputs(fixture.godot());
        std::fs::remove_file(fixture.0.root.join(".godot/imported/model.scn")).unwrap();
        assert!(baseline.classify(&fixture.0, &checked()).is_err());
    }

    #[test]
    fn glb_chunk_and_declared_buffer_ranges_are_verified() {
        let bytes = glb();
        let images = embedded_images(&bytes).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].name, "colormap");
        assert_eq!(images[0].bytes, PNG);
        for offset in [0, 4, 8, 12, 16] {
            let mut malformed = bytes.clone();
            malformed[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(embedded_images(&malformed).is_err());
        }
        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(embedded_images(&truncated).is_err());
        let mut invalid_png = bytes.clone();
        let position = invalid_png
            .windows(PNG.len())
            .position(|window| window == PNG)
            .unwrap();
        invalid_png[position] = b'x';
        assert!(embedded_images(&invalid_png).is_err());
    }

    #[test]
    fn storage_and_registration_limits_are_enforced_before_unbounded_reads() {
        let fixture = Fixture::new(false);
        let sparse = sparse_fixture_file(&fixture.0.root.join("too-large.bin"), SOURCE_BYTES + 1);
        assert!(Baseline::capture(&fixture.0).is_err());
        drop(sparse);
        std::fs::remove_file(fixture.0.root.join("too-large.bin")).unwrap();
        for index in 0..=MAX_ARTIFACTS {
            std::fs::write(
                fixture.0.root.join(format!("assets/source{index}.wav")),
                b"original source",
            )
            .unwrap();
        }
        let baseline = Baseline::capture(&fixture.0).unwrap();
        std::fs::create_dir_all(fixture.0.root.join(".godot/imported")).unwrap();
        std::fs::write(fixture.0.root.join(".godot/imported/shared.scn"), b"cache").unwrap();
        for index in 0..=MAX_ARTIFACTS {
            let source = format!("assets/source{index}.wav");
            std::fs::write(
                fixture.0.root.join(format!("{source}.import")),
                remap_text(&source, "shared.scn", "wav", "AudioStreamWAV"),
            )
            .unwrap();
        }
        let error = baseline.classify(&fixture.0, &checked()).err().unwrap();
        assert!(error.contains("registration bound"));
        let _large_cache = sparse_fixture_file(
            &fixture.0.root.join(".godot/imported/large.scn"),
            CACHE_BYTES + 1,
        );
        assert!(read_file(
            &fixture.0.root,
            ".godot/imported/large.scn",
            CACHE_BYTES,
            &mut Budget::new().unwrap(),
            false
        )
        .is_err());
    }

    #[test]
    fn cancellation_and_expiry_refuse_even_an_empty_inspection() {
        let fixture = Fixture::new(false);
        let expired = crate::operation_context::OperationContext::new(Instant::now());
        assert!(expired.scope(|| Baseline::capture(&fixture.0)).is_err());
        let mut expired_scan = Budget {
            deadline: Instant::now(),
            bytes: 0,
        };
        assert!(inventory(&fixture.0.root, &mut expired_scan).is_err());
        let cancelled = crate::operation_context::OperationContext::new(
            Instant::now() + Duration::from_secs(30),
        );
        assert!(cancelled
            .scope(|| {
                cancelled.request_cancel().unwrap();
                Baseline::capture(&fixture.0)
            })
            .unwrap()
            .is_err());
    }

    #[cfg(windows)]
    #[test]
    fn linked_cache_destination_is_refused_without_touching_outside_bytes() {
        use std::os::windows::process::CommandExt;
        let fixture = Fixture::new(false);
        let outside = Fixture::new(false);
        let protected = crate::snapshot::fingerprint_all(&outside.0.root).unwrap();
        let baseline = Baseline::capture(&fixture.0).unwrap();
        outputs(fixture.godot());
        std::fs::remove_dir_all(fixture.0.root.join(".godot/imported")).unwrap();
        // cmd's mklink parses a forward slash within an unquoted pathname as
        // a switch. Use native path separators for this Windows fixture.
        let junction = fixture.0.root.join(".godot").join("imported");
        let result = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside.0.root)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "junction fixture failed: exit={:?}, {}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr)
                .chars()
                .take(240)
                .collect::<String>()
        );
        assert!(baseline.classify(&fixture.0, &checked()).is_err());
        assert_eq!(
            crate::snapshot::fingerprint_all(&outside.0.root).unwrap(),
            protected
        );
        std::fs::remove_dir(junction).unwrap();
    }
}

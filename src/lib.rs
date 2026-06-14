use extism_pdk::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

// ─── Message types (plugin → host) ───────────────────────────────────────────

#[derive(Clone, Serialize, ToBytes)]
#[encoding(Json)]
enum PluginMessage {
    Choice {
        name: String,
        query: String,
        choices: Vec<String>,
    },
    Text {
        name: String,
        query: String,
    },
    Exit,
}

// ─── Message types (host → plugin) ───────────────────────────────────────────

#[derive(Clone, Deserialize, FromBytes)]
#[encoding(Json)]
enum HostMessage {
    Answer { name: String, value: String },
    Kill,
}

// ─── Host functions ───────────────────────────────────────────────────────────

#[host_fn]
unsafe extern "ExtismHost" {
    fn open_url(url: &str) -> ();
    fn print_msg(message: &str) -> ();
}

fn print(msg: &str) {
    unsafe { let _ = print_msg(msg); }
}

fn println(msg: &str) {
    unsafe { let _ = print_msg(&format!("{msg}\n")); }
}

// ─── Settings (persisted in KV store) ────────────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    smart_version_matching: bool,
    sync_deletions: bool,
}

fn load_settings() -> Settings {
    var::get::<Vec<u8>>("settings")
        .ok()
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_settings(s: &Settings) {
    if let Ok(bytes) = serde_json::to_vec(s) {
        let _ = var::set("settings", &bytes);
    }
}

// ─── Core / extension discovery ──────────────────────────────────────────────

fn discover_core_extensions() -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();

    let cores_path = PathBuf::from("pocket/Cores");
    let entries = match fs::read_dir(&cores_path) {
        Ok(e) => e,
        Err(_) => return map,
    };

    for entry in entries.flatten() {
        let data_json = entry.path().join("data.json");
        if !data_json.exists() { continue; }

        let raw = match fs::read_to_string(&data_json) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let parsed: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(p) => p,
            Err(_) => continue,
        };

        if let Some(slots) = parsed["data"]["data_slots"].as_array() {
            for slot in slots {
                let params = slot["parameters"].as_u64().unwrap_or(0);
                if params & 0x4 != 0 { continue; }
                if params & 0x2 != 0 { continue; }

                let extensions = match slot["extensions"].as_array() {
                    Some(e) => e,
                    None => continue,
                };

                let platform = slot["filename"]
                    .as_str()
                    .and_then(|f| {
                        let parts: Vec<&str> = f.split('/').collect();
                        if parts.len() >= 3 && parts[0] == "Assets" {
                            Some(parts[1].to_string())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| {
                        entry.file_name()
                            .to_string_lossy()
                            .split('.')
                            .last()
                            .unwrap_or("unknown")
                            .to_lowercase()
                    });

                for ext in extensions {
                    if let Some(e) = ext.as_str() {
                        let dest = format!("Assets/{}/common", platform);
                        map.insert(e.to_lowercase(), dest);
                    }
                }
            }
        }
    }

    map
}

// ─── SD card ROM index ────────────────────────────────────────────────────────

fn index_sd_card(ext_map: &HashMap<String, String>) -> HashMap<String, Vec<String>> {
    let mut index: HashMap<String, Vec<String>> = HashMap::new();

    let mut folders: Vec<String> = ext_map.values().cloned().collect();
    folders.sort();
    folders.dedup();

    for folder in folders {
        let path = PathBuf::from("pocket").join(&folder);
        let files = match fs::read_dir(&path) {
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.path().is_file())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect(),
            Err(_) => Vec::new(),
        };
        index.insert(folder, files);
    }

    index
}

// ─── Local ROM scan ───────────────────────────────────────────────────────────

#[derive(Clone)]
struct LocalRom {
    filename: String,
    extension: String,
    dest_folder: String,
}

fn scan_local_roms(ext_map: &HashMap<String, String>) -> Vec<LocalRom> {
    let mut roms = Vec::new();
    scan_dir_recursive(&PathBuf::from("host"), ext_map, &mut roms);
    roms
}

fn scan_dir_recursive(dir: &PathBuf, ext_map: &HashMap<String, String>, roms: &mut Vec<LocalRom>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir_recursive(&path, ext_map, roms);
        } else if path.is_file() {
            let ext = path.extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            if let Some(dest) = ext_map.get(ext.as_str()) {
                let filename = path.file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();

                roms.push(LocalRom {
                    filename,
                    extension: ext.to_string(),
                    dest_folder: dest.clone(),
                });
            }
        }
    }
}

// ─── Fuzzy / version matching ─────────────────────────────────────────────────

fn base_name(filename: &str) -> String {
    let stem = filename.rsplitn(2, '.').last().unwrap_or(filename);
    let mut result = stem.to_string();
    loop {
        if let Some(start) = result.find('(') {
            if let Some(end) = result[start..].find(')') {
                let token = &result[start..start + end + 1];
                let lower = token.to_lowercase();
                if lower.contains("v0.") || lower.contains("v1.") || lower.contains("v2.")
                    || lower.contains("v3.") || lower.contains("v4.") || lower.contains("v5.")
                    || lower.contains("v6.") || lower.contains("v7.") || lower.contains("v8.")
                    || lower.contains("v9.") || lower.contains("rev ") || lower.contains("version")
                {
                    result = format!("{}{}", &result[..start], &result[start + end + 1..]);
                } else {
                    break;
                }
            } else {
                break;
            }
        } else {
            break;
        }
    }
    result.trim().to_string()
}

// ─── Sync plan ────────────────────────────────────────────────────────────────

struct SyncPlan {
    to_copy: Vec<LocalRom>,
    conflicts: Vec<ConflictGroup>,
    orphans: Vec<OrphanRom>,
}

struct ConflictGroup {
    base: String,
    local: Vec<LocalRom>,
    on_card: Vec<String>,
}

struct OrphanRom {
    filename: String,
    folder: String,
}

fn build_sync_plan(
    local_roms: &[LocalRom],
    sd_index: &HashMap<String, Vec<String>>,
    settings: &Settings,
    selected_platforms: &[String],
) -> SyncPlan {
    let mut to_copy: Vec<LocalRom> = Vec::new();
    let mut conflicts: Vec<ConflictGroup> = Vec::new();

    let filtered: Vec<&LocalRom> = local_roms
        .iter()
        .filter(|r| selected_platforms.iter().any(|p| r.dest_folder.contains(p.as_str())))
        .collect();

    for rom in &filtered {
        let sd_files = sd_index.get(&rom.dest_folder).cloned().unwrap_or_default();

        if sd_files.contains(&rom.filename) {
            continue;
        }

        if settings.smart_version_matching {
            let local_base = base_name(&rom.filename);
            let close_matches: Vec<String> = sd_files
                .iter()
                .filter(|f| {
                    let card_base = base_name(f);
                    !card_base.is_empty() && card_base == local_base && **f != rom.filename
                })
                .cloned()
                .collect();

            if !close_matches.is_empty() {
                if let Some(group) = conflicts.iter_mut().find(|g| g.base == local_base) {
                    group.local.push((*rom).clone());
                } else {
                    conflicts.push(ConflictGroup {
                        base: local_base,
                        local: vec![(*rom).clone()],
                        on_card: close_matches,
                    });
                }
                continue;
            }
        }

        to_copy.push((*rom).clone());
    }

    let mut orphans: Vec<OrphanRom> = Vec::new();
    if settings.sync_deletions {
        for (folder, sd_files) in sd_index {
            if !selected_platforms.iter().any(|p| folder.contains(p.as_str())) {
                continue;
            }
            for sd_file in sd_files {
                let locally_present = local_roms.iter().any(|r| &r.filename == sd_file);
                if !locally_present {
                    orphans.push(OrphanRom {
                        filename: sd_file.clone(),
                        folder: folder.clone(),
                    });
                }
            }
        }
    }

    SyncPlan { to_copy, conflicts, orphans }
}

// ─── File operations ──────────────────────────────────────────────────────────

fn copy_rom_to_sd(rom: &LocalRom) -> Result<(), String> {
    let src = find_in_host(&PathBuf::from("host"), &rom.filename)
        .ok_or_else(|| format!("Could not find {} in host", rom.filename))?;

    let dest_dir = PathBuf::from("pocket").join(&rom.dest_folder);
    fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let dest = dest_dir.join(&rom.filename);
    fs::copy(&src, &dest).map_err(|e| e.to_string())?;
    Ok(())
}

fn find_in_host(dir: &PathBuf, filename: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_in_host(&path, filename) {
                return Some(found);
            }
        } else if path.file_name().map(|f| f.to_string_lossy().to_string()).as_deref() == Some(filename) {
            return Some(path);
        }
    }
    None
}

fn delete_from_sd(orphan: &OrphanRom) -> Result<(), String> {
    let path = PathBuf::from("pocket").join(&orphan.folder).join(&orphan.filename);
    fs::remove_file(&path).map_err(|e| e.to_string())
}

// ─── Platform helpers ─────────────────────────────────────────────────────────

fn platforms_from_ext_map(ext_map: &HashMap<String, String>) -> Vec<String> {
    let mut platforms: Vec<String> = ext_map
        .values()
        .map(|v| v.split('/').nth(1).unwrap_or("unknown").to_string())
        .collect();
    platforms.sort();
    platforms.dedup();
    platforms
}

// ─── Plugin state ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct PluginState {
    stage: String,
    settings: Settings,
    selected_platforms: Vec<String>,
    available_platforms: Vec<String>,
    to_copy: Vec<(String, String)>,
    conflicts: Vec<SerialConflict>,
    orphans: Vec<(String, String)>,
    copied: usize,
    deleted: usize,
    skipped: usize,
}

#[derive(Serialize, Deserialize)]
struct SerialConflict {
    base: String,
    local_files: Vec<String>,
    card_files: Vec<String>,
    dest_folder: String,
}

fn load_state() -> Option<PluginState> {
    var::get::<Vec<u8>>("state").ok().flatten()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn save_state(state: &PluginState) {
    if let Ok(bytes) = serde_json::to_vec(state) {
        let _ = var::set("state", &bytes);
    }
}

// ─── Entry point ──────────────────────────────────────────────────────────────

#[plugin_fn]
pub fn start() -> FnResult<PluginMessage> {
    Ok(PluginMessage::Choice {
        name: "main-menu".to_string(),
        query: "🎮 Pocket ROM Sync".to_string(),
        choices: vec![
            "Sync ROMs".to_string(),
            "Settings".to_string(),
            "Exit".to_string(),
        ],
    })
}

#[plugin_fn]
pub fn handle_response(input: HostMessage) -> FnResult<PluginMessage> {
    match input {
        HostMessage::Kill => Ok(PluginMessage::Exit),
        HostMessage::Answer { name, value } => handle_answer(&name, &value),
    }
}

// ─── Answer router ────────────────────────────────────────────────────────────

fn handle_answer(name: &str, value: &str) -> FnResult<PluginMessage> {
    match name {
        "main-menu" => match value {
            "Sync ROMs" => {
                println("Reading cores from your SD card...");
                let ext_map = discover_core_extensions();

                if ext_map.is_empty() {
                    println("No cores found. Please insert your Pocket SD card and try again.");
                    return Ok(PluginMessage::Exit);
                }

                let platforms = platforms_from_ext_map(&ext_map);

                let state = PluginState {
                    stage: "platform-select".to_string(),
                    settings: load_settings(),
                    selected_platforms: Vec::new(),
                    available_platforms: platforms.clone(),
                    to_copy: Vec::new(),
                    conflicts: Vec::new(),
                    orphans: Vec::new(),
                    copied: 0,
                    deleted: 0,
                    skipped: 0,
                };
                save_state(&state);

                if let Ok(bytes) = serde_json::to_vec(&ext_map) {
                    let _ = var::set("ext_map", &bytes);
                }

                let mut choices: Vec<String> = platforms.iter().map(|p| p.to_uppercase()).collect();
                choices.push("── Select All ──".to_string());
                choices.push("── Continue ──".to_string());

                Ok(PluginMessage::Choice {
                    name: "platform-select".to_string(),
                    query: "Which platforms would you like to sync?\n(Select platforms then choose Continue)".to_string(),
                    choices,
                })
            }

            "Settings" => {
                let settings = load_settings();
                Ok(PluginMessage::Choice {
                    name: "settings-menu".to_string(),
                    query: "Settings".to_string(),
                    choices: vec![
                        format!("Smart version matching: {}", if settings.smart_version_matching { "ON ✓" } else { "OFF" }),
                        format!("Sync deletions from local library: {}", if settings.sync_deletions { "ON ✓" } else { "OFF" }),
                        "Back".to_string(),
                    ],
                })
            }

            "Exit" => Ok(PluginMessage::Exit),
            _ => Ok(PluginMessage::Exit),
        },

        "settings-menu" => {
            let mut settings = load_settings();
            if value.starts_with("Smart version matching") {
                settings.smart_version_matching = !settings.smart_version_matching;
                println(&format!(
                    "Smart version matching: {}",
                    if settings.smart_version_matching {
                        "ON — ROMs with similar names but different version numbers will be flagged for review."
                    } else {
                        "OFF — Only exact filename matches will be skipped."
                    }
                ));
            } else if value.starts_with("Sync deletions") {
                settings.sync_deletions = !settings.sync_deletions;
                println(&format!(
                    "Sync deletions: {}",
                    if settings.sync_deletions {
                        "ON — ROMs missing from your local library will be listed for optional removal."
                    } else {
                        "OFF — Nothing will be removed from your SD card."
                    }
                ));
            }
            save_settings(&settings);

            Ok(PluginMessage::Choice {
                name: "settings-menu".to_string(),
                query: "Settings".to_string(),
                choices: vec![
                    format!("Smart version matching: {}", if settings.smart_version_matching { "ON ✓" } else { "OFF" }),
                    format!("Sync deletions from local library: {}", if settings.sync_deletions { "ON ✓" } else { "OFF" }),
                    "Back".to_string(),
                ],
            })
        }

        "platform-select" => {
            let mut state = load_state().unwrap_or_else(|| PluginState {
                stage: "platform-select".to_string(),
                settings: load_settings(),
                selected_platforms: Vec::new(),
                available_platforms: Vec::new(),
                to_copy: Vec::new(),
                conflicts: Vec::new(),
                orphans: Vec::new(),
                copied: 0,
                deleted: 0,
                skipped: 0,
            });

            if value == "── Select All ──" {
                state.selected_platforms = state.available_platforms.clone();
                println("All platforms selected.");
            } else if value == "── Continue ──" {
                if state.selected_platforms.is_empty() {
                    let mut choices: Vec<String> = state.available_platforms.iter().map(|p| p.to_uppercase()).collect();
                    choices.push("── Select All ──".to_string());
                    choices.push("── Continue ──".to_string());
                    save_state(&state);
                    return Ok(PluginMessage::Choice {
                        name: "platform-select".to_string(),
                        query: "Please select at least one platform to sync.".to_string(),
                        choices,
                    });
                }

                println("Scanning your ROM library — this may take a moment for large collections...");

                let ext_map: HashMap<String, String> = var::get::<Vec<u8>>("ext_map")
                    .ok()
                    .flatten()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                    .unwrap_or_default();

                let local_roms = scan_local_roms(&ext_map);
                let sd_index = index_sd_card(&ext_map);
                let plan = build_sync_plan(&local_roms, &sd_index, &state.settings, &state.selected_platforms);

                let copy_count = plan.to_copy.len();
                let conflict_count = plan.conflicts.len();
                let orphan_count = plan.orphans.len();

                state.to_copy = plan.to_copy.iter().map(|r| (r.filename.clone(), r.dest_folder.clone())).collect();
                state.conflicts = plan.conflicts.iter().map(|g| SerialConflict {
                    base: g.base.clone(),
                    local_files: g.local.iter().map(|r| r.filename.clone()).collect(),
                    card_files: g.on_card.clone(),
                    dest_folder: g.local.first().map(|r| r.dest_folder.clone()).unwrap_or_default(),
                }).collect();
                state.orphans = plan.orphans.iter().map(|o| (o.filename.clone(), o.folder.clone())).collect();
                state.stage = "preview".to_string();
                save_state(&state);

                return Ok(PluginMessage::Choice {
                    name: "preview-confirm".to_string(),
                    query: format!(
                        "Ready to sync\n\n  {} ROM(s) to copy\n  {} version conflict(s) to review\n  {} SD-only file(s) found\n\nContinue?",
                        copy_count, conflict_count, orphan_count
                    ),
                    choices: vec!["Yes, start sync".to_string(), "Cancel".to_string()],
                });
            } else {
                let platform_lower = value.to_lowercase();
                if state.selected_platforms.contains(&platform_lower) {
                    state.selected_platforms.retain(|p| p != &platform_lower);
                } else {
                    state.selected_platforms.push(platform_lower);
                }
            }

            save_state(&state);

            let selected_display = if state.selected_platforms.is_empty() {
                "None selected".to_string()
            } else {
                state.selected_platforms.iter().map(|p| p.to_uppercase()).collect::<Vec<_>>().join(", ")
            };

            let mut choices: Vec<String> = state.available_platforms.iter().map(|p| {
                if state.selected_platforms.contains(&p.to_lowercase()) {
                    format!("{} ✓", p.to_uppercase())
                } else {
                    p.to_uppercase()
                }
            }).collect();
            choices.push("── Select All ──".to_string());
            choices.push("── Continue ──".to_string());

            Ok(PluginMessage::Choice {
                name: "platform-select".to_string(),
                query: format!("Which platforms would you like to sync?\nSelected: {}", selected_display),
                choices,
            })
        }

        "preview-confirm" => {
            if value == "Cancel" {
                println("Sync cancelled.");
                return Ok(PluginMessage::Exit);
            }

            let mut state = match load_state() {
                Some(s) => s,
                None => return Ok(PluginMessage::Exit),
            };

            if !state.conflicts.is_empty() && state.settings.smart_version_matching {
                return show_next_conflict(&state);
            }

            do_copy(&mut state)
        }

        "conflict-resolve" => {
            let mut state = match load_state() {
                Some(s) => s,
                None => return Ok(PluginMessage::Exit),
            };

            if !state.conflicts.is_empty() {
                let conflict = state.conflicts.remove(0);

                if value.starts_with("Copy new: ") {
                    let filename = value.trim_start_matches("Copy new: ").to_string();
                    state.to_copy.push((filename, conflict.dest_folder.clone()));
                    for old in &conflict.card_files {
                        let _ = fs::remove_file(
                            PathBuf::from("pocket").join(&conflict.dest_folder).join(old)
                        );
                        state.deleted += 1;
                    }
                } else if value == "Skip this conflict" {
                    state.skipped += 1;
                }
            }

            save_state(&state);

            if !state.conflicts.is_empty() {
                return show_next_conflict(&state);
            }

            do_copy(&mut state)
        }

        "orphan-review" => {
            let mut state = match load_state() {
                Some(s) => s,
                None => return Ok(PluginMessage::Exit),
            };

            if value.starts_with("Delete: ") {
                let filename = value.trim_start_matches("Delete: ").to_string();
                if let Some(pos) = state.orphans.iter().position(|(f, _)| f == &filename) {
                    let (fname, folder) = state.orphans.remove(pos);
                    let orphan = OrphanRom { filename: fname, folder };
                    match delete_from_sd(&orphan) {
                        Ok(_) => { state.deleted += 1; println(&format!("Removed: {}", orphan.filename)); }
                        Err(e) => println(&format!("Failed to remove {}: {}", orphan.filename, e)),
                    }
                }
                save_state(&state);

                if !state.orphans.is_empty() {
                    return show_orphan_screen(&state);
                }
            }

            show_report(&state)
        }

        _ => Ok(PluginMessage::Exit),
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn show_next_conflict(state: &PluginState) -> FnResult<PluginMessage> {
    let conflict = &state.conflicts[0];
    let mut choices: Vec<String> = conflict.card_files.iter()
        .map(|f| format!("Keep on card: {}", f))
        .collect();
    for lf in &conflict.local_files {
        choices.push(format!("Copy new: {}", lf));
    }
    choices.push("Skip this conflict".to_string());

    Ok(PluginMessage::Choice {
        name: "conflict-resolve".to_string(),
        query: format!(
            "Version conflict: \"{}\"\n\nOn SD card:\n{}\n\nIn your library:\n{}\n\nWhat would you like to do?",
            conflict.base,
            conflict.card_files.join("\n"),
            conflict.local_files.join("\n")
        ),
        choices,
    })
}

fn do_copy(state: &mut PluginState) -> FnResult<PluginMessage> {
    println(&format!("Copying {} ROM(s) to your SD card...", state.to_copy.len()));

    let roms_to_copy: Vec<LocalRom> = state.to_copy.iter().map(|(filename, dest_folder)| {
        let extension = filename.rsplitn(2, '.').next().unwrap_or("").to_string();
        LocalRom {
            filename: filename.clone(),
            extension,
            dest_folder: dest_folder.clone(),
        }
    }).collect();

    for rom in &roms_to_copy {
        match copy_rom_to_sd(rom) {
            Ok(_) => {
                state.copied += 1;
                print("█");
            }
            Err(e) => println(&format!("\nFailed to copy {}: {}", rom.filename, e)),
        }
    }
    println("");
    state.to_copy.clear();
    save_state(state);

    if !state.orphans.is_empty() && state.settings.sync_deletions {
        return show_orphan_screen(state);
    }

    show_report(state)
}

fn show_orphan_screen(state: &PluginState) -> FnResult<PluginMessage> {
    let mut choices: Vec<String> = state.orphans.iter()
        .map(|(f, _)| format!("Delete: {}", f))
        .collect();
    choices.push("Done".to_string());

    Ok(PluginMessage::Choice {
        name: "orphan-review".to_string(),
        query: format!(
            "These {} file(s) are on your SD card but not in your local library.\nSelect any you'd like to remove:",
            state.orphans.len()
        ),
        choices,
    })
}

fn show_report(state: &PluginState) -> FnResult<PluginMessage> {
    println(&format!(
        "\n✓ Sync complete\n  Copied:  {}\n  Removed: {}\n  Skipped: {}",
        state.copied, state.deleted, state.skipped
    ));
    Ok(PluginMessage::Exit)
}
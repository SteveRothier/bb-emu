use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

struct StubState {
    child: Mutex<Option<Child>>,
}

#[derive(Serialize)]
struct StubStatus {
    running: bool,
    stub_ready: bool,
    stub_path: Option<String>,
}

fn now_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

fn log_file(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_log_dir().ok().map(|d| d.join("bb-emu.log"))
}

fn append_log(app: &AppHandle, line: &str) {
    let Some(path) = log_file(app) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{}] {line}", now_stamp());
    }
}

fn stub_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(resource) = app.path().resource_dir() {
        let packaged = resource.join("bb-mp-stub");
        if packaged.join("server.js").exists() {
            return Ok(packaged);
        }
        if resource.join("server.js").exists() {
            return Ok(resource);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join("bb-mp-stub");
            if beside.join("server.js").exists() {
                return Ok(beside);
            }
        }
    }

    let from_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("bb-mp-stub");
    let from_manifest = from_manifest
        .canonicalize()
        .map_err(|e| format!("bb-mp-stub introuvable: {e}"))?;
    if !from_manifest.join("server.js").exists() {
        return Err(format!(
            "server.js manquant dans {}",
            from_manifest.display()
        ));
    }
    Ok(from_manifest)
}

fn is_child_running(child: &mut Child) -> bool {
    match child.try_wait() {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(_) => false,
    }
}

fn kill_stub(app: &AppHandle, state: &StubState) -> Result<bool, String> {
    let mut guard = state.child.lock().map_err(|_| "lock stub".to_string())?;
    if let Some(mut child) = guard.take() {
        let _ = child.kill();
        let _ = child.wait();
        append_log(app, "stub arrêté");
        return Ok(true);
    }
    Ok(false)
}

fn spawn_stub(app: &AppHandle, state: &StubState, port: u16, lobby_name: &str) -> Result<(), String> {
    {
        let mut guard = state.child.lock().map_err(|_| "lock stub".to_string())?;
        if let Some(child) = guard.as_mut() {
            if is_child_running(child) {
                return Err("Stub déjà en cours".to_string());
            }
            *guard = None;
        }
    }

    let dir = stub_dir(app)?;
    let log_path = log_file(app);
    if let Some(parent) = log_path.as_ref().and_then(|p| p.parent()) {
        let _ = fs::create_dir_all(parent);
    }

    let (stdout, stderr) = if let Some(ref path) = log_path {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("Impossible d'ouvrir le log: {e}"))?;
        let file2 = file
            .try_clone()
            .map_err(|e| format!("Impossible de cloner le log: {e}"))?;
        (Stdio::from(file), Stdio::from(file2))
    } else {
        (Stdio::null(), Stdio::null())
    };

    let mut cmd = Command::new("node");
    cmd.current_dir(&dir)
        .arg("server.js")
        .env("BB_MP_PORT", port.to_string())
        .env("BB_LOBBY_NAME", lobby_name)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Impossible de démarrer node server.js: {e}"))?;

    append_log(
        app,
        &format!(
            "stub démarré port={port} lobby={lobby_name} dir={}",
            dir.display()
        ),
    );

    let mut guard = state.child.lock().map_err(|_| "lock stub".to_string())?;
    *guard = Some(child);
    Ok(())
}

fn launch_game_exe(app: &AppHandle, game_path: &str) -> Result<(), String> {
    let path = Path::new(game_path);
    if game_path.trim().is_empty() {
        return Err("Chemin du jeu non défini".to_string());
    }
    if !path.exists() {
        return Err(format!("Exécutable introuvable: {game_path}"));
    }

    let workdir = path.parent().unwrap_or(path);
    Command::new(path)
        .current_dir(workdir)
        .spawn()
        .map_err(|e| format!("Échec lancement jeu: {e}"))?;
    append_log(app, &format!("jeu lancé: {game_path}"));
    Ok(())
}

fn push_unique(out: &mut Vec<PathBuf>, path: PathBuf) {
    if path.exists() && !out.iter().any(|p| p == &path) {
        out.push(path);
    }
}

fn collect_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let user = std::env::var("USERPROFILE").unwrap_or_default();

    for base in [pf, pf86] {
        push_unique(&mut roots, PathBuf::from(&base).join("Ubisoft"));
        push_unique(&mut roots, PathBuf::from(&base).join("Ubisoft Game Launcher\\games"));
        push_unique(&mut roots, PathBuf::from(&base).join("Steam\\steamapps\\common"));
        push_unique(
            &mut roots,
            PathBuf::from(&base).join("Epic Games\\BlitzBrigade"),
        );
    }
    if !local.is_empty() {
        push_unique(
            &mut roots,
            PathBuf::from(&local).join("Ubisoft Game Launcher\\games"),
        );
    }
    if !user.is_empty() {
        push_unique(&mut roots, PathBuf::from(&user).join("Games"));
        push_unique(&mut roots, PathBuf::from(&user).join("Desktop"));
    }
    roots
}

fn walk_find_exe(root: &Path, depth: u8, out: &mut Option<PathBuf>) {
    if out.is_some() || depth == 0 || !root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.eq_ignore_ascii_case("BlitzBrigade.exe")
                    || name.eq_ignore_ascii_case("Blitz Brigade.exe")
                {
                    *out = Some(path);
                    return;
                }
            }
        } else if path.is_dir() {
            walk_find_exe(&path, depth - 1, out);
            if out.is_some() {
                return;
            }
        }
    }
}

#[tauri::command]
fn find_game(app: AppHandle) -> Option<String> {
    let mut found = None;
    for root in collect_search_roots() {
        walk_find_exe(&root, 4, &mut found);
        if found.is_some() {
            break;
        }
    }
    if let Some(ref p) = found {
        append_log(&app, &format!("jeu détecté: {}", p.display()));
        Some(p.display().to_string())
    } else {
        append_log(&app, "jeu non détecté automatiquement");
        None
    }
}

#[tauri::command]
fn read_recent_logs(app: AppHandle) -> Result<String, String> {
    let path = log_file(&app).ok_or_else(|| "dossier logs indisponible".to_string())?;
    if !path.exists() {
        return Ok(String::new());
    }
    let raw = fs::read_to_string(&path).map_err(|e| format!("lecture log: {e}"))?;
    let lines: Vec<&str> = raw.lines().collect();
    let start = lines.len().saturating_sub(40);
    Ok(lines[start..].join("\n"))
}

#[tauri::command]
fn get_status(app: AppHandle, state: State<'_, StubState>) -> StubStatus {
    let running = state
        .child
        .lock()
        .ok()
        .and_then(|mut g| {
            let alive = g.as_mut().map(is_child_running).unwrap_or(false);
            if !alive {
                *g = None;
            }
            Some(alive)
        })
        .unwrap_or(false);

    match stub_dir(&app) {
        Ok(dir) => StubStatus {
            running,
            stub_ready: true,
            stub_path: Some(dir.display().to_string()),
        },
        Err(_) => StubStatus {
            running,
            stub_ready: false,
            stub_path: None,
        },
    }
}

#[tauri::command]
fn stop_stub(app: AppHandle, state: State<'_, StubState>) -> Result<String, String> {
    if kill_stub(&app, &state)? {
        Ok("Arrêté".to_string())
    } else {
        Ok("Déjà arrêté".to_string())
    }
}

#[tauri::command]
fn launch_all(
    app: AppHandle,
    state: State<'_, StubState>,
    port: u16,
    lobby_name: String,
    game_path: String,
) -> Result<String, String> {
    {
        let mut guard = state.child.lock().map_err(|_| "lock stub".to_string())?;
        if let Some(child) = guard.as_mut() {
            if is_child_running(child) {
                return Err("Stub déjà en ligne".to_string());
            }
            *guard = None;
        }
    }

    spawn_stub(&app, &state, port, &lobby_name)?;

    match launch_game_exe(&app, &game_path) {
        Ok(()) => Ok(format!("En ligne — lobby {lobby_name} :{port}")),
        Err(e) => {
            append_log(&app, &format!("jeu échec: {e}"));
            Ok(format!("Stub :{port} OK — {e}"))
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(StubState {
            child: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            stop_stub,
            launch_all,
            find_game,
            read_recent_logs
        ])
        .setup(|app| {
            append_log(app.handle(), "launcher démarré");
            let _ = stub_dir(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building BB EMU")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app_handle.state::<StubState>();
                let _ = kill_stub(app_handle, &state);
            }
        });
}

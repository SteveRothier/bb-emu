use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

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

fn stub_dir(app: &AppHandle) -> Result<PathBuf, String> {
    // Packaged: resource_dir/bb-mp-stub
    if let Ok(resource) = app.path().resource_dir() {
        let packaged = resource.join("bb-mp-stub");
        if packaged.join("server.js").exists() {
            return Ok(packaged);
        }
        // Sometimes resources are flattened
        if resource.join("server.js").exists() {
            return Ok(resource);
        }
    }

    // Next to the exe (portable)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join("bb-mp-stub");
            if beside.join("server.js").exists() {
                return Ok(beside);
            }
        }
    }

    // Dev: bb-emu/src-tauri -> ../bb-mp-stub
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

fn kill_stub(state: &StubState) -> Result<bool, String> {
    let mut guard = state.child.lock().map_err(|_| "lock stub".to_string())?;
    if let Some(mut child) = guard.take() {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(true);
    }
    Ok(false)
}

fn spawn_stub(app: &AppHandle, state: &StubState, port: u16, lobby_name: &str) -> Result<(), String> {
    // Already running?
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
    let mut cmd = Command::new("node");
    cmd.current_dir(&dir)
        .arg("server.js")
        .env("BB_MP_PORT", port.to_string())
        .env("BB_LOBBY_NAME", lobby_name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Impossible de démarrer node server.js: {e}"))?;

    let mut guard = state.child.lock().map_err(|_| "lock stub".to_string())?;
    *guard = Some(child);
    Ok(())
}

fn launch_game_exe(game_path: &str) -> Result<(), String> {
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
    Ok(())
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
fn stop_stub(state: State<'_, StubState>) -> Result<String, String> {
    if kill_stub(&state)? {
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
    // If already running → refuse double start (UI should call stop)
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

    match launch_game_exe(&game_path) {
        Ok(()) => Ok(format!("En ligne — stub :{port}")),
        Err(e) => {
            // Stub is up even if game failed
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
        .invoke_handler(tauri::generate_handler![get_status, stop_stub, launch_all])
        .setup(|app| {
            let _ = stub_dir(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building BB EMU")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app_handle.state::<StubState>();
                let _ = kill_stub(&state);
            }
        });
}

#![allow(dead_code)] // probe CamMgr : helpers touch/stomp temporairement dormants

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};


#[derive(Clone)]
struct LobbyConfig {
    host_name: String,
    /// Index MP level (name[0] du 0x02) — mode×carte.
    level_id: u8,
    map_id: u8,
    max_players: u8,
    game_mode: u8,
}

impl Default for LobbyConfig {
    fn default() -> Self {
        Self {
            host_name: "MaPartie".into(),
            level_id: 0, // legacy / logs (mode*6+map)
            map_id: 0,   // Cathédrale (wire map 0)
            max_players: 8,
            game_mode: 0, // Domination
        }
    }
}

struct StubState {
    child: Mutex<Option<Child>>,
    lobby: Mutex<LobbyConfig>,
}

#[derive(Serialize)]
struct StubStatus {
    running: bool,
    stub_ready: bool,
    stub_path: Option<String>,
    /// Compteurs cave inject (vide si jeu non patché).
    join_debug: Option<String>,
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

pub(crate) fn append_log(app: &AppHandle, line: &str) {
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

fn stub_is_running(state: &StubState) -> bool {
    state
        .child
        .lock()
        .ok()
        .and_then(|mut guard| guard.as_mut().map(is_child_running))
        .unwrap_or(false)
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
        kill_process_tree(&mut child);
        append_log(app, "stub arrêté");
        return Ok(true);
    }
    Ok(false)
}

fn kill_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn node_executable() -> Result<PathBuf, String> {
    let output = Command::new("where.exe")
        .arg("node.exe")
        .output()
        .map_err(|e| format!("node introuvable: {e}"))?;
    if !output.status.success() {
        return Err("node.exe introuvable".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let found = text.lines().map(str::trim).find(|line| {
        !line.is_empty() && !line.to_ascii_lowercase().contains("\\windowsapps\\")
    });
    let Some(path) = found else {
        return Err("node.exe introuvable hors du stub WindowsApps".to_string());
    };
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(format!("node.exe absent: {}", path.display()));
    }
    Ok(path)
}

fn stop_node_on_udp_port(port: u16) -> Vec<u32> {
    let Ok(output) = Command::new("netstat").args(["-ano", "-p", "udp"]).output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let marker = format!(":{port}");
    let mut stopped = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(proto) = parts.next() else {
            continue;
        };
        if !proto.eq_ignore_ascii_case("UDP") {
            continue;
        }
        let Some(local) = parts.next() else {
            continue;
        };
        if !local.ends_with(&marker) {
            continue;
        }
        let Some(pid) = parts.last().and_then(|token| token.parse::<u32>().ok()) else {
            continue;
        };
        if !process_is_node(pid) {
            continue;
        }
        let killed = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if killed {
            stopped.push(pid);
        }
    }
    stopped
}

fn process_is_node(pid: u32) -> bool {
    let Ok(output) = Command::new("tasklist")
        .args([
            "/FI",
            &format!("PID eq {pid}"),
            "/FO",
            "CSV",
            "/NH",
        ])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&output.stdout)
        .to_ascii_lowercase()
        .contains("\"node.exe\"")
}

fn normalize_lobby_name(lobby_name: &str) -> String {
    let trimmed = lobby_name.trim();
    let name = if trimmed.is_empty() {
        "MaPartie"
    } else {
        trimmed
    };
    name.chars().take(16).collect::<String>()
}

fn spawn_stub(
    app: &AppHandle,
    state: &StubState,
    port: u16,
    lobby_name: &str,
    level_id: u8,
    map_id: u8,
    max_players: u8,
    game_mode: u8,
) -> Result<(), String> {
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
    let server = dir.join("server.js");
    if !server.exists() {
        return Err(format!("server.js manquant dans {}", dir.display()));
    }
    let lobby = normalize_lobby_name(lobby_name);
    let max_players = if max_players == 0 { 8 } else { max_players };
    let cfg = LobbyConfig {
        host_name: lobby.clone(),
        level_id,
        map_id,
        max_players,
        game_mode,
    };

    let stopped = stop_node_on_udp_port(port);
    if !stopped.is_empty() {
        append_log(
            app,
            &format!("node du bureau arrêté sur :{port} pids={stopped:?}"),
        );
    }

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

    let node = node_executable()?;
    let mut cmd = desktop_stub_command(&node, &server, port, &cfg, log_path.as_deref());
    cmd.stdin(Stdio::null()).stdout(stdout).stderr(stderr);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("Impossible de démarrer le stub: {e}"))?;

    // Commit lobby seulement après spawn réussi.
    {
        let mut lobby_guard = state.lobby.lock().map_err(|_| "lock lobby".to_string())?;
        *lobby_guard = cfg.clone();
    }

    append_log(
        app,
        &format!(
            "stub desktop port={port} lobby={} level={} map={} mode={} max={} node={} server={}",
            cfg.host_name,
            cfg.level_id,
            cfg.map_id,
            cfg.game_mode,
            cfg.max_players,
            node.display(),
            server.display()
        ),
    );

    let mut guard = state.child.lock().map_err(|_| "lock stub".to_string())?;
    *guard = Some(child);
    Ok(())
}

const STORE_PACKAGE_NAME: &str = "GAMELOFTSA.BlitzBrigade";
const STORE_APP_ID: &str = "GAMELOFTSA.BlitzBrigade_0pp20fcewvvtj!App";

/// Stub Node hors conteneur UWP — bind 0.0.0.0:7891, joignable via IPv4 LAN.
fn desktop_stub_command(
    node: &Path,
    server: &Path,
    port: u16,
    lobby: &LobbyConfig,
    log: Option<&Path>,
) -> Command {
    let mut cmd = Command::new(node);
    cmd.arg(server)
        .arg("--port")
        .arg(port.to_string())
        .arg("--lobby")
        .arg(&lobby.host_name)
        .arg("--level")
        .arg(lobby.level_id.to_string())
        .arg("--map")
        .arg(lobby.map_id.to_string())
        .arg("--max-players")
        .arg(lobby.max_players.to_string())
        .arg("--mode")
        .arg(lobby.game_mode.to_string());
    if let Some(log) = log {
        cmd.arg("--log").arg(log);
    }
    cmd
}

fn is_game_exe_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("BlitzBrigade.exe")
        || name.eq_ignore_ascii_case("Blitz Brigade.exe")
        || name.eq_ignore_ascii_case("InfamousHeroesEntry.exe")
}

fn is_store_game_path(game_path: &str) -> bool {
    let lower = game_path.to_ascii_lowercase();
    lower.contains("windowsapps")
        && (lower.contains("gameloftsa.blitzbrigade") || lower.contains("infamousheroesentry.exe"))
}

fn launch_store_process(lobby: &LobbyConfig) -> Result<String, String> {
    let target = format!("shell:AppsFolder\\{STORE_APP_ID}");
    Command::new("explorer.exe")
        .arg(&target)
        .spawn()
        .map_err(|e| format!("Échec lancement Store: {e}"))?;
    apply_sizechanged_jump(lobby)
}

fn launch_store_app(app: &AppHandle, lobby: &LobbyConfig) -> Result<(), String> {
    let target = format!("shell:AppsFolder\\{STORE_APP_ID}");
    append_log(
        app,
        &format!(
            "lancement Store: {target} lobby={} level={} map={} mode={} max={}",
            lobby.host_name, lobby.level_id, lobby.map_id, lobby.game_mode, lobby.max_players
        ),
    );
    let detail = launch_store_process(lobby)?;
    append_log(app, &format!("saut SizeChanged écrit: {detail}"));
    Ok(())
}

#[cfg(windows)]
fn apply_sizechanged_jump(lobby: &LobbyConfig) -> Result<String, String> {
    const SITE_RVA: u32 = 0x13A3B9;
    const CAVE_RVA: u32 = 0x13AFD1;
    const PAGE_RVA: u32 = 0x13A000;
    const EXPECT: [u8; 5] = [0x8B, 0x45, 0x08, 0x8B, 0x4A];
    const JUMP: [u8; 5] = [0xE9, 0x13, 0x0C, 0x00, 0x00];
    const CAVE: [u8; 24] = [
        0x85, 0xD2, 0x74, 0x12, 0x8B, 0x45, 0x08, 0x8B, 0x4A, 0x08, 0x89, 0x41, 0x30, 0x8B, 0x4A,
        0x08, 0x8B, 0x45, 0x0C, 0x89, 0x41, 0x34, 0x5D, 0xC3,
    ];

    enable_debug_privilege();
    let started = Instant::now();
    let mut last_err = 0u32;
    while started.elapsed() < Duration::from_secs(25) {
        let Some(pid) = find_game_pid() else {
            last_err = last_error();
            sleep(Duration::from_millis(10));
            continue;
        };
        match try_patch(
            pid,
            SITE_RVA,
            CAVE_RVA,
            PAGE_RVA,
            &EXPECT,
            &JUMP,
            &CAVE,
            lobby,
        ) {
            PatchTry::Done(detail) => return Ok(detail),
            PatchTry::Wait(err) => {
                last_err = err;
                sleep(Duration::from_millis(10));
            }
            PatchTry::Fail(message) => return Err(message),
        }
    }
    Err(format!("processus introuvable (code {last_err})"))
}

#[cfg(not(windows))]
fn apply_sizechanged_jump(_lobby: &LobbyConfig) -> Result<String, String> {
    Err("Windows requis pour le saut SizeChanged".to_string())
}

#[cfg(windows)]
enum PatchTry {
    Done(String),
    Wait(u32),
    Fail(String),
}

#[cfg(windows)]
fn try_patch(
    pid: u32,
    site_rva: u32,
    cave_rva: u32,
    page_rva: u32,
    expect: &[u8; 5],
    jump: &[u8; 5],
    cave: &[u8; 24],
    lobby: &LobbyConfig,
) -> PatchTry {
    let process = unsafe { OpenProcess(0x0438, 0, pid) };
    if process.is_null() {
        return PatchTry::Wait(last_error());
    }
    let process = ProcessHandle(process);
    let mut peb32: u64 = 0;
    let mut returned = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            process.0,
            26,
            &mut peb32 as *mut u64 as *mut _,
            8,
            &mut returned,
        )
    };
    if status != 0 || peb32 == 0 {
        return PatchTry::Wait(status as u32);
    }
    let mut image = 0u32;
    if !read_exact(process.0, peb32 + 8, &mut image).is_ok() || image == 0 {
        return PatchTry::Wait(last_error());
    }
    let mut site = [0u8; 5];
    if read_exact(process.0, (image + site_rva) as u64, &mut site).is_err() {
        return PatchTry::Wait(last_error());
    }
    if &site != jump {
        if &site != expect {
            if site == [0, 0, 0, 0, 0] {
                return PatchTry::Wait(0);
            }
            return PatchTry::Fail(format!(
                "octets inattendus au RVA {site_rva:X}: {}",
                hex_bytes(&site)
            ));
        }
        let page = (image + page_rva) as u64;
        let write_result = with_process_threads_suspended(pid, || {
            let mut old = 0u32;
            if unsafe { VirtualProtectEx(process.0, page, 0x1000, 0x40, &mut old) } == 0 {
                return Err(format!(
                    "protection de la page refusée (code {})",
                    last_error()
                ));
            }
            // Cave avant le saut : un thread ne doit jamais jumper vers 0xCC.
            if write_exact(process.0, (image + cave_rva) as u64, cave).is_err() {
                let err = last_error();
                unsafe { VirtualProtectEx(process.0, page, 0x1000, old, &mut old) };
                return Err(format!("écriture de la cave refusée (code {err})"));
            }
            if write_exact(process.0, (image + site_rva) as u64, jump).is_err() {
                let err = last_error();
                unsafe { VirtualProtectEx(process.0, page, 0x1000, old, &mut old) };
                return Err(format!("écriture du saut refusée (code {err})"));
            }
            unsafe { VirtualProtectEx(process.0, page, 0x1000, old, &mut old) };
            Ok(())
        });
        if let Err(message) = write_result {
            return PatchTry::Fail(message);
        }
        if read_exact(process.0, (image + site_rva) as u64, &mut site).is_err() {
            return PatchTry::Fail(format!("relecture refusée (code {})", last_error()));
        }
        if &site != jump {
            return PatchTry::Fail(format!("relecture inattendue: {}", hex_bytes(&site)));
        }
    }
    if let Err(message) = open_menu_sections(process.0, pid, image, lobby) {
        return PatchTry::Fail(message);
    }
    // LocalState only — jamais WindowsApps.
    let _ = clear_settings_gyro_flag();
    if let Ok(mut g) = FPS_LOOK_STATE.lock() {
        *g = Some((pid, image));
    }
    PatchTry::Done(format!(
        "pid={pid} base=0x{image:X} site={}",
        hex_bytes(&site)
    ))
}

#[cfg(windows)]
fn open_menu_sections(
    process: *mut std::ffi::c_void,
    pid: u32,
    image: u32,
    lobby: &LobbyConfig,
) -> Result<(), String> {
    with_process_threads_suspended(pid, || {
        open_menu_sections_inner(process, image, lobby)
    })
}

#[cfg(windows)]
fn open_menu_sections_inner(
    process: *mut std::ffi::c_void,
    image: u32,
    lobby: &LobbyConfig,
) -> Result<(), String> {
    // FUN_00653280 quitte tout de suite si le login fédération est à 0.
    // Sans ce saut, Boutique, Amis, Packs, Multijoueur et classements s'ouvrent.
    patch_code(
        process,
        image,
        0x2532C0,
        &[0x0F, 0x84, 0xF6, 0x05, 0x00, 0x00],
        &[0x90, 0x90, 0x90, 0x90, 0x90, 0x90],
    )?;
    // Menu_IsLoggedIn répondait non, et l'interface affichait l'échec de connexion.
    patch_code(process, image, 0x39498A, &[0x32, 0xC0], &[0xB0, 0x01])?;
    // Partie rapide est ChangeState: Quick. Sans login, le saut ne crée aucun état
    // et l'écran de recherche reste vide. On le renvoie vers LocalWifiJoin.
    patch_code(
        process,
        image,
        0x4627AC,
        &[0x0F, 0x84, 0x2E, 0x03, 0x00, 0x00],
        &[0x0F, 0x84, 0x84, 0xFF, 0xFF, 0xFF],
    )?;
    // Le client local envoie le 0x01 à 255.255.255.255. Le loopback UWP ne
    // livre pas le 0x02 retour. On pointe Discover vers l'IPv4 LAN (hairpin).
    patch_discover_to_lan(process, image)?;
    // Filtre « before snapshot » : jette type>0x15 tant que 0x15 absent.
    // Bypass OK si la phase A n'injecte PAS 0x16 (handler AV sans tables).
    patch_code(process, image, 0x2964EB, &[0x75, 0x3B], &[0xEB, 0x3B])?;
    // Respawn local sans caméra active → mov [mp+0xac98],1 → Connexion perdue (raison 7).
    // Nop tant que le snapshot/respawn n'installe pas encore une caméra type 2.
    patch_code(
        process,
        image,
        0x309F3E,
        &[0xC6, 0x80, 0x98, 0xAC, 0x00, 0x00, 0x01],
        &[0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90],
    )?;
    // Sans arme/loadout, Respawn laisse mgr+0x178=-1 → vue figée vers le bas + AV ~4s.
    // Cave : SetCamera(0x2755) puis réactiver l'index type 2.
    install_respawn_camera_fix(process, image)?;
    // Pas de patch look/sticks ici — ça plantait le menu. Look = watcher match-only.
    // HUD ignore settings+8 si [hud+0x8C4]!=0 — nop la sortie anticipée.
    patch_code(process, image, 0x2FA798, &[0x75, 0x47], &[0x90, 0x90])?;
    // EnableGyroscope → toujours false.
    patch_code(
        process,
        image,
        0x2573EA,
        &[0x0F, 0xB6, 0x40, 0x4E, 0x50],
        &[0x33, 0xC0, 0x50, 0x90, 0x90],
    )?;
    // Garde look-scale : renvoie 1.0 si contrôleur NULL (comme en entraînement).
    patch_code(process, image, 0x3374B0, &[0x74, 0x07], &[0x90, 0x90])?;
    // Garde GetActiveController : index < 0 → false (évite AV [esi+0x84]).
    patch_code(process, image, 0x306754, &[0x78, 0x0F], &[0x78, 0x60])?;
    // Caméra index < 0 : ne pas call 0x736F00(ecx=0) → AV. Sauter au ret OK.
    patch_code(process, image, 0x30678F, &[0x78, 0x18], &[0x78, 0x1F])?;
    // Watchdog 60 s (jamais respawné) → mp+0xac30=8 → Connexion perdue.
    patch_code(process, image, 0x46E22E, &[0x7D, 0x79], &[0xEB, 0x79])?;
    // Watchdog 300 s → mp+0xac30=9.
    patch_code(process, image, 0x46E2CE, &[0x7E, 0x4B], &[0xEB, 0x4B])?;
    // L'inbound UWP ne remplit jamais comms+0x514. On injecte un 0x02
    // dans le chemin recv pour peupler la liste via FUN_0074dbf0.
    install_discover_inject(process, image, lobby)?;
    // Le login fédération expiré empile l'écran « délai de la demande ».
    // On retire cet état et on revient au menu, sans le chemin SUCCESS.
    patch_code(
        process,
        image,
        0x51B57,
        &[0xCC; 24],
        &[
            0xE8, 0xB4, 0x50, 0x1B, 0x00, 0x05, 0x14, 0x03, 0x00, 0x00, 0x89, 0xC1, 0xE8, 0x38,
            0x99, 0x1C, 0x00, 0xB0, 0x01, 0xE9, 0xD3, 0xAD, 0x42, 0x00,
        ],
    )?;
    patch_code(
        process,
        image,
        0x47B3A8,
        &[0xFF, 0x77, 0x2C, 0x8D, 0x85],
        &[0xE9, 0xAA, 0x67, 0xBD, 0xFF],
    )?;
    // Frame de GS_LocalClientSearchingServers, VA 0087263b.
    // Tant que le widget +0xd vaut 0, chaque image fait clignoter la recherche
    // et, après 0x1770 ms, rappelle le widget. Le saut qui suit pousse déjà
    // LOCAL_JOIN_SELECTION. On prend toujours ce saut : la liste reste,
    // les 15 envois du constructeur ne sont pas réécrits.
    patch_code(process, image, 0x47263B, &[0x75, 0x21], &[0xEB, 0x21])?;
    Ok(())
}

/// Première IPv4 unicast non-loopback / non-link-local (ordre réseau a.b.c.d).
#[cfg(windows)]
fn lan_ipv4_octets() -> Result<[u8; 4], String> {
    const AF_INET: u32 = 2;
    const GAA_FLAG_SKIP_ANYCAST: u32 = 0x0002;
    const GAA_FLAG_SKIP_MULTICAST: u32 = 0x0004;
    const GAA_FLAG_SKIP_DNS_SERVER: u32 = 0x0008;
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;

    unsafe {
        let mut size = 0u32;
        let _ = GetAdaptersAddresses(AF_INET, flags, std::ptr::null_mut(), std::ptr::null_mut(), &mut size);
        if size == 0 {
            return Err("aucune adresse adaptateur".into());
        }
        let mut buf = vec![0u8; size as usize];
        let status = GetAdaptersAddresses(
            AF_INET,
            flags,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut _,
            &mut size,
        );
        if status != 0 {
            return Err(format!("GetAdaptersAddresses code {status}"));
        }

        let mut adapter = buf.as_ptr() as *const IpAdapterAddresses;
        while !adapter.is_null() {
            let mut unicast = (*adapter).first_unicast;
            while !unicast.is_null() {
                let sa = (*unicast).lp_sockaddr;
                if !sa.is_null() && (*sa).sa_family == AF_INET as u16 {
                    // sockaddr_in: family(2) + port(2) + addr(4)
                    let octets = [
                        (*sa).sa_data[2],
                        (*sa).sa_data[3],
                        (*sa).sa_data[4],
                        (*sa).sa_data[5],
                    ];
                    if is_usable_lan_ipv4(octets) {
                        return Ok(octets);
                    }
                }
                unicast = (*unicast).next;
            }
            adapter = (*adapter).next;
        }
    }
    Err("aucune IPv4 LAN utilisable".into())
}

#[cfg(windows)]
fn is_usable_lan_ipv4(o: [u8; 4]) -> bool {
    if o == [0, 0, 0, 0] || o[0] == 127 || o[0] == 169 && o[1] == 254 {
        return false;
    }
    // Exclure APIPA déjà fait ; garder RFC1918 + adresses privées usuelles.
    true
}

#[cfg(windows)]
const GAME_ID_BE: u32 = 0x0AB2_5571;

#[cfg(windows)]
fn pad_name20(name: &str) -> [u8; 0x14] {
    let mut buf = [0u8; 0x14];
    let b = name.as_bytes();
    let n = b.len().min(0x13);
    buf[..n].copy_from_slice(&b[..n]);
    buf
}

#[cfg(windows)]
fn wrap_inject_payload(payload: &[u8]) -> Vec<u8> {
    wrap_inject_payload_ack(payload, 0, 0)
}

/// Header 13 octets : flag=0, peerA=0, peerB/mask = ACK piggyback (fiable sortant).
#[cfg(windows)]
fn wrap_inject_payload_ack(payload: &[u8], peer_b: u32, seq_mask: u32) -> Vec<u8> {
    let mut packet = vec![0u8; 13];
    packet[5..9].copy_from_slice(&peer_b.to_le_bytes());
    packet[9..13].copy_from_slice(&seq_mask.to_le_bytes());
    packet.extend_from_slice(payload);
    packet
}

#[cfg(windows)]
fn wrap_inject_var(type_id: u8, body: &[u8]) -> Vec<u8> {
    let total_len = (1 + 2 + body.len()) as u16;
    let mut payload = Vec::with_capacity(3 + body.len());
    payload.push(type_id);
    payload.extend_from_slice(&total_len.to_be_bytes());
    payload.extend_from_slice(body);
    wrap_inject_payload(&payload)
}

/// Paquet 0x02 ServerGameDetails pour la liste UI.
/// Hôte = device[0x14] ; name = [gameMode, playerCount, maxPlayers, mapId].
/// Joueurs lit name[1]/name[2] (sinon mapId=0 → affichage « 1/0 »).
#[cfg(windows)]
fn build_inject_server_details(
    host_display: &str,
    game_mode: u8,
    map_id: u8,
    player_count: u8,
    max_players: u8,
) -> Vec<u8> {
    let max_players = if max_players < 2 { 8 } else { max_players };
    let name = [game_mode, player_count, max_players, map_id];
    let device_buf = pad_name20(host_display);
    let mut body = Vec::with_capacity(4 + 1 + name.len() + 0x14);
    body.extend_from_slice(&GAME_ID_BE.to_be_bytes());
    body.push(name.len() as u8);
    body.extend_from_slice(&name);
    body.extend_from_slice(&device_buf);
    wrap_inject_var(0x02, &body)
}

/// 0x05 LobbyDetails — descriptor [mode,map,max,flags] + 1 joueur.
#[cfg(windows)]
fn build_inject_lobby_details(lobby: &LobbyConfig, player_name: &str) -> Vec<u8> {
    let max_players = if lobby.max_players < 2 {
        8
    } else {
        lobby.max_players
    };
    let desc = [
        lobby.game_mode,
        lobby.map_id,
        max_players,
        0u8,
    ];
    let appearance = [0u8, 0, 0];
    let mut body = Vec::with_capacity(64);
    body.extend_from_slice(&1u32.to_be_bytes()); // seq
    body.push(desc.len() as u8);
    body.extend_from_slice(&desc);
    body.push(1); // N players
    body.extend_from_slice(&pad_name20(player_name));
    body.push(appearance.len() as u8);
    body.extend_from_slice(&appearance);
    body.push(0); // deviceId
    wrap_inject_var(0x05, &body)
}

/// 0x16 NewPlayer (fixe 30) — désactivé en phase A (AV avec bypass filtre).
#[cfg(windows)]
#[allow(dead_code)]
fn build_inject_new_player(player_name: &str) -> Vec<u8> {
    let mut payload = Vec::with_capacity(30);
    payload.extend_from_slice(&[0x16, 0, 1, 0, 0, 0]); // id, used, device, team, class
    payload.extend_from_slice(&pad_name20(player_name));
    payload.extend_from_slice(&0u32.to_be_bytes()); // token
    wrap_inject_payload(&payload)
}

/// 0x10 StartLoading (fixe 9).
#[cfg(windows)]
fn build_inject_start_loading() -> Vec<u8> {
    wrap_inject_payload(&[0x10, 0, 0, 0, 0, 0, 0, 0, 0])
}

/// ACK fiable pour seq 0 (0x03) + seq 1 (0x11) : peerB=1, mask bits 0+1.
#[cfg(windows)]
const JOIN_ACK_PEER_B: u32 = 1;
#[cfg(windows)]
const JOIN_ACK_MASK: u32 = 0b11;

/// 0x06 ReqClock (fixe 2) — phase B après 0x11.
#[cfg(windows)]
fn build_inject_req_clock(peer_b: u32, seq_mask: u32) -> Vec<u8> {
    wrap_inject_payload_ack(&[0x06, 0], peer_b, seq_mask)
}

/// 0x08 TimeSync (fixe 9) — phase B après 0x11.
#[cfg(windows)]
fn build_inject_time_sync(delay_val: u32, peer_b: u32, seq_mask: u32) -> Vec<u8> {
    let mut payload = vec![0x08];
    payload.extend_from_slice(&100u32.to_be_bytes()); // rtt
    payload.extend_from_slice(&delay_val.to_be_bytes());
    wrap_inject_payload_ack(&payload, peer_b, seq_mask)
}

/// 0x12 StartGame (fixe 13) — phase B après 0x11.
#[cfg(windows)]
fn build_inject_start_game(peer_b: u32, seq_mask: u32) -> Vec<u8> {
    let mut payload = vec![0x12];
    payload.extend_from_slice(&2000u32.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    payload.extend_from_slice(&0u32.to_be_bytes());
    wrap_inject_payload_ack(&payload, peer_b, seq_mask)
}

/// 0x0F AckOnly — ACK dans le header.
#[cfg(windows)]
fn build_inject_ack_only(peer_b: u32, seq_mask: u32) -> Vec<u8> {
    wrap_inject_payload_ack(&[0x0f], peer_b, seq_mask)
}

/// 0x0D CommsPing (fixe 9).
#[cfg(windows)]
fn build_inject_comms_ping(ping_seq: u32, peer_b: u32, seq_mask: u32) -> Vec<u8> {
    let mut payload = vec![0x0d];
    payload.extend_from_slice(&ping_seq.to_be_bytes());
    payload.extend_from_slice(&50u32.to_be_bytes());
    wrap_inject_payload_ack(&payload, peer_b, seq_mask)
}

/// 0x0E PlayerPings (VAR) — 1 entrée slot0.
#[cfg(windows)]
fn build_inject_player_pings(peer_b: u32, seq_mask: u32) -> Vec<u8> {
    let body = [1u8, 0, 50];
    let total_len = (1 + 2 + body.len()) as u16;
    let mut payload = Vec::with_capacity(3 + body.len());
    payload.push(0x0e);
    payload.extend_from_slice(&total_len.to_be_bytes());
    payload.extend_from_slice(&body);
    wrap_inject_payload_ack(&payload, peer_b, seq_mask)
}

/// Lot phase B1 : ReqClock + ACK×2 (file à 3 slots).
#[cfg(windows)]
fn build_phase_b_clock_batch() -> [Vec<u8>; 3] {
    let ack = build_inject_ack_only(JOIN_ACK_PEER_B, JOIN_ACK_MASK);
    [
        build_inject_req_clock(JOIN_ACK_PEER_B, JOIN_ACK_MASK),
        ack.clone(),
        ack,
    ]
}

/// Lot phase B2 : TimeSync + StartGame + ACK.
#[cfg(windows)]
fn build_phase_b_start_batch(delay_val: u32) -> [Vec<u8>; 3] {
    [
        build_inject_time_sync(delay_val, JOIN_ACK_PEER_B, JOIN_ACK_MASK),
        build_inject_start_game(JOIN_ACK_PEER_B, JOIN_ACK_MASK),
        build_inject_ack_only(JOIN_ACK_PEER_B, JOIN_ACK_MASK),
    ]
}

/// Lot keepalive : ping + pings joueurs + ACK (mask dynamique).
#[cfg(windows)]
fn build_phase_b_keepalive_batch(ping_seq: u32, peer_b: u32, seq_mask: u32) -> [Vec<u8>; 3] {
    [
        build_inject_comms_ping(ping_seq, peer_b, seq_mask),
        build_inject_player_pings(peer_b, seq_mask),
        build_inject_ack_only(peer_b, seq_mask),
    ]
}

/// Concatène plusieurs messages wire dans un seul datagramme injecté.
#[cfg(windows)]
fn bundle_inject_msgs(msgs: &[Vec<u8>], peer_b: u32, seq_mask: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    for m in msgs {
        payload.extend_from_slice(m);
    }
    wrap_inject_payload_ack(&payload, peer_b, seq_mask)
}

#[cfg(windows)]
fn msg_var(type_id: u8, body: &[u8]) -> Vec<u8> {
    let total_len = (1 + 2 + body.len()) as u16;
    let mut v = Vec::with_capacity(3 + body.len());
    v.push(type_id);
    v.extend_from_slice(&total_len.to_be_bytes());
    v.extend_from_slice(body);
    v
}

/// Snapshot 0x15 — remplit la table joueurs (AddPlayerDetails) + lève le filtre.
/// packed = (classe<<4) | équipe ; équipe ≠ 0 obligatoire pour le slot local.
/// N=0 sur le slot local : les ids loadout du snapshot ne s'appliquent qu'aux non-locaux.
#[cfg(windows)]
fn msg_snapshot(name: &str, self_id: u8, packed: u8, render_delay: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(359);
    body.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // u16 a, u16 b, u8 c, u8 d
    body.extend_from_slice(&render_delay.to_be_bytes());
    body.push(1); // état ≠ 4 → [mp+0xa460]=0 (autorise 0x25)
    for slot in 0..12u8 {
        if slot == self_id {
            body.extend_from_slice(&[1, 0, self_id, packed, 0]); // used, device, id, packed, N
            body.extend_from_slice(&pad_name20(name));
        } else {
            body.extend_from_slice(&[0, 0xFF, 0xFF, 0, 0]);
            body.extend_from_slice(&[0u8; 20]);
        }
        body.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0]); // F, G(u16be=0xFFFF), H
    }
    debug_assert_eq!(body.len(), 359);
    msg_var(0x15, &body)
}

/// 0x13 — force l'état de partie (2 = en jeu).
#[cfg(windows)]
fn msg_game_state(state: u8) -> Vec<u8> {
    let mut v = vec![0x13];
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.extend_from_slice(&0u32.to_be_bytes());
    v.push(state);
    v
}

/// 0x6d — MatchStart → SetGameState(2).
#[cfg(windows)]
fn msg_match_start() -> Vec<u8> {
    vec![0x6d, 0, 0, 0, 0]
}

/// Réponse SyncRequest : snapshot 0x15 (remplit la table joueurs) + état 2 + 0x6d.
/// Le patch caméra 0x309F3E évite ConnLost 7 au respawn déclenché par 0x15.
#[cfg(windows)]
fn build_inject_sync_snapshot(peer_b: u32, seq_mask: u32, packed: u8) -> Vec<u8> {
    bundle_inject_msgs(
        &[
            msg_snapshot("Player", 0, packed, 150),
            msg_game_state(2),
            msg_match_start(),
        ],
        peer_b,
        seq_mask,
    )
}

/// 0x62 ClassConfirm (VAR) — flag=1 obligatoire pour le local.
#[cfg(windows)]
fn msg_class_confirm(player_id: u8, faction: u8, class_id: u8, loadout: &[u16]) -> Vec<u8> {
    let n = loadout.len().min(32) as u8;
    let mut body = Vec::with_capacity(5 + 2 * n as usize);
    body.extend_from_slice(&[player_id, 1, faction, class_id, n]);
    for &id in loadout.iter().take(n as usize) {
        body.extend_from_slice(&id.to_be_bytes());
    }
    msg_var(0x62, &body)
}

/// 0x25 Respawn (VAR). N=0 vide le vecteur loadout réseau — passer les ids profil.
#[cfg(windows)]
fn msg_respawn(player_id: u8, spawn_id: u32, loadout: &[u16]) -> Vec<u8> {
    let n = loadout.len().min(32) as u8;
    let mut body = Vec::with_capacity(14 + 2 * n as usize);
    body.extend_from_slice(&0u32.to_be_bytes());
    body.push(player_id);
    body.extend_from_slice(&spawn_id.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.push(n);
    for &id in loadout.iter().take(n as usize) {
        body.extend_from_slice(&id.to_be_bytes());
    }
    msg_var(0x25, &body)
}

#[cfg(windows)]
fn build_inject_class_batch(
    player_id: u8,
    faction: u8,
    class_id: u8,
    loadout: &[u16],
    peer_b: u32,
    seq_mask: u32,
) -> Vec<u8> {
    // Toujours 0x62 + 0x25 avec N>0 (N=0 efface le loadout → pas d'arme / AV).
    bundle_inject_msgs(
        &[
            msg_class_confirm(player_id, faction, class_id, loadout),
            msg_respawn(player_id, 0, loadout),
        ],
        peer_b,
        seq_mask,
    )
}

/// Starter offline (ids type arme du registre 0x656eb0) — N=0 vide le loadout et AV ~4s.
#[cfg(windows)]
const FALLBACK_LOADOUT: [u16; 3] = [
    0x275B, // tokarev
    0x2793, // M38
    0x27F5, // shovel
];

/// Profil local (singleton image+0x9A4580) :
/// 1) vecteur u16 équipés (+0x20)
/// 2) sinon jusqu'à 4 ids possédés (table +0x10) — bon espace d'ids item
/// 3) sinon starter hardcodé
#[cfg(windows)]
fn read_profile_loadout(process: *mut std::ffi::c_void, image: u32) -> Vec<u16> {
    let mut mgr_ptr = [0u8; 4];
    if read_bytes(process, image as u64 + 0x9A4580, &mut mgr_ptr).is_err() {
        return FALLBACK_LOADOUT.to_vec();
    }
    let mgr = u32::from_le_bytes(mgr_ptr);
    if mgr == 0 {
        return FALLBACK_LOADOUT.to_vec();
    }

    let mut be = [0u8; 8];
    if read_bytes(process, mgr as u64 + 0x20, &mut be).is_ok() {
        let begin = u32::from_le_bytes([be[0], be[1], be[2], be[3]]);
        let end = u32::from_le_bytes([be[4], be[5], be[6], be[7]]);
        if begin != 0 && end > begin && end - begin <= 64 {
            let nbytes = (end - begin) as usize;
            let mut raw = vec![0u8; nbytes];
            if read_bytes(process, begin as u64, &mut raw).is_ok() {
                let out: Vec<u16> = raw
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .filter(|&id| id != 0 && id != 0xFFFF)
                    .take(16)
                    .collect();
                if !out.is_empty() {
                    return out;
                }
            }
        }
    }

    // Inventaire possédé (index = item id). Limité : éviter le dump massif.
    if read_bytes(process, mgr as u64 + 0x10, &mut be).is_ok() {
        let begin = u32::from_le_bytes([be[0], be[1], be[2], be[3]]);
        let end = u32::from_le_bytes([be[4], be[5], be[6], be[7]]);
        if begin != 0 && end > begin && end - begin <= 0x4000 {
            let take = (((end - begin) / 4) as usize).min(512);
            let mut raw = vec![0u8; take * 4];
            if read_bytes(process, begin as u64, &mut raw).is_ok() {
                let mut out = Vec::new();
                for (i, chunk) in raw.chunks_exact(4).enumerate() {
                    let bits = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    if bits != 0 && i > 0 && i < 0x10000 {
                        out.push(i as u16);
                        if out.len() >= 4 {
                            break;
                        }
                    }
                }
                if !out.is_empty() {
                    return out;
                }
            }
        }
    }

    FALLBACK_LOADOUT.to_vec()
}

/// peerB = max seq vu ; bit i si (peerB-i) reçu.
#[cfg(windows)]
fn ack_fields(seen: &std::collections::BTreeSet<u32>, max: u32) -> (u32, u32) {
    let mut mask = 0u32;
    for i in 0..32u32 {
        if let Some(s) = max.checked_sub(i) {
            if seen.contains(&s) {
                mask |= 1 << i;
            }
        }
    }
    (max, mask)
}

/// Parse `flag=N` / `seq=N` d'une ligne stub ; Some(seq) si flag≠0.
#[cfg(windows)]
fn parse_reliable_seq(line: &str) -> Option<u32> {
    let num = |key: &str| -> Option<u32> {
        let i = line.find(key)? + key.len();
        let s: String = line[i..].chars().take_while(|c| c.is_ascii_digit()).collect();
        s.parse().ok()
    };
    if num("flag=")? == 0 {
        return None;
    }
    num("seq=")
}

/// Réécrit les 3 slots join et remet IDX=0 si la file est vide (IDX≥COUNT).
#[cfg(windows)]
fn inject_push_batch(
    process: *mut std::ffi::c_void,
    cave: u32,
    pkts: &[Vec<u8>; 3],
) -> Result<bool, String> {
    let mut idx = [0u8];
    read_bytes(process, cave as u64 + INJECT_IDX_OFF as u64, &mut idx)
        .map_err(|_| "lecture IDX refusée".to_string())?;
    if idx[0] < INJECT_JOIN_COUNT {
        return Ok(false);
    }
    for (i, p) in pkts.iter().enumerate() {
        if p.is_empty() || p.len() > INJECT_SLOT_SIZE as usize {
            return Err(format!("batch[{i}] invalide len={}", p.len()));
        }
        let len_off = cave + INJECT_JOIN_LENS_OFF + (i as u32) * 2;
        let pkt_off = cave + INJECT_JOIN_PKT_BASE + (i as u32) * INJECT_SLOT_SIZE;
        write_exact(process, len_off as u64, &(p.len() as u16).to_le_bytes())
            .map_err(|_| format!("écriture len batch[{i}]"))?;
        write_exact(process, pkt_off as u64, p)
            .map_err(|_| format!("écriture pkt batch[{i}]"))?;
    }
    write_exact(process, cave as u64 + INJECT_IDX_OFF as u64, &[0u8])
        .map_err(|_| "écriture IDX batch".to_string())?;
    Ok(true)
}

/// Phase A join (avant FinishedLoading 0x11) : lobby → StartLoading → ACK.
/// Pas de 0x16 : avec le bypass filtre snapshot, NewPlayer AV (tables vides).
/// Horloge / StartGame viennent plus tard (sinon trop tôt vs stub réel).
#[cfg(windows)]
fn build_join_inject_packets(lobby: &LobbyConfig) -> Result<Vec<Vec<u8>>, String> {
    let player = "Player";
    let packets = vec![
        build_inject_lobby_details(lobby, player),
        build_inject_start_loading(),
        build_inject_ack_only(JOIN_ACK_PEER_B, JOIN_ACK_MASK),
    ];
    for (i, p) in packets.iter().enumerate() {
        if p.is_empty() || p.len() > INJECT_SLOT_SIZE as usize {
            return Err(format!("paquet join[{i}] invalide len={}", p.len()));
        }
    }
    Ok(packets)
}

#[cfg(windows)]
const DISCOVER_INJECT_SITE_RVA: u32 = 0x34F031;
/// Cave layout (page 0x1000) — file discover/join, 1 datagramme par select≤0.
#[cfg(windows)]
const INJECT_ENTER_STUB_OFF: u32 = 0x280;
#[cfg(windows)]
const INJECT_JOIN_ARM_STUB_OFF: u32 = 0x2B0;
#[cfg(windows)]
const INJECT_MODE_OFF: u32 = 0x2E0; // 0=discover, 1=join
#[cfg(windows)]
const INJECT_IDX_OFF: u32 = 0x2E1;
#[cfg(windows)]
const INJECT_HITS_OFF: u32 = 0x2E2; // u8 select≤0 inject hits
#[cfg(windows)]
const INJECT_JOIN_OK_OFF: u32 = 0x2E3; // u8 e370 join ok
#[cfg(windows)]
const INJECT_DISCOVER_LEN_OFF: u32 = 0x2E4; // u16
#[cfg(windows)]
const INJECT_JOIN_NO_PEER_OFF: u32 = 0x2E6; // u8 join sans peer [comms+0x194]
#[cfg(windows)]
const INJECT_JOIN_LENS_OFF: u32 = 0x2E8; // u16[6]
#[cfg(windows)]
const INJECT_DISCOVER_PKT_OFF: u32 = 0x300;
/// Discover tient dans 0x300..0x380.
#[cfg(windows)]
const INJECT_DISCOVER_MAX: u32 = 0x80;
#[cfg(windows)]
const INJECT_JOIN_PKT_BASE: u32 = 0x380;
/// Slots join agrandis pour snapshot 0x15 (~362 B) + bundle.
#[cfg(windows)]
const INJECT_SLOT_SIZE: u32 = 0x200;
/// Phase A seulement (0x05/0x16/0x10) — pas d'horloge avant 0x11.
#[cfg(windows)]
const INJECT_JOIN_COUNT: u8 = 3;
/// Peer actif post-Connect (lu après e370 natif).
#[cfg(windows)]
const COMMS_ACTIVE_PEER_OFF: u32 = 0x194;
/// ctor GS_LocalClientSearchingServers — une fois par entrée écran.
#[cfg(windows)]
const SEARCH_ENTER_SITE_RVA: u32 = 0x472280;
#[cfg(windows)]
const SEARCH_ENTER_ORIG: [u8; 5] = [0x55, 0x8B, 0xEC, 0x6A, 0xFF];
/// `call ClientSendDetails` dans Connect — armement sans toucher au prologue SEH.
#[cfg(windows)]
const JOIN_ARM_CALL_RVA: u32 = 0x34E1E9;
#[cfg(windows)]
const CLIENT_SEND_DETAILS_RVA: u32 = 0x34DF70;

#[cfg(windows)]
fn patch_rel32(buf: &mut [u8], at: usize, base: u32, target: u32) {
    let next = base.wrapping_add(at as u32).wrapping_add(4);
    let rel = target.wrapping_sub(next).to_le_bytes();
    buf[at..at + 4].copy_from_slice(&rel);
}

#[cfg(windows)]
fn install_site_jmp_hook(
    process: *mut std::ffi::c_void,
    site: u32,
    stub_abs: u32,
    expected: &[u8; 5],
    label: &str,
) -> Result<(), String> {
    let mut orig = [0u8; 5];
    if read_bytes(process, site as u64, &mut orig).is_err() {
        return Err(format!("lecture {label} refusée (code {})", last_error()));
    }
    if orig[0] == 0xE9 {
        let rel = i32::from_le_bytes([orig[1], orig[2], orig[3], orig[4]]);
        let target = (site as i64 + 5 + rel as i64) as u32;
        if target == stub_abs {
            return Ok(());
        }
    } else if orig != *expected {
        return Err(format!(
            "octets inattendus {label}: {}",
            hex_bytes(&orig)
        ));
    }
    let rel = stub_abs.wrapping_sub(site.wrapping_add(5)).to_le_bytes();
    let hook = [0xE9, rel[0], rel[1], rel[2], rel[3]];
    let page_base = (site as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection {label} refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, site as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| format!("écriture hook {label} refusée (code {})", last_error()))?;
    Ok(())
}

/// Réarme la file discover (MODE=0, IDX=0) à l'entrée recherche.
#[cfg(windows)]
fn install_search_enter_flag_hook(
    process: *mut std::ffi::c_void,
    image: u32,
    cave: u32,
) -> Result<(), String> {
    let stub_abs = cave + INJECT_ENTER_STUB_OFF;
    let site = image + SEARCH_ENTER_SITE_RVA;
    let mode_abs = cave + INJECT_MODE_OFF;
    let idx_abs = cave + INJECT_IDX_OFF;

    let mut stub = Vec::with_capacity(24);
    // mov byte [mode], 0
    stub.extend_from_slice(&[0xC6, 0x05]);
    stub.extend_from_slice(&mode_abs.to_le_bytes());
    stub.push(0x00);
    // mov byte [idx], 0
    stub.extend_from_slice(&[0xC6, 0x05]);
    stub.extend_from_slice(&idx_abs.to_le_bytes());
    stub.push(0x00);
    stub.extend_from_slice(&SEARCH_ENTER_ORIG);
    let jmp_at = stub.len();
    stub.extend_from_slice(&[0xE9, 0, 0, 0, 0]);
    patch_rel32(
        &mut stub,
        jmp_at + 1,
        stub_abs,
        site.wrapping_add(5),
    );

    write_exact(process, stub_abs as u64, &stub).map_err(|_| {
        format!(
            "écriture stub entrée recherche refusée (code {})",
            last_error()
        )
    })?;
    install_site_jmp_hook(process, site, stub_abs, &SEARCH_ENTER_ORIG, "ctor recherche")
}

/// Arme la file join (MODE=1, IDX=0) juste avant l'envoi 0x03 (call site Connect).
/// Pas de détournement de retour : le drain synchrone crashait au Rejoindre.
/// Les paquets phase A partent via le stub select (MODE=1) tant que select≤0 tourne.
#[cfg(windows)]
fn install_join_arm_hook(
    process: *mut std::ffi::c_void,
    image: u32,
    cave: u32,
) -> Result<(), String> {
    let stub_abs = cave + INJECT_JOIN_ARM_STUB_OFF;
    let site = image + JOIN_ARM_CALL_RVA;
    let target = image + CLIENT_SEND_DETAILS_RVA;
    let mode_abs = cave + INJECT_MODE_OFF;
    let idx_abs = cave + INJECT_IDX_OFF;
    let join_ok_abs = cave + INJECT_JOIN_OK_OFF;
    let join_no_peer_abs = cave + INJECT_JOIN_NO_PEER_OFF;

    // Attendu : E8 rel32 → ClientSendDetails
    let expect_rel = target.wrapping_sub(site.wrapping_add(5)).to_le_bytes();
    let expected = [0xE8, expect_rel[0], expect_rel[1], expect_rel[2], expect_rel[3]];

    // call→stub : MODE=1, IDX=0, compteurs join à 0, jmp ClientSendDetails.
    let mut stub = Vec::with_capacity(40);
    stub.extend_from_slice(&[0xC6, 0x05]);
    stub.extend_from_slice(&mode_abs.to_le_bytes());
    stub.push(0x01);
    stub.extend_from_slice(&[0xC6, 0x05]);
    stub.extend_from_slice(&idx_abs.to_le_bytes());
    stub.push(0x00);
    stub.extend_from_slice(&[0xC6, 0x05]);
    stub.extend_from_slice(&join_ok_abs.to_le_bytes());
    stub.push(0x00);
    stub.extend_from_slice(&[0xC6, 0x05]);
    stub.extend_from_slice(&join_no_peer_abs.to_le_bytes());
    stub.push(0x00);
    let jmp_at = stub.len();
    stub.extend_from_slice(&[0xE9, 0, 0, 0, 0]);
    patch_rel32(&mut stub, jmp_at + 1, stub_abs, target);
    if stub.len() > (INJECT_MODE_OFF - INJECT_JOIN_ARM_STUB_OFF) as usize {
        return Err(format!(
            "stub arm join trop long ({} > {})",
            stub.len(),
            INJECT_MODE_OFF - INJECT_JOIN_ARM_STUB_OFF
        ));
    }

    write_exact(process, stub_abs as u64, &stub).map_err(|_| {
        format!(
            "écriture stub armement join refusée (code {})",
            last_error()
        )
    })?;

    // Si un ancien hook JMP sur le prologue ClientSendDetails traîne, le restaurer.
    let prologue = image + CLIENT_SEND_DETAILS_RVA;
    let mut prol = [0u8; 5];
    if read_bytes(process, prologue as u64, &mut prol).is_ok() && prol[0] == 0xE9 {
        let restore = [0x55u8, 0x8B, 0xEC, 0x6A, 0xFF];
        let page_base = (prologue as u64) & !0xFFF;
        let mut old = 0u32;
        if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } != 0 {
            let _ = write_exact(process, prologue as u64, &restore);
            unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
        }
    }

    let mut orig = [0u8; 5];
    if read_bytes(process, site as u64, &mut orig).is_err() {
        return Err(format!(
            "lecture call ClientSendDetails refusée (code {})",
            last_error()
        ));
    }
    if orig[0] == 0xE8 {
        let rel = i32::from_le_bytes([orig[1], orig[2], orig[3], orig[4]]);
        let tgt = (site as i64 + 5 + rel as i64) as u32;
        if tgt == stub_abs {
            return Ok(());
        }
        if orig != expected && tgt != target {
            return Err(format!(
                "call ClientSendDetails inattendu: {}",
                hex_bytes(&orig)
            ));
        }
    } else {
        return Err(format!(
            "call ClientSendDetails inattendu: {}",
            hex_bytes(&orig)
        ));
    }

    let rel = stub_abs.wrapping_sub(site.wrapping_add(5)).to_le_bytes();
    let hook = [0xE8, rel[0], rel[1], rel[2], rel[3]];
    let page_base = (site as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection call ClientSendDetails refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, site as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| {
        format!(
            "écriture hook call ClientSendDetails refusée (code {})",
            last_error()
        )
    })?;
    Ok(())
}

#[cfg(windows)]
fn write_inject_packet_blobs(
    process: *mut std::ffi::c_void,
    cave: u32,
    lobby: &LobbyConfig,
    arm_discover: bool,
) -> Result<(), String> {
    let discover = build_inject_server_details(
        &lobby.host_name,
        lobby.game_mode,
        lobby.map_id,
        1,
        lobby.max_players,
    );
    if discover.is_empty() || discover.len() > INJECT_DISCOVER_MAX as usize {
        return Err("paquet discover inject trop grand".into());
    }
    let join_pkts = build_join_inject_packets(lobby)?;

    write_exact(
        process,
        cave as u64 + INJECT_DISCOVER_LEN_OFF as u64,
        &(discover.len() as u16).to_le_bytes(),
    )
    .map_err(|_| format!("écriture len discover refusée (code {})", last_error()))?;
    write_exact(
        process,
        cave as u64 + INJECT_DISCOVER_PKT_OFF as u64,
        &discover,
    )
    .map_err(|_| format!("écriture pkt discover refusée (code {})", last_error()))?;

    for (i, pkt) in join_pkts.iter().enumerate() {
        let len_off = cave + INJECT_JOIN_LENS_OFF + (i as u32) * 2;
        let pkt_off = cave + INJECT_JOIN_PKT_BASE + (i as u32) * INJECT_SLOT_SIZE;
        write_exact(process, len_off as u64, &(pkt.len() as u16).to_le_bytes())
            .map_err(|_| format!("écriture len join[{i}] refusée (code {})", last_error()))?;
        write_exact(process, pkt_off as u64, pkt)
            .map_err(|_| format!("écriture pkt join[{i}] refusée (code {})", last_error()))?;
    }

    if arm_discover {
        // Ne pas écraser un join en cours (MODE=1) — sync Créer / refresh.
        let mut cur_mode = [0u8];
        let _ = read_bytes(process, cave as u64 + INJECT_MODE_OFF as u64, &mut cur_mode);
        if cur_mode[0] != 1 {
            write_exact(process, cave as u64 + INJECT_MODE_OFF as u64, &[0u8])
                .map_err(|_| format!("écriture mode refusée (code {})", last_error()))?;
            write_exact(process, cave as u64 + INJECT_IDX_OFF as u64, &[0u8])
                .map_err(|_| format!("écriture idx refusée (code {})", last_error()))?;
            write_exact(process, cave as u64 + INJECT_HITS_OFF as u64, &[0u8])
                .map_err(|_| format!("écriture hits refusée (code {})", last_error()))?;
            write_exact(process, cave as u64 + INJECT_JOIN_OK_OFF as u64, &[0u8])
                .map_err(|_| format!("écriture join_ok refusée (code {})", last_error()))?;
            write_exact(process, cave as u64 + INJECT_JOIN_NO_PEER_OFF as u64, &[0u8])
                .map_err(|_| format!("écriture no_peer refusée (code {})", last_error()))?;
        }
    }
    Ok(())
}

/// Met à jour les paquets discover/join si le hook est en place.
/// Retourne true si sync effectuée, false si pas encore hooké.
#[cfg(windows)]
fn refresh_discover_inject_packet(
    process: *mut std::ffi::c_void,
    image: u32,
    lobby: &LobbyConfig,
) -> Result<bool, String> {
    let mut orig = [0u8; 6];
    if read_bytes(
        process,
        image as u64 + DISCOVER_INJECT_SITE_RVA as u64,
        &mut orig,
    )
    .is_err()
    {
        return Ok(false);
    }
    if orig[0] != 0xE9 {
        return Ok(false);
    }

    let rel = i32::from_le_bytes([orig[1], orig[2], orig[3], orig[4]]);
    let cave = (image as i64 + DISCOVER_INJECT_SITE_RVA as i64 + 5 + rel as i64) as u32;
    // Réécrit le stub (dispatch join→e370) + blobs, même si déjà hooké.
    let lan = lan_ipv4_octets()?;
    let code = build_select_inject_stub(cave, image, lan)?;
    write_exact(process, cave as u64, &code).map_err(|_| {
        format!("réécriture stub inject refusée (code {})", last_error())
    })?;
    write_inject_packet_blobs(process, cave, lobby, true)?;
    install_search_enter_flag_hook(process, image, cave)?;
    install_join_arm_hook(process, image, cave)?;
    Ok(true)
}

/// Machine code de la cave post-select (discover + file join).
#[cfg(windows)]
fn build_select_inject_stub(
    page: u32,
    image: u32,
    lan: [u8; 4],
) -> Result<Vec<u8>, String> {
    const RECV_RVA: u32 = 0x34F037;
    const EXIT_RVA: u32 = 0x34F160;
    const PARSE_RVA: u32 = 0x34F083;
    const LOOKUP_RVA: u32 = 0x34E310; // peer lookup by sockaddr (fallback)
    const DISPATCH_RVA: u32 = 0x34E370; // packet dispatch

    let mode_abs = page + INJECT_MODE_OFF;
    let idx_abs = page + INJECT_IDX_OFF;
    let hits_abs = page + INJECT_HITS_OFF;
    let join_ok_abs = page + INJECT_JOIN_OK_OFF;
    let join_no_peer_abs = page + INJECT_JOIN_NO_PEER_OFF;
    let disc_len_abs = page + INJECT_DISCOVER_LEN_OFF;
    let join_lens_abs = page + INJECT_JOIN_LENS_OFF;
    let disc_pkt_abs = page + INJECT_DISCOVER_PKT_OFF;
    let join_base_abs = page + INJECT_JOIN_PKT_BASE;
    let ip_imm = u32::from_le_bytes(lan);

    let mut code: Vec<u8> = Vec::with_capacity(320);
    // Force inject : MODE==1 && IDX<COUNT → on saute le `jg go_recv` même si
    // select>0 (en jeu, le socket est lisible : loopback LAN / trafic natif),
    // sinon la file join/phase B n'est jamais drainée (hits/idx/join_ok figés).
    code.extend_from_slice(&[0x80, 0x3D]);
    code.extend_from_slice(&mode_abs.to_le_bytes());
    code.push(0x01); // cmp byte [MODE],1
    code.extend_from_slice(&[0x75, 0x09]); // jne short norm (+9)
    code.extend_from_slice(&[0x80, 0x3D]);
    code.extend_from_slice(&idx_abs.to_le_bytes());
    code.push(INJECT_JOIN_COUNT); // cmp byte [IDX],COUNT
    code.extend_from_slice(&[0x72, 0x08]); // jb short inject (+8, saute test+jg)
    // norm:
    code.extend_from_slice(&[0x85, 0xC0]);
    let jg_recv_at = code.len();
    code.extend_from_slice(&[0x0F, 0x8F, 0, 0, 0, 0]);

    // inject: compteur hits (select≤0 OU inject forcé)
    code.extend_from_slice(&[0xFE, 0x05]);
    code.extend_from_slice(&hits_abs.to_le_bytes());

    code.extend_from_slice(&[0x53, 0x8B, 0xDE]);
    code.extend_from_slice(&[0x0F, 0xB6, 0x15]);
    code.extend_from_slice(&idx_abs.to_le_bytes());
    code.extend_from_slice(&[0x80, 0x3D]);
    code.extend_from_slice(&mode_abs.to_le_bytes());
    code.push(0x00);
    let jne_join_at = code.len();
    code.extend_from_slice(&[0x0F, 0x85, 0, 0, 0, 0]); // jne near join_path

    code.extend_from_slice(&[0x83, 0xFA, 0x01]);
    let jae_exit_disc_at = code.len();
    code.extend_from_slice(&[0x0F, 0x83, 0, 0, 0, 0]); // jae near restore_exit
    code.extend_from_slice(&[0x0F, 0xB7, 0x0D]);
    code.extend_from_slice(&disc_len_abs.to_le_bytes());
    code.push(0xBE);
    code.extend_from_slice(&disc_pkt_abs.to_le_bytes());
    let jmp_do_copy_at = code.len();
    code.extend_from_slice(&[0xE9, 0, 0, 0, 0]); // jmp near do_copy

    let join_path = code.len();
    code.extend_from_slice(&[0x83, 0xFA, INJECT_JOIN_COUNT]);
    let jae_exit_join_at = code.len();
    code.extend_from_slice(&[0x0F, 0x83, 0, 0, 0, 0]); // jae near restore_exit
    code.extend_from_slice(&[0x0F, 0xB7, 0x0C, 0x55]);
    code.extend_from_slice(&join_lens_abs.to_le_bytes());
    code.extend_from_slice(&[0x69, 0xC2]);
    code.extend_from_slice(&INJECT_SLOT_SIZE.to_le_bytes());
    code.push(0xBE);
    code.extend_from_slice(&join_base_abs.to_le_bytes());
    code.extend_from_slice(&[0x03, 0xF0]);

    let do_copy = code.len();
    code.extend_from_slice(&[0x8D, 0xBB]);
    code.extend_from_slice(&0x514u32.to_le_bytes());
    code.extend_from_slice(&[0x8B, 0xC1]);
    code.extend_from_slice(&[0xF3, 0xA4]);
    code.extend_from_slice(&[0xFE, 0x05]);
    code.extend_from_slice(&idx_abs.to_le_bytes());
    code.extend_from_slice(&[0x8B, 0xF3, 0x8B, 0xF8]);
    code.extend_from_slice(&[0x66, 0xC7, 0x45, 0xE0, 0x02, 0x00]);
    code.extend_from_slice(&[0x66, 0xC7, 0x45, 0xE2, 0x1E, 0xD3]);
    code.extend_from_slice(&[0xC7, 0x45, 0xE4]);
    code.extend_from_slice(&ip_imm.to_le_bytes());
    // Queue sockaddr (16 octets) : zéroiser [ebp-0x18]/[ebp-0x14] pour e310.
    code.extend_from_slice(&[0xC7, 0x45, 0xE8, 0, 0, 0, 0]);
    code.extend_from_slice(&[0xC7, 0x45, 0xEC, 0, 0, 0, 0]);
    code.extend_from_slice(&[0xC7, 0x45, 0xF8, 0x10, 0x00, 0x00, 0x00]);
    code.push(0x5B);

    code.extend_from_slice(&[0x80, 0x3D]);
    code.extend_from_slice(&mode_abs.to_le_bytes());
    code.push(0x00);
    let je_disc_parse_at = code.len();
    code.extend_from_slice(&[0x0F, 0x84, 0, 0, 0, 0]); // je near discover_parse

    // Join : [comms+0x194] = handler (garde ≠0) ; peer e370 = INDEX e310 (0..0x1F).
    // Passer le pointeur 0x194 comme index → AV à 0x34E473 (ecx*4+0x1d4).
    // Si e310 échoue (−1 / ≥32), repli index 0 (entrée discover unique).
    code.extend_from_slice(&[0x83, 0xBE]);
    code.extend_from_slice(&COMMS_ACTIVE_PEER_OFF.to_le_bytes());
    code.push(0x00); // cmp dword [esi+0x194],0
    let je_no_handler_at = code.len();
    code.extend_from_slice(&[0x74, 0x00]); // je short retry_later
    code.extend_from_slice(&[0x8D, 0x45, 0xE0]); // lea eax,[ebp-0x20]
    code.push(0x50); // push sockaddr
    code.extend_from_slice(&[0x8B, 0xCE]); // mov ecx,esi
    let call_e310_at = code.len();
    code.extend_from_slice(&[0xE8, 0, 0, 0, 0]);
    code.extend_from_slice(&[0x83, 0xF8, 0x20]); // cmp eax,0x20
    let jb_have_idx_at = code.len();
    code.extend_from_slice(&[0x72, 0x02]); // jb short +2 (skip xor)
    code.extend_from_slice(&[0x31, 0xC0]); // xor eax,eax — fallback index 0
    code.push(0x50); // push peer index
    code.push(0x57); // push len
    code.extend_from_slice(&[0x8D, 0x86]);
    code.extend_from_slice(&0x514u32.to_le_bytes());
    code.push(0x50); // push buf
    code.extend_from_slice(&[0x8B, 0xCE]); // mov ecx,esi
    let call_e370_at = code.len();
    code.extend_from_slice(&[0xE8, 0, 0, 0, 0]);
    code.extend_from_slice(&[0xFE, 0x05]); // inc join_ok
    code.extend_from_slice(&join_ok_abs.to_le_bytes());
    let jmp_exit_after_join_at = code.len();
    code.extend_from_slice(&[0xE9, 0, 0, 0, 0]);

    // Handler absent : clamp-dec IDX + compteur, réessai au prochain select.
    let retry_later = code.len();
    code.extend_from_slice(&[0xFE, 0x05]); // inc join_no_peer
    code.extend_from_slice(&join_no_peer_abs.to_le_bytes());
    code.extend_from_slice(&[0x80, 0x3D]);
    code.extend_from_slice(&idx_abs.to_le_bytes());
    code.push(0x00);
    let je_idx_zero_at = code.len();
    code.extend_from_slice(&[0x74, 0x00]); // je skip_dec (IDX déjà 0)
    code.extend_from_slice(&[0xFE, 0x0D]); // dec byte [idx]
    code.extend_from_slice(&idx_abs.to_le_bytes());
    let skip_dec = code.len();
    let jmp_exit_retry_at = code.len();
    code.extend_from_slice(&[0xE9, 0, 0, 0, 0]);

    let discover_parse = code.len();
    let jmp_parse_at = code.len();
    code.extend_from_slice(&[0xE9, 0, 0, 0, 0]);

    let restore_exit = code.len();
    code.extend_from_slice(&[0x8B, 0xF3]);
    code.push(0x5B);
    let jmp_exit_at = code.len();
    code.extend_from_slice(&[0xE9, 0, 0, 0, 0]);

    let go_recv = code.len();
    let jmp_recv_at = code.len();
    code.extend_from_slice(&[0xE9, 0, 0, 0, 0]);

    let short_ok = |from: usize, to: usize| -> Result<u8, String> {
        let d = to as isize - (from as isize + 2);
        if !(-128..128).contains(&d) {
            return Err(format!("short jump trop long {from}->{to}"));
        }
        Ok(d as u8)
    };
    // jb_have_idx_at déjà codé en +2 (skip xor) — vérifier la portée.
    let _ = jb_have_idx_at;
    code[je_no_handler_at + 1] = short_ok(je_no_handler_at, retry_later)?;
    code[je_idx_zero_at + 1] = short_ok(je_idx_zero_at, skip_dec)?;

    patch_rel32(&mut code, jg_recv_at + 2, page, page + go_recv as u32);
    patch_rel32(&mut code, jne_join_at + 2, page, page + join_path as u32);
    patch_rel32(&mut code, jae_exit_disc_at + 2, page, page + restore_exit as u32);
    patch_rel32(&mut code, jmp_do_copy_at + 1, page, page + do_copy as u32);
    patch_rel32(&mut code, jae_exit_join_at + 2, page, page + restore_exit as u32);
    patch_rel32(&mut code, je_disc_parse_at + 2, page, page + discover_parse as u32);
    let patch_e8 = |buf: &mut [u8], at: usize, target_rva: u32| {
        let next = page.wrapping_add(at as u32).wrapping_add(5);
        let rel = (image + target_rva).wrapping_sub(next).to_le_bytes();
        buf[at + 1..at + 5].copy_from_slice(&rel);
    };
    patch_e8(&mut code, call_e310_at, LOOKUP_RVA);
    patch_e8(&mut code, call_e370_at, DISPATCH_RVA);
    patch_rel32(&mut code, jmp_exit_after_join_at + 1, page, image + EXIT_RVA);
    patch_rel32(&mut code, jmp_exit_retry_at + 1, page, image + EXIT_RVA);
    patch_rel32(&mut code, jmp_parse_at + 1, page, image + PARSE_RVA);
    patch_rel32(&mut code, jmp_exit_at + 1, page, image + EXIT_RVA);
    patch_rel32(&mut code, jmp_recv_at + 1, page, image + RECV_RVA);

    if code.len() > INJECT_ENTER_STUB_OFF as usize {
        return Err(format!(
            "cave inject trop longue ({} > {})",
            code.len(),
            INJECT_ENTER_STUB_OFF
        ));
    }
    Ok(code)
}

/// Si le jeu tourne déjà avec le hook discover : maj paquet + flag=0.
#[cfg(windows)]
fn sync_lobby_inject_live(lobby: &LobbyConfig) -> Result<String, String> {
    let Some(pid) = find_game_pid() else {
        return Ok("jeu non lancé".into());
    };
    let process = unsafe { OpenProcess(0x0438, 0, pid) };
    if process.is_null() {
        return Ok(format!("ouverture pid={pid} refusée"));
    }
    let process = ProcessHandle(process);
    let mut peb32: u64 = 0;
    let mut returned = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            process.0,
            26,
            &mut peb32 as *mut u64 as *mut _,
            8,
            &mut returned,
        )
    };
    if status != 0 || peb32 == 0 {
        return Ok(format!("PEB pid={pid} indisponible"));
    }
    let mut image = 0u32;
    if read_exact(process.0, peb32 + 8, &mut image).is_err() || image == 0 {
        return Ok(format!("image pid={pid} indisponible"));
    }
    match refresh_discover_inject_packet(process.0, image, lobby)? {
        true => Ok(format!(
            "inject sync pid={pid} lobby={} mode={} map={}",
            lobby.host_name, lobby.game_mode, lobby.map_id
        )),
        false => Ok(format!("hook inject absent pid={pid}")),
    }
}

#[cfg(not(windows))]
fn sync_lobby_inject_live(_lobby: &LobbyConfig) -> Result<String, String> {
    Ok("sync inject Windows uniquement".into())
}

/// Sans datagramme UWP, select (FUN_004e0430) rend ≤0 et sort avant recv.
/// File : MODE0=discover 0x02 ; MODE1=join 0x05→0x16→0x10→0x06→0x08→0x12.
#[cfg(windows)]
/// Rel32 depuis l'octet suivant une instruction E8/E9.
#[cfg(windows)]
fn enc_rel32(insn_addr: u32, target: u32) -> [u8; 4] {
    target.wrapping_sub(insn_addr.wrapping_add(5)).to_le_bytes()
}

/// Base d'un module 32-bit dans le process distant (TH32CS_SNAPMODULE32).
#[cfg(windows)]
#[allow(dead_code)]
fn remote_module_base(pid: u32, module_name: &str) -> Result<u32, String> {
    #[repr(C)]
    struct ModuleEntry32W {
        size: u32,
        module_id: u32,
        process_id: u32,
        glbl_cnt: u32,
        proc_cnt: u32,
        mod_base_addr: usize,
        mod_base_size: u32,
        h_module: usize,
        module_name: [u16; 256],
        exe_path: [u16; 260],
    }
    const TH32CS_SNAPMODULE: u32 = 0x8;
    const TH32CS_SNAPMODULE32: u32 = 0x10;
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
        if snap.is_null() || snap == (-1isize as *mut std::ffi::c_void) {
            return Err(format!(
                "snapshot modules pid={pid} refusé (code {})",
                last_error()
            ));
        }
        let snap = ProcessHandle(snap);
        let mut entry: ModuleEntry32W = std::mem::zeroed();
        entry.size = std::mem::size_of::<ModuleEntry32W>() as u32;
        if Module32FirstW(snap.0, &mut entry as *mut _ as *mut _) == 0 {
            return Err(format!(
                "Module32FirstW pid={pid} échoué (code {})",
                last_error()
            ));
        }
        let want = module_name.to_ascii_lowercase();
        loop {
            let end = entry
                .module_name
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.module_name.len());
            let name = String::from_utf16_lossy(&entry.module_name[..end]).to_ascii_lowercase();
            if name == want {
                return Ok(entry.mod_base_addr as u32);
            }
            if Module32NextW(snap.0, &mut entry as *mut _ as *mut _) == 0 {
                break;
            }
        }
    }
    Err(format!("module {module_name} introuvable dans pid={pid}"))
}

/// Résout un export PE32 dans la mémoire du process distant.
#[cfg(windows)]
#[allow(dead_code)]
fn remote_proc_address(
    process: *mut std::ffi::c_void,
    module_base: u32,
    export_name: &str,
) -> Result<u32, String> {
    let mut dos = [0u8; 0x40];
    read_bytes(process, module_base as u64, &mut dos).map_err(|_| {
        format!("lecture DOS header {module_base:08X} refusée (code {})", last_error())
    })?;
    if dos[0] != b'M' || dos[1] != b'Z' {
        return Err(format!("DOS header invalide @ {module_base:08X}"));
    }
    let e_lfanew = u32::from_le_bytes([dos[0x3C], dos[0x3D], dos[0x3E], dos[0x3F]]);
    let mut nt = [0u8; 0xF8];
    read_bytes(process, module_base as u64 + e_lfanew as u64, &mut nt).map_err(|_| {
        format!("lecture NT headers refusée (code {})", last_error())
    })?;
    if nt[0] != b'P' || nt[1] != b'E' {
        return Err("signature PE invalide".into());
    }
    let magic = u16::from_le_bytes([nt[0x18], nt[0x19]]);
    if magic != 0x10B {
        return Err(format!("PE non-32bit (magic {magic:04X})"));
    }
    // DataDirectory[0] Export : OptionalHeader+96 = NT+24+96 = NT+0x78
    let exp_rva = u32::from_le_bytes([nt[0x78], nt[0x79], nt[0x7A], nt[0x7B]]);
    let exp_size = u32::from_le_bytes([nt[0x7C], nt[0x7D], nt[0x7E], nt[0x7F]]);
    if exp_rva == 0 || exp_size < 40 {
        return Err(format!("pas d'exports pour {export_name}"));
    }
    let mut exp = [0u8; 40];
    read_bytes(process, module_base as u64 + exp_rva as u64, &mut exp).map_err(|_| {
        format!("lecture export dir refusée (code {})", last_error())
    })?;
    let n_names = u32::from_le_bytes([exp[24], exp[25], exp[26], exp[27]]);
    let addr_of_functions = u32::from_le_bytes([exp[28], exp[29], exp[30], exp[31]]);
    let addr_of_names = u32::from_le_bytes([exp[32], exp[33], exp[34], exp[35]]);
    let addr_of_ords = u32::from_le_bytes([exp[36], exp[37], exp[38], exp[39]]);
    if n_names == 0 || n_names > 0x10000 {
        return Err("table d'exports suspecte".into());
    }
    let mut name_rvas = vec![0u8; (n_names as usize) * 4];
    read_bytes(
        process,
        module_base as u64 + addr_of_names as u64,
        &mut name_rvas,
    )
    .map_err(|_| format!("lecture AddressOfNames refusée (code {})", last_error()))?;
    let want = export_name.as_bytes();
    for i in 0..n_names as usize {
        let name_rva = u32::from_le_bytes([
            name_rvas[i * 4],
            name_rvas[i * 4 + 1],
            name_rvas[i * 4 + 2],
            name_rvas[i * 4 + 3],
        ]);
        let mut raw = vec![0u8; want.len() + 1];
        if read_bytes(process, module_base as u64 + name_rva as u64, &mut raw).is_err() {
            continue;
        }
        if &raw[..want.len()] == want && raw[want.len()] == 0 {
            let mut ord_buf = [0u8; 2];
            read_bytes(
                process,
                module_base as u64 + addr_of_ords as u64 + (i as u64) * 2,
                &mut ord_buf,
            )
            .map_err(|_| format!("lecture ordinal refusée (code {})", last_error()))?;
            let ordinal = u16::from_le_bytes(ord_buf) as u32;
            let mut fn_rva_buf = [0u8; 4];
            read_bytes(
                process,
                module_base as u64 + addr_of_functions as u64 + (ordinal as u64) * 4,
                &mut fn_rva_buf,
            )
            .map_err(|_| format!("lecture AddressOfFunctions refusée (code {})", last_error()))?;
            let fn_rva = u32::from_le_bytes(fn_rva_buf);
            // Forwarded export si RVA dans la dir export — non géré.
            if fn_rva >= exp_rva && fn_rva < exp_rva + exp_size {
                return Err(format!("export forwardé non supporté: {export_name}"));
            }
            return Ok(module_base.wrapping_add(fn_rva));
        }
    }
    Err(format!("export {export_name} introuvable"))
}

/// pid + image base (ASLR).
#[cfg(windows)]
static FPS_LOOK_STATE: Mutex<Option<(u32, u32)>> = Mutex::new(None);

/// Deltas souris relatifs (Raw Input) — GetCursorPos est mort si le jeu capture le curseur.
#[cfg(windows)]
static FPS_RAW_DX: AtomicI32 = AtomicI32::new(0);
#[cfg(windows)]
static FPS_RAW_DY: AtomicI32 = AtomicI32::new(0);

#[cfg(windows)]
fn read_u32_at(process: *mut std::ffi::c_void, addr: u64) -> Option<u32> {
    let mut b = [0u8; 4];
    if read_bytes(process, addr, &mut b).is_err() {
        return None;
    }
    Some(u32::from_le_bytes(b))
}

/// Joueur local : index packer `mp+0x2EE8` → table `mp+0x30F0`.
#[cfg(windows)]
fn resolve_local_player(process: *mut std::ffi::c_void, image: u32) -> Option<u32> {
    let mp = read_u32_at(process, image as u64 + 0x9A42DC)?;
    if mp < 0x10000 {
        return None;
    }
    let table = read_u32_at(process, mp as u64 + 0x30F0)?;
    if table < 0x10000 {
        return None;
    }
    let mut idx_buf = [0u8; 4];
    if read_bytes(process, mp as u64 + 0x2EE8, &mut idx_buf).is_err() {
        return None;
    }
    let idx = i32::from_le_bytes(idx_buf);
    if idx < 0 || idx > 12 {
        return None;
    }
    let player = read_u32_at(process, table as u64 + (idx as u64) * 4)?;
    if player < 0x10000 {
        return None;
    }
    Some(player)
}

/// Match : joueur présent.
#[cfg(windows)]
fn match_player_ready(process: *mut std::ffi::c_void, image: u32) -> bool {
    resolve_local_player(process, image).is_some()
}

fn wrap_deg(mut d: f32) -> f32 {
    while d > 180.0 {
        d -= 360.0;
    }
    while d < -180.0 {
        d += 360.0;
    }
    d
}

/// deg → byte réseau 0x17 (même formule que le packer ×256/360).
fn deg_to_pitch_byte(deg: f32) -> u8 {
    let mut d = deg % 360.0;
    if d < 0.0 {
        d += 360.0;
    }
    ((d * 256.0 / 360.0).round() as i32).rem_euclid(256) as u8
}

/// Packer FUN_00693760 : `obj = *player`, pitch=`[obj+0x58]`, yaw=`[obj+0x60]`.
#[cfg(windows)]
#[allow(dead_code)]
struct PackerAngles {
    player: u32,
    obj: u32,
    /// f32 brut à player+0x58 (mauvais niveau — diagnostic)
    player_pitch_raw: f32,
    player_yaw_raw: f32,
    /// f32 packer à *player+0x58 / +0x60
    obj_pitch_raw: f32,
    obj_yaw_raw: f32,
    obj_pitch_wrap: f32,
    est_pitch_byte: u8,
    est_yaw_byte: u8,
}

#[cfg(windows)]
fn resolve_packer_angles(process: *mut std::ffi::c_void, image: u32) -> Option<PackerAngles> {
    let player = resolve_local_player(process, image)?;
    let obj = read_u32_at(process, player as u64)?;
    if obj < 0x10000 || (obj & 3) != 0 {
        return None;
    }
    let player_pitch_raw = read_f32_at(process, player as u64 + 0x58)?;
    let player_yaw_raw = read_f32_at(process, player as u64 + 0x60).unwrap_or(f32::NAN);
    let obj_pitch_raw = read_f32_at(process, obj as u64 + 0x58)?;
    let obj_yaw_raw = read_f32_at(process, obj as u64 + 0x60)?;
    if !obj_pitch_raw.is_finite() || !obj_yaw_raw.is_finite() {
        return None;
    }
    let obj_pitch_wrap = wrap_deg(obj_pitch_raw);
    Some(PackerAngles {
        player,
        obj,
        player_pitch_raw,
        player_yaw_raw,
        obj_pitch_raw,
        obj_yaw_raw,
        obj_pitch_wrap,
        est_pitch_byte: deg_to_pitch_byte(obj_pitch_raw),
        est_yaw_byte: deg_to_pitch_byte(obj_yaw_raw),
    })
}

/// Force horizon sur le **vrai** champ packer `*player+0x58` (+ miroir player+0x58).
#[cfg(windows)]
#[allow(dead_code)]
fn force_packer_pitch_horizon(
    process: *mut std::ffi::c_void,
    image: u32,
) -> Option<(PackerAngles, bool, PackerAngles)> {
    let before = resolve_packer_angles(process, image)?;
    // Toujours écrire 0 sur obj+0x58 (hold horizon) — le jeu peut réécrire entre ticks.
    let z = 0f32.to_le_bytes();
    let ok_obj = write_exact(process, before.obj as u64 + 0x58, &z).is_ok();
    let ok_pl = write_exact(process, before.player as u64 + 0x58, &z).is_ok();
    let wrote = ok_obj && ok_pl;
    let after = resolve_packer_angles(process, image)?;
    Some((before, wrote, after))
}

/// Souris → `*player+0x60` yaw (pitch forcé à part). Désarmé : yaw natif.
#[cfg(windows)]
#[allow(dead_code)]
fn apply_packer_yaw_deg(process: *mut std::ffi::c_void, image: u32, dx: i32) -> bool {
    let Some(a) = resolve_packer_angles(process, image) else {
        return false;
    };
    const SENS: f32 = 0.15;
    let new_yaw = a.obj_yaw_raw + (dx as f32) * SENS;
    let y = new_yaw.to_le_bytes();
    let ok_obj = write_exact(process, a.obj as u64 + 0x60, &y).is_ok();
    let ok_pl = write_exact(process, a.player as u64 + 0x60, &y).is_ok();
    ok_obj && ok_pl
}

/// Fire-look gates — ARM une fois en match FPS, restore à la sortie.
#[cfg(windows)]
const FIRE_GATE_JLE_ORIG: [u8; 8] = [0x85, 0xC0, 0x0F, 0x8E, 0x11, 0x01, 0x00, 0x00];
#[cfg(windows)]
const FIRE_GATE_JLE_ARM: [u8; 8] = [0x85, 0xC0, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90];
#[cfg(windows)]
const FIRE_GATE_JZ1_ORIG: [u8; 4] = [0x84, 0xC0, 0x74, 0x4A];
#[cfg(windows)]
const FIRE_GATE_JZ1_ARM: [u8; 4] = [0x84, 0xC0, 0x90, 0x90];
#[cfg(windows)]
const FIRE_GATE_JZ2_ORIG: [u8; 4] = [0x84, 0xC0, 0x74, 0x2A];
#[cfg(windows)]
const FIRE_GATE_JZ2_ARM: [u8; 4] = [0x84, 0xC0, 0x90, 0x90];

#[cfg(windows)]
fn restore_fire_look_gates(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    patch_code(
        process,
        image,
        0x25280B,
        &FIRE_GATE_JLE_ARM,
        &FIRE_GATE_JLE_ORIG,
    )?;
    patch_code(
        process,
        image,
        0x252868,
        &FIRE_GATE_JZ1_ARM,
        &FIRE_GATE_JZ1_ORIG,
    )?;
    patch_code(
        process,
        image,
        0x2528BE,
        &FIRE_GATE_JZ2_ARM,
        &FIRE_GATE_JZ2_ORIG,
    )?;
    Ok(())
}

#[cfg(windows)]
#[allow(dead_code)]
fn arm_fire_look_gates(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    patch_code(
        process,
        image,
        0x25280B,
        &FIRE_GATE_JLE_ORIG,
        &FIRE_GATE_JLE_ARM,
    )?;
    patch_code(
        process,
        image,
        0x252868,
        &FIRE_GATE_JZ1_ORIG,
        &FIRE_GATE_JZ1_ARM,
    )?;
    patch_code(
        process,
        image,
        0x2528BE,
        &FIRE_GATE_JZ2_ORIG,
        &FIRE_GATE_JZ2_ARM,
    )?;
    Ok(())
}

/// Un seul fire-gate — JZ1 d'abord, JZ2 si octets JZ1 absents (évite triple-NOP).
/// Désarmé en match : JZ1 cassait le tir en rafale.
#[cfg(windows)]
#[allow(dead_code)]
fn arm_fire_look_gate_one(process: *mut std::ffi::c_void, image: u32) -> Result<&'static str, String> {
    match patch_code(
        process,
        image,
        0x252868,
        &FIRE_GATE_JZ1_ORIG,
        &FIRE_GATE_JZ1_ARM,
    ) {
        Ok(()) => Ok("JZ1"),
        Err(e1) => patch_code(
            process,
            image,
            0x2528BE,
            &FIRE_GATE_JZ2_ORIG,
            &FIRE_GATE_JZ2_ARM,
        )
        .map(|_| "JZ2")
        .map_err(|e2| format!("JZ1: {e1}; JZ2: {e2}")),
    }
}

#[cfg(windows)]
fn restore_fire_look_gate_one(process: *mut std::ffi::c_void, image: u32) {
    let _ = patch_code(
        process,
        image,
        0x252868,
        &FIRE_GATE_JZ1_ARM,
        &FIRE_GATE_JZ1_ORIG,
    );
    let _ = patch_code(
        process,
        image,
        0x2528BE,
        &FIRE_GATE_JZ2_ARM,
        &FIRE_GATE_JZ2_ORIG,
    );
}

/// TouchBegin abs no-center (je→jmp) — évite le snap centre stick quand `9A4740=1`.
#[cfg(windows)]
#[allow(dead_code)]
fn arm_abs_look_no_center(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    patch_code(process, image, 0x2E317D, &[0x74, 0x5F], &[0xEB, 0x5F])
}

/// Restaure TouchBegin abs ORIG (`74 5F`) pour laisser sticky `9A4740` faire TouchMove idle.
#[cfg(windows)]
fn restore_abs_look_no_center(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    patch_code(process, image, 0x2E317D, &[0xEB, 0x5F], &[0x74, 0x5F])
}

/// Masque le stick UI tactile.
#[cfg(windows)]
#[allow(dead_code)]
fn disable_touch_controls(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    // FullScreenRotateJoystick::IsActive → toujours faux (pas de dessin stick).
    patch_code(
        process,
        image,
        0x46D070,
        &[0x8A, 0x41, 0x1D, 0xC3],
        &[0x32, 0xC0, 0xC3, 0x90],
    )?;
    Ok(())
}

#[cfg(windows)]
#[allow(dead_code)]
fn resolve_abs_look_ptr(process: *mut std::ffi::c_void, image: u32) -> Option<u32> {
    let a = read_u32_at(process, image as u64 + 0x99EE5C)?;
    if a < 0x10000 {
        return None;
    }
    let b = read_u32_at(process, a as u64 + 0x10)?;
    if b < 0x10000 {
        return None;
    }
    let c = read_u32_at(process, b as u64 + 0x14)?;
    if c < 0x10000 {
        return None;
    }
    let hub = c.wrapping_add(0x2E4);
    let ptr = read_u32_at(process, hub as u64 + 0x18)?;
    if ptr < 0x10000 {
        return None;
    }
    Some(ptr)
}

#[cfg(windows)]
#[allow(dead_code)]
fn write_abs_look_i16(process: *mut std::ffi::c_void, ptr: u32, x: i16, y: i16) -> bool {
    write_exact(process, ptr as u64 + 6, &x.to_le_bytes()).is_ok()
        && write_exact(process, ptr as u64 + 8, &y.to_le_bytes()).is_ok()
}

#[cfg(windows)]
#[allow(dead_code)]
fn invalidate_abs_look_cache(process: *mut std::ffi::c_void, image: u32) {
    let Some(obj) = read_u32_at(process, image as u64 + 0x9A3C00) else {
        return;
    };
    if obj < 0x10000 {
        return;
    }
    let neg1 = (-1i32).to_le_bytes();
    let _ = write_exact(process, obj as u64 + 0x1C, &neg1);
    let _ = write_exact(process, obj as u64 + 0x20, &neg1);
}

/// Idle : aligne le cache last-i16 sur le centre → pas d’apply forcé.
#[cfg(windows)]
#[allow(dead_code)]
fn sync_abs_look_cache(process: *mut std::ffi::c_void, image: u32, x: i16, y: i16) {
    let Some(obj) = read_u32_at(process, image as u64 + 0x9A3C00) else {
        return;
    };
    if obj < 0x10000 {
        return;
    }
    let _ = write_exact(process, obj as u64 + 0x1C, &(x as i32).to_le_bytes());
    let _ = write_exact(process, obj as u64 + 0x20, &(y as i32).to_le_bytes());
}

/// Signature comp caméra : `*(vt+0x1C)` → `B8 52 C6 38 75 C3` (diag_cam).
#[cfg(windows)]
const CAM_COMP_FN_SIG: [u8; 6] = [0xB8, 0x52, 0xC6, 0x38, 0x75, 0xC3];

/// Resolve CamMgr FPS (`comp+0x114`) — garde menus si absent / idx invalide.
#[cfg(windows)]
fn resolve_fps_camera_mgr(process: *mut std::ffi::c_void, image: u32) -> Option<u32> {
    let holder = resolve_local_player(process, image)?;
    let ent = read_u32_at(process, holder as u64)?;
    if ent < 0x10000 {
        return None;
    }
    let cb = read_u32_at(process, ent as u64 + 0xC8)?;
    let ce = read_u32_at(process, ent as u64 + 0xCC)?;
    if cb < 0x10000 || ce <= cb {
        return None;
    }
    let n = ((ce - cb) / 4).min(64);
    let mut cam_comp = None;
    for k in 0..n {
        let c = read_u32_at(process, cb as u64 + (k as u64) * 4)?;
        if c < 0x10000 {
            continue;
        }
        let vt = read_u32_at(process, c as u64)?;
        if vt < 0x10000 {
            continue;
        }
        let fn_ptr = read_u32_at(process, vt as u64 + 0x1C)?;
        if fn_ptr < 0x10000 {
            continue;
        }
        let mut sig = [0u8; 6];
        if read_bytes(process, fn_ptr as u64, &mut sig).is_err() {
            continue;
        }
        if sig == CAM_COMP_FN_SIG {
            cam_comp = Some(c);
            break;
        }
    }
    let comp = cam_comp?;
    let mgr = read_u32_at(process, comp as u64 + 0x114)?;
    if mgr < 0x10000 {
        return None;
    }
    let mut idx_buf = [0u8; 4];
    if read_bytes(process, mgr as u64 + 0x178, &mut idx_buf).is_err() {
        return None;
    }
    let idx = i32::from_le_bytes(idx_buf);
    if idx < 0 || idx > 64 {
        return None;
    }
    Some(mgr)
}

/// Slot caméra FPS : `table[idx*0x24]` — `+0x10` = objet 6eaee0, `+0x18` = ctl.
#[cfg(windows)]
fn resolve_fps_camera_slot(process: *mut std::ffi::c_void, image: u32) -> Option<(u32, u32)> {
    let mgr = resolve_fps_camera_mgr(process, image)?;
    let mut idx_buf = [0u8; 4];
    if read_bytes(process, mgr as u64 + 0x178, &mut idx_buf).is_err() {
        return None;
    }
    let idx = i32::from_le_bytes(idx_buf);
    if idx < 0 || idx > 64 {
        return None;
    }
    let table = read_u32_at(process, mgr as u64 + 0x154)?;
    if table < 0x10000 {
        return None;
    }
    let slot = table as u64 + (idx as u64) * 0x24;
    let cam = read_u32_at(process, slot + 0x10)?;
    let ctl = read_u32_at(process, slot + 0x18)?;
    if cam < 0x10000 || ctl < 0x10000 {
        return None;
    }
    Some((cam, ctl))
}

/// Resolve contrôleur FPS (garde menus : pas de ctl = pas de freelook).
#[cfg(windows)]
fn resolve_fps_camera_ctl(process: *mut std::ffi::c_void, image: u32) -> Option<u32> {
    resolve_fps_camera_slot(process, image).map(|(_, ctl)| ctl)
}

/// Base 3×3 horizon Rx(+90°) column-major → forward (0,-1,0) dans `cam+0x84`.
#[cfg(windows)]
#[allow(dead_code)]
fn write_cam_horizon_basis(process: *mut std::ffi::c_void, cam: u32) -> bool {
    let m: [f32; 9] = [
        1.0, 0.0, 0.0, // col0
        0.0, 0.0, 1.0, // col1
        0.0, -1.0, 0.0, // col2 = forward horizon
    ];
    let mut bytes = [0u8; 36];
    for (i, v) in m.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    write_exact(process, cam as u64 + 0x84, &bytes).is_ok()
}

/// Forward CamView : pitch 0 → `(0,-1,0)` horizon ; `+Z` = sol ; pitch>0 (souris haut) → −Z.
#[cfg(windows)]
#[allow(dead_code)]
fn freelook_view_fwd(yaw: f32, pitch: f32) -> [f32; 3] {
    let cp = pitch.cos();
    let sp = pitch.sin();
    let cy = yaw.cos();
    let sy = yaw.sin();
    [sy * cp, -cy * cp, -sp]
}

#[cfg(windows)]
#[allow(dead_code)]
fn write_share_view_fwd(process: *mut std::ffi::c_void, page: u32, fwd: [f32; 3]) -> bool {
    let mut block = [0u8; 16];
    block[0..4].copy_from_slice(&1u32.to_le_bytes()); // enable
    for (i, v) in fwd.iter().enumerate() {
        block[4 + i * 4..8 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    write_exact(process, page as u64 + CAM_SHARE_ENABLE as u64, &block).is_ok()
}

#[cfg(windows)]
#[allow(dead_code)]
fn read_mgr_look(process: *mut std::ffi::c_void, mgr: u32) -> Option<(f32, f32, f32)> {
    let x = read_f32_at(process, mgr as u64 + 0x84)?;
    let y = read_f32_at(process, mgr as u64 + 0x88)?;
    let z = read_f32_at(process, mgr as u64 + 0x8C)?;
    if !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return None;
    }
    Some((x, y, z))
}

/// `mgr+0x84` look monde Y-up : (0,-1,0) = **sol**. Horizon sûr = (0,0,1).
#[cfg(windows)]
#[allow(dead_code)]
fn write_mgr_look_horizon(process: *mut std::ffi::c_void, mgr: u32) -> bool {
    write_exact(
        process,
        mgr as u64 + 0x84,
        &[
            0u8, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x00, 0x80, 0x3F,
        ],
    )
    .is_ok()
}

/// Cave @0x309E21 : copie share VIEW_FWD → `[esi+0x84]`, puis `call 70b600`.
#[cfg(windows)]
#[allow(dead_code)]
fn install_mgr_look_horizon_hook(
    process: *mut std::ffi::c_void,
    image: u32,
    page: u32,
) -> Result<(), String> {
    let abs = image + CAM_MGR_LOOK_CALL_RVA;
    let mut cur = [0u8; 5];
    if read_bytes(process, abs as u64, &mut cur).is_err() {
        return Err(format!(
            "lecture mgr-look@309E21 refusée (code {})",
            last_error()
        ));
    }
    let code_abs = page + CAM_SHARE_CODE_LOOK;
    let already_ours = if cur[0] == 0xE9 {
        let rel = i32::from_le_bytes([cur[1], cur[2], cur[3], cur[4]]);
        let target = (abs as i64 + 5 + rel as i64) as u32;
        if target == code_abs {
            true
        } else {
            return Err("site mgr-look@309E21 déjà hooké ailleurs".into());
        }
    } else if cur[0] == 0xE8 {
        let rel = i32::from_le_bytes([cur[1], cur[2], cur[3], cur[4]]);
        let call_tgt = (abs as i64 + 5 + rel as i64) as u32;
        if call_tgt != image + CAM_MGR_LOOK_FN_RVA {
            return Err(format!(
                "call cible inattendue @309E21: {:08X} (attendu {:08X})",
                call_tgt,
                image + CAM_MGR_LOOK_FN_RVA
            ));
        }
        false
    } else {
        return Err(format!(
            "octets inattendus mgr-look@309E21: {}",
            hex_bytes(&cur)
        ));
    };

    let fwd_abs = page + CAM_SHARE_VIEW_FWD;
    let mut code = Vec::with_capacity(64);
    // esi=mgr. Copier VIEW_FWD live → [esi+0x84/88/8C].
    // mov eax,[fwd]; mov [esi+0x84],eax ; +4 → +0x88 ; +8 → +0x8C
    code.push(0xA1);
    code.extend_from_slice(&fwd_abs.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x86, 0x84, 0x00, 0x00, 0x00]);
    code.push(0xA1);
    code.extend_from_slice(&(fwd_abs + 4).to_le_bytes());
    code.extend_from_slice(&[0x89, 0x86, 0x88, 0x00, 0x00, 0x00]);
    code.push(0xA1);
    code.extend_from_slice(&(fwd_abs + 8).to_le_bytes());
    code.extend_from_slice(&[0x89, 0x86, 0x8C, 0x00, 0x00, 0x00]);
    code.extend_from_slice(&[0x89, 0xF1]); // mov ecx,esi
    let call_at = code.len();
    code.push(0xE8);
    code.extend_from_slice(&0u32.to_le_bytes());
    let call_insn = code_abs + call_at as u32;
    code[call_at + 1..call_at + 5]
        .copy_from_slice(&enc_rel32(call_insn, image + CAM_MGR_LOOK_FN_RVA));
    let jmp_at = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());
    let jr = code_abs + jmp_at as u32;
    code[jmp_at + 1..jmp_at + 5]
        .copy_from_slice(&enc_rel32(jr, image + CAM_MGR_LOOK_RESUME_RVA));

    write_exact(process, code_abs as u64, &code).map_err(|_| {
        format!(
            "écriture cave mgr-look refusée (code {})",
            last_error()
        )
    })?;

    if already_ours {
        return Ok(());
    }

    let mut hook = [0xE9u8, 0, 0, 0, 0];
    hook[1..5].copy_from_slice(&enc_rel32(abs, code_abs));
    let page_base = (abs as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection mgr-look refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, abs as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| format!("écriture hook mgr-look refusée (code {})", last_error()))?;
    Ok(())
}

#[cfg(windows)]
fn restore_mgr_look_horizon_hook(process: *mut std::ffi::c_void, image: u32) {
    // Restaure call 70b600 si encore un jmp.
    let abs = image + CAM_MGR_LOOK_CALL_RVA;
    let mut cur = [0u8; 5];
    if read_bytes(process, abs as u64, &mut cur).is_err() || cur[0] != 0xE9 {
        return;
    }
    let rel = enc_rel32(abs, image + CAM_MGR_LOOK_FN_RVA);
    let mut orig = [0xE8u8, 0, 0, 0, 0];
    orig[1..5].copy_from_slice(&rel);
    let page_base = (abs as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } != 0 {
        let _ = write_exact(process, abs as u64, &orig);
        unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    }
}

/// Cave post-`6eaee0` @0x30C8C5 : réécrit cam+0x84 horizon, rejoue `mov ecx,[edi+0x188]`.
#[cfg(windows)]
#[allow(dead_code)]
fn install_post_6eaee0_horizon(
    process: *mut std::ffi::c_void,
    image: u32,
    page: u32,
) -> Result<(), String> {
    let abs = image + CAM_POST_6EAEE0_RVA;
    let mut cur = [0u8; 6];
    if read_bytes(process, abs as u64, &mut cur).is_err() {
        return Err(format!(
            "lecture post-6eaee0@30C8C5 refusée (code {})",
            last_error()
        ));
    }
    let code_abs = page + CAM_SHARE_CODE_POST6;
    if cur[0] == 0xE9 {
        let rel = i32::from_le_bytes([cur[1], cur[2], cur[3], cur[4]]);
        let target = (abs as i64 + 5 + rel as i64) as u32;
        if target == code_abs {
            return Ok(());
        }
        return Err("site post-6eaee0 déjà hooké ailleurs".into());
    }
    if cur != CAM_POST_6EAEE0_ORIG {
        return Err(format!(
            "octets inattendus post-6eaee0@30C8C5: {}",
            hex_bytes(&cur)
        ));
    }

    // Rx(+90) column-major floats
    let m: [u32; 9] = [
        0x3f800000, 0, 0, // 1,0,0
        0, 0, 0x3f800000, // 0,0,1
        0, 0xbf800000, 0, // 0,-1,0
    ];

    let mut code = Vec::with_capacity(128);
    code.extend_from_slice(&[0x50, 0x52]); // push eax; push edx
    code.extend_from_slice(&[0x8B, 0x87, 0x78, 0x01, 0x00, 0x00]); // mov eax,[edi+0x178]
    code.extend_from_slice(&[0x8B, 0x97, 0x54, 0x01, 0x00, 0x00]); // mov edx,[edi+0x154]
    code.extend_from_slice(&[0x8D, 0x04, 0xC0]); // lea eax,[eax+eax*8]
    code.extend_from_slice(&[0x8B, 0x44, 0x82, 0x10]); // mov eax,[edx+eax*4+0x10]
    code.extend_from_slice(&[0x85, 0xC0]); // test eax,eax
    let jz_skip = code.len();
    code.extend_from_slice(&[0x74, 0x00]); // jz skip (patch)
    for (i, w) in m.iter().enumerate() {
        let off = 0x84u32 + (i as u32) * 4;
        code.extend_from_slice(&[0xC7, 0x80]); // mov dword [eax+disp32], imm32
        code.extend_from_slice(&off.to_le_bytes());
        code.extend_from_slice(&w.to_le_bytes());
    }
    let skip_at = code.len();
    code[jz_skip + 1] = (skip_at - (jz_skip + 2)) as u8;
    code.extend_from_slice(&[0x5A, 0x58]); // pop edx; pop eax
    code.extend_from_slice(&CAM_POST_6EAEE0_ORIG); // mov ecx,[edi+0x188]
    let jmp_at = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());
    let jr = code_abs + jmp_at as u32;
    let resume = image + CAM_POST_6EAEE0_RESUME_RVA;
    code[jmp_at + 1..jmp_at + 5].copy_from_slice(&enc_rel32(jr, resume));

    write_exact(process, code_abs as u64, &code).map_err(|_| {
        format!(
            "écriture cave post-6eaee0 refusée (code {})",
            last_error()
        )
    })?;

    let mut hook = [0x90u8; 6];
    hook[0] = 0xE9;
    hook[1..5].copy_from_slice(&enc_rel32(abs, code_abs));
    let page_base = (abs as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection post-6eaee0 refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, abs as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| {
        format!(
            "écriture hook post-6eaee0 refusée (code {})",
            last_error()
        )
    })?;
    Ok(())
}

#[cfg(windows)]
fn restore_post_6eaee0_horizon(process: *mut std::ffi::c_void, image: u32) {
    restore_cam_site_bytes(process, image + CAM_POST_6EAEE0_RVA, &CAM_POST_6EAEE0_ORIG);
}

#[cfg(windows)]
#[allow(dead_code)]
fn wrap_rad(mut r: f32) -> f32 {
    const PI: f32 = std::f32::consts::PI;
    while r > PI {
        r -= 2.0 * PI;
    }
    while r < -PI {
        r += 2.0 * PI;
    }
    r
}

/// Stomp pitch look-down : contact look null → ctl+0x88 = aim+0xC0.
#[cfg(windows)]
const PITCH_STOMP_JNE_ORIG: [u8; 6] = [0x0F, 0x85, 0xAF, 0x00, 0x00, 0x00];
#[cfg(windows)]
const PITCH_STOMP_JMP_ARM: [u8; 6] = [0xE9, 0xB0, 0x00, 0x00, 0x00, 0x90];

/// Coupe SEULEMENT le stomp look-down (jne→jmp). Pas les loads BC/C4 (crash map).
#[cfg(windows)]
fn arm_pitch_stomp_skip(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    patch_code(
        process,
        image,
        0x33B09A,
        &PITCH_STOMP_JNE_ORIG,
        &PITCH_STOMP_JMP_ARM,
    )
}

#[cfg(windows)]
fn restore_pitch_stomp_skip(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    patch_code(
        process,
        image,
        0x33B09A,
        &PITCH_STOMP_JMP_ARM,
        &PITCH_STOMP_JNE_ORIG,
    )
}

/// ApplyYaw / ApplyPitch (thiscall ctl) — freelook natif.
#[cfg(windows)]
#[allow(dead_code)]
const APPLY_YAW_RVA: u32 = 0x33BBC0;
#[cfg(windows)]
#[allow(dead_code)]
const APPLY_PITCH_RVA: u32 = 0x33BEE0;
/// CamUpdate après `mov ecx,ctl` : `push ebx; push edi; call 738370`.
#[cfg(windows)]
const CAM_APPLY_HOOK_RVA: u32 = 0x309D7C;
#[cfg(windows)]
#[allow(dead_code)]
const CAM_APPLY_RESUME_RVA: u32 = 0x309D83;
#[cfg(windows)]
const CAM_APPLY_INNER_CALL_RVA: u32 = 0x338370;
#[cfg(windows)]
const CAM_APPLY_HOOK_LEN: usize = 7;
/// Share freelook : +0 dx i32, +4 dy i32, cave @+0x40.
#[cfg(windows)]
#[allow(dead_code)]
const LOOK_SHARE_DX: u32 = 0x00;
#[cfg(windows)]
#[allow(dead_code)]
const LOOK_SHARE_DY: u32 = 0x04;
#[cfg(windows)]
#[allow(dead_code)]
const LOOK_SHARE_CODE: u32 = 0x40;
/// Scale Raw Input → unités stick Apply*.
#[cfg(windows)]
#[allow(dead_code)]
const LOOK_APPLY_SCALE: i32 = 24;
/// Bande pitch hors identity CamView (ctl≤0 → sol (0,0,1)).
#[cfg(windows)]
#[allow(dead_code)]
const PITCH_BAND_MIN: f32 = 0.08;
#[cfg(windows)]
#[allow(dead_code)]
const PITCH_BAND_MAX: f32 = 1.15;

/// Clamps aim (BC/C0 min, C4/C8 max) dans la bande safe — jamais ≤0.
#[cfg(windows)]
#[allow(dead_code)]
fn widen_aim_look_limits(process: *mut std::ffi::c_void, ctl: u32) -> bool {
    let Some(aim) = read_u32_at(process, ctl as u64 + 0x48) else {
        return false;
    };
    if aim < 0x10000 || (aim & 3) != 0 {
        return false;
    }
    let lo = PITCH_BAND_MIN.to_le_bytes();
    let hi = PITCH_BAND_MAX.to_le_bytes();
    write_exact(process, aim as u64 + 0xBC, &lo).is_ok()
        && write_exact(process, aim as u64 + 0xC0, &lo).is_ok()
        && write_exact(process, aim as u64 + 0xC4, &hi).is_ok()
        && write_exact(process, aim as u64 + 0xC8, &hi).is_ok()
}

/// Aligne aim+C0 sur ctl+0x88 → stomp résiduel = no-op. Ne touche pas C8 (max freelook).
#[cfg(windows)]
#[allow(dead_code)]
fn sync_aim_stomp_to_ctl(process: *mut std::ffi::c_void, ctl: u32) -> bool {
    let Some(p) = read_f32_at(process, ctl as u64 + 0x88) else {
        return false;
    };
    if !p.is_finite() {
        return false;
    }
    let Some(aim) = read_u32_at(process, ctl as u64 + 0x48) else {
        return false;
    };
    if aim < 0x10000 || (aim & 3) != 0 {
        return false;
    }
    write_exact(process, aim as u64 + 0xC0, &p.to_le_bytes()).is_ok()
}

/// Filet host : ramène ctl+0x88 dans [PITCH_BAND_MIN, PITCH_BAND_MAX]. Retourne true si clamp.
#[cfg(windows)]
#[allow(dead_code)]
fn clamp_ctl_pitch_band(process: *mut std::ffi::c_void, ctl: u32) -> bool {
    let Some(p) = read_f32_at(process, ctl as u64 + 0x88) else {
        return false;
    };
    if !p.is_finite() {
        return false;
    }
    let clamped = p.clamp(PITCH_BAND_MIN, PITCH_BAND_MAX);
    if (clamped - p).abs() < 1e-6 {
        return false;
    }
    write_ctl_pitch(process, ctl, clamped)
}

/// `aim+0x8C = 2` → hors garde ≤1 (identity si ctl bas) → chemin lookat.
#[cfg(windows)]
#[allow(dead_code)]
fn write_aim_lookat_gate(process: *mut std::ffi::c_void, ctl: u32) -> bool {
    let Some(aim) = read_u32_at(process, ctl as u64 + 0x48) else {
        return false;
    };
    if aim < 0x10000 || (aim & 3) != 0 {
        return false;
    }
    write_exact(process, aim as u64 + 0x8C, &2i32.to_le_bytes()).is_ok()
}

#[cfg(windows)]
#[allow(dead_code)]
fn read_aim_i32(process: *mut std::ffi::c_void, ctl: u32, off: u32) -> Option<i32> {
    let aim = read_u32_at(process, ctl as u64 + 0x48)?;
    if aim < 0x10000 || (aim & 3) != 0 {
        return None;
    }
    let mut b = [0u8; 4];
    if read_bytes(process, aim as u64 + off as u64, &mut b).is_err() {
        return None;
    }
    Some(i32::from_le_bytes(b))
}

/// Cave @0x309D7C : si share dx/dy ≠ 0, ApplyYaw/ApplyPitch(ctl), puis stolen + resume.
#[cfg(windows)]
#[allow(dead_code)]
fn install_apply_look_cave(
    process: *mut std::ffi::c_void,
    image: u32,
    page: u32,
) -> Result<(), String> {
    let abs = image + CAM_APPLY_HOOK_RVA;
    let mut cur = [0u8; CAM_APPLY_HOOK_LEN];
    if read_bytes(process, abs as u64, &mut cur).is_err() {
        return Err(format!(
            "lecture apply-hook@309D7C refusée (code {})",
            last_error()
        ));
    }
    let code_abs = page + LOOK_SHARE_CODE;
    let already_ours = if cur[0] == 0xE9 {
        let rel = i32::from_le_bytes([cur[1], cur[2], cur[3], cur[4]]);
        let target = (abs as i64 + 5 + rel as i64) as u32;
        if target == code_abs {
            true
        } else {
            return Err("site apply-hook@309D7C déjà hooké ailleurs".into());
        }
    } else if cur[0] == 0x53 && cur[1] == 0x57 && cur[2] == 0xE8 {
        let rel = i32::from_le_bytes([cur[3], cur[4], cur[5], cur[6]]);
        let call_insn = abs + 2;
        let call_tgt = (call_insn as i64 + 5 + rel as i64) as u32;
        if call_tgt != image + CAM_APPLY_INNER_CALL_RVA {
            return Err(format!(
                "call cible inattendue @309D7E: {:08X} (attendu {:08X})",
                call_tgt,
                image + CAM_APPLY_INNER_CALL_RVA
            ));
        }
        false
    } else {
        return Err(format!(
            "octets inattendus apply-hook@309D7C: {}",
            hex_bytes(&cur)
        ));
    };

    let dx_abs = page + LOOK_SHARE_DX;
    let dy_abs = page + LOOK_SHARE_DY;

    let mut code = Vec::with_capacity(112);
    // ecx=ctl. Sauver eax/edx/ebx.
    code.extend_from_slice(&[0x50, 0x52, 0x53]); // push eax; push edx; push ebx
    code.extend_from_slice(&[0x89, 0xCB]); // mov ebx, ecx (ctl)
    code.extend_from_slice(&[0x31, 0xC0]); // xor eax,eax
    code.push(0x87);
    code.push(0x05);
    code.extend_from_slice(&dx_abs.to_le_bytes()); // xchg [dx], eax
    code.extend_from_slice(&[0x31, 0xD2]); // xor edx,edx
    code.push(0x87);
    code.push(0x15);
    code.extend_from_slice(&dy_abs.to_le_bytes()); // xchg [dy], edx
    code.push(0x52); // push edx (sauve dy pendant ApplyYaw)
    // ApplyYaw si dx≠0 (natif: push 0; push delta; ret 8)
    code.extend_from_slice(&[0x85, 0xC0]); // test eax,eax
    let jz_yaw_at = code.len();
    code.extend_from_slice(&[0x74, 0x00]);
    code.extend_from_slice(&[0x6A, 0x00]); // push 0
    code.push(0x50); // push eax
    code.extend_from_slice(&[0x89, 0xD9]); // mov ecx, ebx
    let call_yaw_at = code.len();
    code.push(0xE8);
    code.extend_from_slice(&0u32.to_le_bytes());
    let jz_yaw_end = code.len();
    code[jz_yaw_at + 1] = (jz_yaw_end - (jz_yaw_at + 2)) as u8;
    code.push(0x5A); // pop edx (dy)
    // ApplyPitch si dy≠0 — même ABI que yaw (push 0; push delta; ret 8)
    code.extend_from_slice(&[0x85, 0xD2]);
    let jz_pitch_at = code.len();
    code.extend_from_slice(&[0x74, 0x00]);
    code.extend_from_slice(&[0x6A, 0x00]); // push 0
    code.push(0x52); // push edx (delta)
    code.extend_from_slice(&[0x89, 0xD9]); // mov ecx, ebx
    let call_pitch_at = code.len();
    code.push(0xE8);
    code.extend_from_slice(&0u32.to_le_bytes());
    let jz_pitch_end = code.len();
    code[jz_pitch_at + 1] = (jz_pitch_end - (jz_pitch_at + 2)) as u8;
    code.extend_from_slice(&[0x89, 0xD9]); // mov ecx, ebx
    code.extend_from_slice(&[0x5B, 0x5A, 0x58]); // pop ebx; pop edx; pop eax
    code.extend_from_slice(&[0x53, 0x57]); // stolen push ebx; push edi
    let call_inner_at = code.len();
    code.push(0xE8);
    code.extend_from_slice(&0u32.to_le_bytes());
    let jmp_at = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());

    let patch_call = |code: &mut [u8], at: usize, target: u32| {
        let insn = code_abs + at as u32;
        code[at + 1..at + 5].copy_from_slice(&enc_rel32(insn, target));
    };
    patch_call(&mut code, call_yaw_at, image + APPLY_YAW_RVA);
    patch_call(&mut code, call_pitch_at, image + APPLY_PITCH_RVA);
    patch_call(&mut code, call_inner_at, image + CAM_APPLY_INNER_CALL_RVA);
    {
        let insn = code_abs + jmp_at as u32;
        code[jmp_at + 1..jmp_at + 5]
            .copy_from_slice(&enc_rel32(insn, image + CAM_APPLY_RESUME_RVA));
    }

    write_exact(process, code_abs as u64, &code).map_err(|_| {
        format!(
            "écriture cave apply-look refusée (code {})",
            last_error()
        )
    })?;

    if already_ours {
        return Ok(());
    }

    let mut hook = [0x90u8; CAM_APPLY_HOOK_LEN];
    hook[0] = 0xE9;
    hook[1..5].copy_from_slice(&enc_rel32(abs, code_abs));
    let page_base = (abs as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection apply-hook refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, abs as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| format!("écriture apply-hook refusée (code {})", last_error()))?;
    Ok(())
}

#[cfg(windows)]
fn restore_apply_look_cave(process: *mut std::ffi::c_void, image: u32) {
    let abs = image + CAM_APPLY_HOOK_RVA;
    let mut cur = [0u8; CAM_APPLY_HOOK_LEN];
    if read_bytes(process, abs as u64, &mut cur).is_err() || cur[0] != 0xE9 {
        return;
    }
    // push ebx; push edi; call 738370
    let mut orig = [0u8; CAM_APPLY_HOOK_LEN];
    orig[0] = 0x53;
    orig[1] = 0x57;
    orig[2] = 0xE8;
    let call_insn = abs + 2;
    orig[3..7].copy_from_slice(&enc_rel32(call_insn, image + CAM_APPLY_INNER_CALL_RVA));
    let page_base = (abs as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } != 0 {
        let _ = write_exact(process, abs as u64, &orig);
        unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    }
}

/// Horizon CamView : +Z = sol, −Y = horizon.
#[cfg(windows)]
#[allow(dead_code)]
const HORIZON_VIEW_FWD: [f32; 3] = [0.0, -1.0, 0.0];

/// Identity forward CamView 734 : `[esp+0x98]=0` → Y=−1.
#[cfg(windows)]
const CAMVIEW_ID_Y_A_RVA: u32 = 0x334690;
#[cfg(windows)]
const CAMVIEW_ID_Y_A_ORIG: [u8; 11] =
    [0xC7, 0x84, 0x24, 0x98, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
#[cfg(windows)]
const CAMVIEW_ID_Y_A_ARM: [u8; 11] =
    [0xC7, 0x84, 0x24, 0x98, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xBF]; // −1.0
/// Identity Z=1.0 → 0 (734).
#[cfg(windows)]
const CAMVIEW_ID_Z_A_RVA: u32 = 0x33469B;
#[cfg(windows)]
const CAMVIEW_ID_Z_A_ORIG: [u8; 11] =
    [0xC7, 0x84, 0x24, 0x9C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3F];
#[cfg(windows)]
const CAMVIEW_ID_Z_A_ARM: [u8; 11] =
    [0xC7, 0x84, 0x24, 0x9C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

/// Identity Y (736 FPS) `[esp+0xB0]`.
#[cfg(windows)]
const CAMVIEW_ID_Y_B_RVA: u32 = 0x3361BC;
#[cfg(windows)]
const CAMVIEW_ID_Y_B_ORIG: [u8; 11] =
    [0xC7, 0x84, 0x24, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
#[cfg(windows)]
const CAMVIEW_ID_Y_B_ARM: [u8; 11] =
    [0xC7, 0x84, 0x24, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xBF];
/// Identity Z (736 FPS) `[esp+0xB4]`.
#[cfg(windows)]
const CAMVIEW_ID_Z_B_RVA: u32 = 0x3361C7;
#[cfg(windows)]
const CAMVIEW_ID_Z_B_ORIG: [u8; 11] =
    [0xC7, 0x84, 0x24, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3F];
#[cfg(windows)]
const CAMVIEW_ID_Z_B_ARM: [u8; 11] =
    [0xC7, 0x84, 0x24, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

/// LocalState `settings.dat+0x4E` = gyro (jamais WindowsApps).
fn clear_settings_gyro_flag() -> Result<&'static str, String> {
    let local = std::env::var("LOCALAPPDATA").map_err(|_| "LOCALAPPDATA manquant".to_string())?;
    let packages = PathBuf::from(local).join("Packages");
    let rd = fs::read_dir(&packages).map_err(|e| format!("Packages: {e}"))?;
    let mut settings = None;
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("GAMELOFTSA.BlitzBrigade_") {
            continue;
        }
        let p = e.path().join("LocalState").join("settings.dat");
        if p.is_file() {
            settings = Some(p);
            break;
        }
    }
    let path = settings.ok_or_else(|| "settings.dat LocalState introuvable".to_string())?;
    let mut data = fs::read(&path).map_err(|e| format!("lecture settings.dat: {e}"))?;
    if data.len() < 0x4F {
        return Err(format!("settings.dat trop court ({})", data.len()));
    }
    if data[0x4E] == 0 {
        return Ok("déjà 0");
    }
    data[0x4E] = 0;
    fs::write(&path, &data).map_err(|e| format!("écriture settings.dat: {e}"))?;
    Ok("cleared")
}

#[cfg(windows)]
#[allow(dead_code)]
fn arm_identity_horizon(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    patch_code(
        process,
        image,
        CAMVIEW_ID_Y_A_RVA,
        &CAMVIEW_ID_Y_A_ORIG,
        &CAMVIEW_ID_Y_A_ARM,
    )?;
    patch_code(
        process,
        image,
        CAMVIEW_ID_Z_A_RVA,
        &CAMVIEW_ID_Z_A_ORIG,
        &CAMVIEW_ID_Z_A_ARM,
    )?;
    patch_code(
        process,
        image,
        CAMVIEW_ID_Y_B_RVA,
        &CAMVIEW_ID_Y_B_ORIG,
        &CAMVIEW_ID_Y_B_ARM,
    )?;
    patch_code(
        process,
        image,
        CAMVIEW_ID_Z_B_RVA,
        &CAMVIEW_ID_Z_B_ORIG,
        &CAMVIEW_ID_Z_B_ARM,
    )?;
    Ok(())
}

#[cfg(windows)]
fn restore_identity_horizon(process: *mut std::ffi::c_void, image: u32) {
    let _ = patch_code(
        process,
        image,
        CAMVIEW_ID_Y_A_RVA,
        &CAMVIEW_ID_Y_A_ARM,
        &CAMVIEW_ID_Y_A_ORIG,
    );
    let _ = patch_code(
        process,
        image,
        CAMVIEW_ID_Z_A_RVA,
        &CAMVIEW_ID_Z_A_ARM,
        &CAMVIEW_ID_Z_A_ORIG,
    );
    let _ = patch_code(
        process,
        image,
        CAMVIEW_ID_Y_B_RVA,
        &CAMVIEW_ID_Y_B_ARM,
        &CAMVIEW_ID_Y_B_ORIG,
    );
    let _ = patch_code(
        process,
        image,
        CAMVIEW_ID_Z_B_RVA,
        &CAMVIEW_ID_Z_B_ARM,
        &CAMVIEW_ID_Z_B_ORIG,
    );
}

/// WndProc Raw Input → atomics relatifs (souris bouge même si le jeu capture le curseur).
#[cfg(windows)]
unsafe extern "system" fn fps_raw_wnd_proc(
    hwnd: *mut std::ffi::c_void,
    msg: u32,
    wp: usize,
    lp: isize,
) -> isize {
    const WM_INPUT: u32 = 0x00FF;
    const RID_INPUT: u32 = 0x10000003;
    const RIM_TYPEMOUSE: u32 = 0;
    if msg == WM_INPUT {
        let mut size = 0u32;
        GetRawInputData(
            lp as *mut std::ffi::c_void,
            RID_INPUT,
            std::ptr::null_mut(),
            &mut size,
            std::mem::size_of::<RawInputHeader>() as u32,
        );
        if size > 0 && size < 256 {
            let mut buf = vec![0u8; size as usize];
            let got = GetRawInputData(
                lp as *mut std::ffi::c_void,
                RID_INPUT,
                buf.as_mut_ptr() as *mut _,
                &mut size,
                std::mem::size_of::<RawInputHeader>() as u32,
            );
            if got != u32::MAX && got as usize >= std::mem::size_of::<RawInputHeader>() {
                let header = &*(buf.as_ptr() as *const RawInputHeader);
                if header.ty == RIM_TYPEMOUSE {
                    let mouse_off = std::mem::size_of::<RawInputHeader>();
                    if buf.len() >= mouse_off + std::mem::size_of::<RawMouse>() {
                        let mouse = &*(buf.as_ptr().add(mouse_off) as *const RawMouse);
                        // Touch/pen = MOUSE_MOVE_ABSOLUTE : ignore (sinon stick→coins→pitch 199).
                        const MOUSE_MOVE_ABSOLUTE: u16 = 0x01;
                        if mouse.flags & MOUSE_MOVE_ABSOLUTE != 0 {
                            return 0;
                        }
                        if mouse.last_x != 0 {
                            FPS_RAW_DX.fetch_add(mouse.last_x, Ordering::Relaxed);
                        }
                        if mouse.last_y != 0 {
                            FPS_RAW_DY.fetch_add(mouse.last_y, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        return 0;
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

/// Fenêtre message-only + RIDEV_INPUTSINK (reçoit les deltas même focus jeu).
#[cfg(windows)]
fn setup_fps_raw_input() -> Option<*mut std::ffi::c_void> {
    const HWND_MESSAGE: isize = -3;
    const RIDEV_INPUTSINK: u32 = 0x00000100;
    let class_name: Vec<u16> = "BBEmuFpsRaw\0".encode_utf16().collect();
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = RawWndClass {
        style: 0,
        wnd_proc: Some(fps_raw_wnd_proc),
        cls_extra: 0,
        wnd_extra: 0,
        instance,
        icon: std::ptr::null_mut(),
        cursor: std::ptr::null_mut(),
        background: std::ptr::null_mut(),
        menu_name: std::ptr::null(),
        class_name: class_name.as_ptr(),
    };
    unsafe {
        let atom = RegisterClassW(&wc);
        if atom == 0 && GetLastError() != 1410 {
            // 1410 = already registered
            return None;
        }
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE as *mut std::ffi::c_void,
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return None;
        }
        let dev = RawInputDevice {
            usage_page: 0x01,
            usage: 0x02,
            flags: RIDEV_INPUTSINK,
            hwnd_target: hwnd,
        };
        if RegisterRawInputDevices(&dev, 1, std::mem::size_of::<RawInputDevice>() as u32) == 0 {
            return None;
        }
        Some(hwnd)
    }
}

#[cfg(windows)]
fn pump_fps_raw_input() {
    let mut msg = RawMsg {
        hwnd: std::ptr::null_mut(),
        message: 0,
        wparam: 0,
        lparam: 0,
        time: 0,
        pt_x: 0,
        pt_y: 0,
        private: 0,
    };
    unsafe {
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, 1) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Sites anciens à restaurer au heal (hooks morts).
#[cfg(windows)]
const CAM_VIEW_SITE_RVA: u32 = 0x309E90;
#[cfg(windows)]
const CAM_VIEW_CALL_ORIG: [u8; 5] = [0xE8, 0x8B, 0x24, 0x00, 0x00];
#[cfg(windows)]
const CAM_LEGACY_SITE_RVA: u32 = 0x309F1A;
#[cfg(windows)]
const CAM_LEGACY_CALL_ORIG: [u8; 5] = [0xE8, 0x51, 0x17, 0x03, 0x00];
#[cfg(windows)]
const CAM_EPILOGUE_SITE_RVA: u32 = 0x309F2A;
#[cfg(windows)]
const CAM_EPILOGUE_CALL_ORIG: [u8; 5] = [0xE8, 0x21, 0x00, 0x00, 0x00];
#[cfg(windows)]
const CAM_NATIVE_PUBLISH_RVA: u32 = 0x30C8C0;
#[cfg(windows)]
const CAM_NATIVE_PUBLISH_ORIG: [u8; 5] = [0xE8, 0x1B, 0xE6, 0xFD, 0xFF];
#[cfg(windows)]
const CAM_NATIVE_PUBLISH_SKIP: [u8; 5] = [0x83, 0xC4, 0x04, 0x90, 0x90];
#[cfg(windows)]
const CAM_BAKER_SITE_RVA: u32 = 0x30A1EB;
#[cfg(windows)]
const CAM_BAKER_CALL_ORIG: [u8; 5] = [0xE8, 0x20, 0x04, 0x00, 0x00];
/// `call` apply pitch stomp (aim+0xC0 → ctl) — `ret 4`.
#[cfg(windows)]
const CAM_STOMP_CALL_RVA: u32 = 0x33B136;
#[cfg(windows)]
const CAM_STOMP_CALL_ORIG: [u8; 5] = [0xE8, 0x85, 0xEE, 0xFF, 0xFF];
#[cfg(windows)]
const CAM_STOMP_CALL_SKIP: [u8; 5] = [0x83, 0xC4, 0x04, 0x90, 0x90];

/// Après `call 6eaee0` : `mov ecx,[edi+0x188]` — ancien sink (cam+0x84 ≠ rendu).
#[cfg(windows)]
const CAM_POST_6EAEE0_RVA: u32 = 0x30C8C5;
#[cfg(windows)]
const CAM_POST_6EAEE0_ORIG: [u8; 6] = [0x8B, 0x8F, 0x88, 0x01, 0x00, 0x00];
#[cfg(windows)]
const CAM_POST_6EAEE0_RESUME_RVA: u32 = 0x30C8CB;
#[cfg(windows)]
#[allow(dead_code)]
const CAM_SHARE_CODE_POST6: u32 = 0x180;

/// Vrai look vec3 : `mgr+0x84` copié puis `call 70b600` @0x309E21.
#[cfg(windows)]
const CAM_MGR_LOOK_CALL_RVA: u32 = 0x309E21;
#[cfg(windows)]
const CAM_MGR_LOOK_FN_RVA: u32 = 0x30B600;
#[cfg(windows)]
const CAM_MGR_LOOK_RESUME_RVA: u32 = 0x309E26;
#[cfg(windows)]
const CAM_SHARE_CODE_LOOK: u32 = 0x40;

/// Share : +0 enable, +4 fwd[3], +0x10 yaw[3], +0x20 quat[4], caves +0x40/+0x100/+0x1C0/+0x280.
#[cfg(windows)]
const CAM_SHARE_ENABLE: u32 = 0x00;
#[cfg(windows)]
const CAM_SHARE_VIEW_FWD: u32 = 0x04;
#[cfg(windows)]
const CAM_SHARE_VIEW_QUAT: u32 = 0x20;
#[cfg(windows)]
const CAM_SHARE_CODE_A: u32 = 0x40;
#[cfg(windows)]
const CAM_SHARE_CODE_B: u32 = 0x100;
#[cfg(windows)]
const CAM_SHARE_CODE_FINAL: u32 = 0x1C0;
#[cfg(windows)]
const CAM_SHARE_CODE_PUBLISH: u32 = 0x280;
/// Avant `call [eax+0xAC]` : inject quat dans `[esi]=mgr+0x108` (frustum rendu).
#[cfg(windows)]
const CAM_PUBLISH_VCALL_RVA: u32 = 0x30C8CE;
#[cfg(windows)]
const CAM_PUBLISH_VCALL_ORIG: [u8; 6] = [0xFF, 0x90, 0xAC, 0x00, 0x00, 0x00];
#[cfg(windows)]
const CAM_PUBLISH_VCALL_RESUME_RVA: u32 = 0x30C8D4;
#[cfg(windows)]
#[allow(dead_code)]
const CAM_LOOK_SENS: f32 = 0.0008;
#[cfg(windows)]
#[allow(dead_code)]
const CAM_PITCH_LIMIT: f32 = 1.20;
/// ctl anti-identity seulement (pas le freelook) — freelook = mgr+0x84.
#[cfg(windows)]
#[allow(dead_code)]
const MGR_CTL_ANTI_ID: f32 = 0.25;

/// Hold ctl/aim : >0 évite identity sol (0,0,1). 0.20 = horizon stable (logs historiques).
#[cfg(windows)]
#[allow(dead_code)]
const CAM_CTL_PITCH_HOLD: f32 = 0.20;

/// Ancien hook lookat-pile (désarmé) — restauré si encore présent.
#[cfg(windows)]
const CAMVIEW_FINAL_HOOK_RVA: u32 = 0x334A71;
#[cfg(windows)]
const CAMVIEW_FINAL_ORIG: [u8; 5] = [0xE8, 0x7A, 0x05, 0xF6, 0xFF];
/// Après `call 0x7712d0` : ancien stomp edi+0x68 (insuffisant — direction reprise sur pile).
#[cfg(windows)]
const CAMVIEW_POST7712_HOOK_RVA: u32 = 0x334B2A;
#[cfg(windows)]
const CAMVIEW_POST7712_ORIG: [u8; 5] = [0x8B, 0x4B, 0x58, 0x85, 0xC9];
#[cfg(windows)]
const CAMVIEW_POST7712_RESUME_RVA: u32 = 0x334B2F;
/// Vrai sink direction : remix lookat−eye → `[esp+0xA0/A4/A8]` @ 0x334B76, puis 69f2f0.
#[cfg(windows)]
const CAMVIEW_STACK_DIR_HOOK_RVA: u32 = 0x334B76;
#[cfg(windows)]
const CAMVIEW_STACK_DIR_ORIG: [u8; 9] =
    [0xF3, 0x0F, 0x10, 0x84, 0x24, 0x8C, 0x00, 0x00, 0x00]; // movss xmm0,[esp+0x8c]
#[cfg(windows)]
#[allow(dead_code)]
const CAMVIEW_STACK_DIR_RESUME_RVA: u32 = 0x334B7F;
/// Sauter le normalize natif : fwd déjà unitaire dans a0/a4/a8.
#[cfg(windows)]
#[allow(dead_code)]
const CAMVIEW_STACK_DIR_SKIP_RVA: u32 = 0x334C48;
/// Force le chemin jusqu'à 7712d0 (évite je → 334CE3 si eax==0).
#[cfg(windows)]
const CAMVIEW_FORCE7712_RVA: u32 = 0x334A50;
#[cfg(windows)]
const CAMVIEW_FORCE7712_ORIG: [u8; 6] = [0x0F, 0x84, 0x8D, 0x02, 0x00, 0x00];
#[cfg(windows)]
const CAMVIEW_FORCE7712_ARM: [u8; 6] = [0x90, 0x90, 0x90, 0x90, 0x90, 0x90];
/// Skip branche `69bac0` (fwd+pos early) : jle 3349BF → jmp always.
#[cfg(windows)]
const CAMVIEW_SKIP_69_RVA: u32 = 0x334912;
#[cfg(windows)]
const CAMVIEW_SKIP_69_ORIG: [u8; 6] = [0x0F, 0x8E, 0xA7, 0x00, 0x00, 0x00];
#[cfg(windows)]
const CAMVIEW_SKIP_69_ARM: [u8; 6] = [0xE9, 0xA8, 0x00, 0x00, 0x00, 0x90];
/// Share yaw-only pour caves early (évite poison 69bac0 / builder).
#[cfg(windows)]
#[allow(dead_code)]
const CAM_SHARE_VIEW_YAW: u32 = 0x10;

/// Dernier `movss` forward + `ja short` look-at (longueur variable selon encodage PE).
#[cfg(windows)]
#[allow(dead_code)]
struct CamViewFwdSite {
    hook_rva: u32,
    orig: &'static [u8],
    post_rva: u32,
    /// Offsets esp (avant push cave) pour deux triplets forward.
    fwd_a: [u32; 3],
    fwd_b: [u32; 3],
    code_off: u32,
    label: &'static str,
    required: bool,
}

#[cfg(windows)]
const CAMVIEW_FWD_ORIG_A: &[u8] = &[0xF3, 0x0F, 0x11, 0x7C, 0x24, 0x78, 0x77, 0x10];
/// PE live : movss [esp+0x9c],xmm6 en disp32 (9 B) + ja short (2 B).
#[cfg(windows)]
const CAMVIEW_FWD_ORIG_B: &[u8] = &[
    0xF3, 0x0F, 0x11, 0xB4, 0x24, 0x9C, 0x00, 0x00, 0x00, 0x77, 0x10,
];

#[cfg(windows)]
const CAMVIEW_FWD_SITES: [CamViewFwdSite; 2] = [
    CamViewFwdSite {
        // 0x3346DC movss [esp+0x78],xmm7 ; 0x3346E2 ja short lookat
        hook_rva: 0x3346DC,
        orig: CAMVIEW_FWD_ORIG_A,
        post_rva: 0x334890,
        fwd_a: [0x70, 0x74, 0x78],
        fwd_b: [0x94, 0x98, 0x9C],
        code_off: CAM_SHARE_CODE_A,
        label: "camview-fwd@3346DC",
        required: true,
    },
    CamViewFwdSite {
        // 0x33621D movss disp32 [esp+0x9c],xmm6 ; 0x336226 ja short lookat
        hook_rva: 0x33621D,
        orig: CAMVIEW_FWD_ORIG_B,
        post_rva: 0x336405,
        fwd_a: [0x94, 0x98, 0x9C],
        fwd_b: [0xAC, 0xB0, 0xB4],
        code_off: CAM_SHARE_CODE_B,
        label: "camview-fwd@33621D",
        required: true, // clone FPS 736xxx
    },
];

#[cfg(windows)]
#[allow(dead_code)]
fn skip_call_ret4(
    process: *mut std::ffi::c_void,
    image: u32,
    rva: u32,
    orig: &[u8; 5],
    skip: &[u8; 5],
) -> Result<(), String> {
    let addr = image as u64 + rva as u64;
    let mut cur = [0u8; 5];
    if read_bytes(process, addr, &mut cur).is_err() {
        return Err(format!("lecture refusée au RVA {rva:X} (code {})", last_error()));
    }
    if cur == *skip {
        return Ok(());
    }
    let old_nop = [0x90u8, 0x90, 0x90, 0x90, 0x90];
    if cur == *orig || cur == old_nop {
        return patch_code(process, image, rva, &cur, skip);
    }
    Err(format!(
        "octets inattendus au RVA {rva:X}: {}",
        hex_bytes(&cur)
    ))
}

#[cfg(windows)]
fn restore_native_cam_publish(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    let site = image + CAM_NATIVE_PUBLISH_RVA;
    restore_cam_site_bytes(process, site, &CAM_NATIVE_PUBLISH_ORIG);
    let _ = patch_code(
        process,
        image,
        CAM_NATIVE_PUBLISH_RVA,
        &CAM_NATIVE_PUBLISH_SKIP,
        &CAM_NATIVE_PUBLISH_ORIG,
    );
    let _ = patch_code(
        process,
        image,
        CAM_NATIVE_PUBLISH_RVA,
        &[0x90, 0x90, 0x90, 0x90, 0x90],
        &CAM_NATIVE_PUBLISH_ORIG,
    );
    Ok(())
}

#[cfg(windows)]
fn read_f32_at(process: *mut std::ffi::c_void, addr: u64) -> Option<f32> {
    let mut b = [0u8; 4];
    if read_bytes(process, addr, &mut b).is_err() {
        return None;
    }
    Some(f32::from_le_bytes(b))
}

#[cfg(windows)]
fn write_ctl_pitch(process: *mut std::ffi::c_void, ctl: u32, pitch: f32) -> bool {
    write_exact(process, ctl as u64 + 0x88, &pitch.to_le_bytes()).is_ok()
}

/// `aim+0xC0` → SetPitch / stomp. HOLD = horizon (0 → sol via identity).
#[cfg(windows)]
#[allow(dead_code)]
fn write_aim_pitch_hold(process: *mut std::ffi::c_void, ctl: u32, hold: f32) -> bool {
    let Some(aim) = read_u32_at(process, ctl as u64 + 0x48) else {
        return false;
    };
    if aim < 0x10000 || (aim & 3) != 0 {
        return false;
    }
    // C0 + C8 (limites) = HOLD — évite stomp look-down et reclamp sol.
    write_exact(process, aim as u64 + 0xC0, &hold.to_le_bytes()).is_ok()
        && write_exact(process, aim as u64 + 0xC8, &hold.to_le_bytes()).is_ok()
}

#[cfg(windows)]
#[allow(dead_code)]
fn read_aim_c0(process: *mut std::ffi::c_void, ctl: u32) -> Option<f32> {
    let aim = read_u32_at(process, ctl as u64 + 0x48)?;
    if aim < 0x10000 || (aim & 3) != 0 {
        return None;
    }
    read_f32_at(process, aim as u64 + 0xC0)
}

/// Forward cam post-builder : col2 de la 3×3 @ `cam+0x84` → (x,y,z) @ +0x9C/+0xA0/+0xA4.
#[cfg(windows)]
#[allow(dead_code)]
fn read_cam_forward(process: *mut std::ffi::c_void, cam: u32) -> Option<(f32, f32, f32)> {
    let x = read_f32_at(process, cam as u64 + 0x9C)?;
    let y = read_f32_at(process, cam as u64 + 0xA0)?;
    let z = read_f32_at(process, cam as u64 + 0xA4)?;
    if !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return None;
    }
    Some((x, y, z))
}

/// `aim+0x8C ≤ 1` : avec ctl≤0, CamView garde l'identity (sinon fallback lookat).
#[cfg(windows)]
#[allow(dead_code)]
fn write_aim_identity_gate(process: *mut std::ffi::c_void, ctl: u32) -> bool {
    let Some(aim) = read_u32_at(process, ctl as u64 + 0x48) else {
        return false;
    };
    if aim < 0x10000 || (aim & 3) != 0 {
        return false;
    }
    write_exact(process, aim as u64 + 0x8C, &1i32.to_le_bytes()).is_ok()
}

/// ctl=0 + aim+0x8C=1 → branche identity (patchée horizon).
#[cfg(windows)]
#[allow(dead_code)]
fn force_identity_branch(process: *mut std::ffi::c_void, ctl: u32) -> bool {
    write_ctl_pitch(process, ctl, 0.0) && write_aim_identity_gate(process, ctl)
}

#[cfg(windows)]
#[allow(dead_code)]
fn nop_stomp_apply_call(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    skip_call_ret4(
        process,
        image,
        CAM_STOMP_CALL_RVA,
        &CAM_STOMP_CALL_ORIG,
        &CAM_STOMP_CALL_SKIP,
    )
}

#[cfg(windows)]
fn restore_stomp_apply_call(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    let _ = patch_code(
        process,
        image,
        CAM_STOMP_CALL_RVA,
        &CAM_STOMP_CALL_SKIP,
        &CAM_STOMP_CALL_ORIG,
    );
    let _ = patch_code(
        process,
        image,
        CAM_STOMP_CALL_RVA,
        &[0x90, 0x90, 0x90, 0x90, 0x90],
        &CAM_STOMP_CALL_ORIG,
    );
    Ok(())
}

/// Share : enable + VIEW_FWD + VIEW_YAW = horizon (0,-1,0).
#[cfg(windows)]
#[allow(dead_code)]
fn write_horizon_share(process: *mut std::ffi::c_void, page: u32, enable: u32) -> Result<(), ()> {
    let mut block = [0u8; 0x1C];
    block[0..4].copy_from_slice(&enable.to_le_bytes());
    for (i, v) in HORIZON_VIEW_FWD.iter().enumerate() {
        let o_fwd = CAM_SHARE_VIEW_FWD as usize + i * 4;
        let o_yaw = CAM_SHARE_VIEW_YAW as usize + i * 4;
        block[o_fwd..o_fwd + 4].copy_from_slice(&v.to_le_bytes());
        block[o_yaw..o_yaw + 4].copy_from_slice(&v.to_le_bytes());
    }
    write_exact(process, page as u64, &block)
}

/// Early A+B (734 + 736 FPS) + stack-dir filet. Pas de publish-quat / post7712.
#[cfg(windows)]
#[allow(dead_code)]
fn install_horizon_hold_hook(
    process: *mut std::ffi::c_void,
    image: u32,
    page: u32,
) -> Result<String, String> {
    restore_camview_fwd_sites(process, image);
    let mut notes = Vec::new();
    for site in &CAMVIEW_FWD_SITES {
        match install_camview_fwd_hook(process, image, page, site) {
            Ok(()) => notes.push(format!("{} OK", site.label)),
            Err(e) if site.required => {
                restore_camview_fwd_sites(process, image);
                return Err(e);
            }
            Err(e) => notes.push(format!("{} skip: {e}", site.label)),
        }
    }
    match install_camview_stack_dir_hook(process, image, page) {
        Ok(()) => notes.push("stack-dir@334B76 OK".into()),
        Err(e) => notes.push(format!("stack-dir skip: {e}")),
    }
    Ok(notes.join("; "))
}

#[cfg(windows)]
fn restore_cam_site_bytes(process: *mut std::ffi::c_void, site: u32, orig: &[u8]) {
    let mut head = [0u8; 1];
    if read_bytes(process, site as u64, &mut head).is_err() {
        return;
    }
    if head[0] != 0xE9 {
        return;
    }
    let page_base = (site as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } != 0 {
        let _ = write_exact(process, site as u64, orig);
        unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    }
}

#[cfg(windows)]
fn restore_cam_site5(process: *mut std::ffi::c_void, site: u32, orig: &[u8; 5]) {
    restore_cam_site_bytes(process, site, orig);
}

#[cfg(windows)]
fn restore_camview_fwd_sites(process: *mut std::ffi::c_void, image: u32) {
    for site in &CAMVIEW_FWD_SITES {
        restore_cam_site_bytes(process, image + site.hook_rva, &site.orig);
    }
    // Heal hooks expérimentaux.
    restore_cam_site_bytes(process, image + CAMVIEW_FINAL_HOOK_RVA, &CAMVIEW_FINAL_ORIG);
    restore_cam_site_bytes(
        process,
        image + CAMVIEW_POST7712_HOOK_RVA,
        &CAMVIEW_POST7712_ORIG,
    );
    restore_cam_site_bytes(
        process,
        image + CAMVIEW_STACK_DIR_HOOK_RVA,
        &CAMVIEW_STACK_DIR_ORIG,
    );
    restore_cam_site_bytes(process, image + CAM_PUBLISH_VCALL_RVA, &CAM_PUBLISH_VCALL_ORIG);
    let _ = patch_code(
        process,
        image,
        CAMVIEW_SKIP_69_RVA,
        &CAMVIEW_SKIP_69_ARM,
        &CAMVIEW_SKIP_69_ORIG,
    );
    // Heal force7712 si une session précédente l'avait NOP.
    let _ = patch_code(
        process,
        image,
        CAMVIEW_FORCE7712_RVA,
        &CAMVIEW_FORCE7712_ARM,
        &CAMVIEW_FORCE7712_ORIG,
    );
    let _ = patch_code(
        process,
        image,
        CAMVIEW_FORCE7712_RVA,
        &[0x90, 0x90, 0x90, 0x90, 0x90, 0x90],
        &CAMVIEW_FORCE7712_ORIG,
    );
    let _ = restore_native_cam_publish(process, image);
}

/// Après `call 0x7712d0` : écrase `[edi+0x68]` (forward rendu CamView) + lookat/scale.
#[cfg(windows)]
#[allow(dead_code)]
fn install_camview_post7712_hook(
    process: *mut std::ffi::c_void,
    image: u32,
    page: u32,
) -> Result<(), String> {
    let abs = image + CAMVIEW_POST7712_HOOK_RVA;
    let mut cur = [0u8; 5];
    if read_bytes(process, abs as u64, &mut cur).is_err() {
        return Err(format!(
            "lecture post7712@334B2A refusée (code {})",
            last_error()
        ));
    }
    let code_abs = page + CAM_SHARE_CODE_FINAL;
    if cur[0] == 0xE9 {
        let rel = i32::from_le_bytes([cur[1], cur[2], cur[3], cur[4]]);
        let target = (abs as i64 + 5 + rel as i64) as u32;
        if target == code_abs {
            return Ok(());
        }
        return Err("site post7712@334B2A déjà hooké ailleurs".into());
    }
    if cur != CAMVIEW_POST7712_ORIG {
        return Err(format!(
            "octets inattendus post7712@334B2A: {}",
            hex_bytes(&cur)
        ));
    }

    let enable_abs = page + CAM_SHARE_ENABLE;
    let fwd_abs = page + CAM_SHARE_VIEW_FWD;
    let resume_abs = image + CAMVIEW_POST7712_RESUME_RVA;

    let mut code = Vec::with_capacity(192);
    // cmp dword [enable], 0
    code.extend_from_slice(&[0x83, 0x3D]);
    code.extend_from_slice(&enable_abs.to_le_bytes());
    code.push(0x00);
    let jz_native = code.len();
    code.extend_from_slice(&[0x0F, 0x84, 0, 0, 0, 0]);

    // [edi+0x68/6C/70] = VIEW_FWD
    for i in 0..3u32 {
        let src = fwd_abs + i * 4;
        let off = 0x68u32 + i * 4;
        code.extend_from_slice(&[0xA1]);
        code.extend_from_slice(&src.to_le_bytes());
        code.extend_from_slice(&[0x89, 0x87]);
        code.extend_from_slice(&off.to_le_bytes());
    }
    // lookat [edi+0xA4..] = eye[edi+0x74..] + fwd
    for i in 0..3u32 {
        let src = fwd_abs + i * 4;
        let eye = 0x74u32 + i * 4;
        let look = 0xA4u32 + i * 4;
        code.extend_from_slice(&[0xF3, 0x0F, 0x10, 0x87]);
        code.extend_from_slice(&eye.to_le_bytes());
        code.extend_from_slice(&[0xF3, 0x0F, 0x58, 0x05]);
        code.extend_from_slice(&src.to_le_bytes());
        code.extend_from_slice(&[0xF3, 0x0F, 0x11, 0x87]);
        code.extend_from_slice(&look.to_le_bytes());
    }
    // scaled [edi+0x8C..] = fwd * [edi+0xB0] (si scale==0 → recopier fwd)
    code.extend_from_slice(&[0xF3, 0x0F, 0x10, 0x8F, 0xB0, 0x00, 0x00, 0x00]); // movss xmm1,[edi+0xB0]
    code.extend_from_slice(&[0x0F, 0x57, 0xC0]); // xorps xmm0,xmm0
    code.extend_from_slice(&[0x0F, 0x2F, 0xC8]); // comiss xmm1,xmm0
    let jz_scale0 = code.len();
    code.extend_from_slice(&[0x0F, 0x84, 0, 0, 0, 0]); // jz copy_fwd
    for i in 0..3u32 {
        let src = fwd_abs + i * 4;
        let off = 0x8Cu32 + i * 4;
        code.extend_from_slice(&[0xF3, 0x0F, 0x10, 0x05]);
        code.extend_from_slice(&src.to_le_bytes());
        code.extend_from_slice(&[0xF3, 0x0F, 0x59, 0xC1]); // mulss xmm0,xmm1
        code.extend_from_slice(&[0xF3, 0x0F, 0x11, 0x87]);
        code.extend_from_slice(&off.to_le_bytes());
    }
    let jmp_after_scale = code.len();
    code.extend_from_slice(&[0xE9, 0, 0, 0, 0]);
    let copy_fwd_at = code.len();
    for i in 0..3u32 {
        let src = fwd_abs + i * 4;
        let off = 0x8Cu32 + i * 4;
        code.extend_from_slice(&[0xA1]);
        code.extend_from_slice(&src.to_le_bytes());
        code.extend_from_slice(&[0x89, 0x87]);
        code.extend_from_slice(&off.to_le_bytes());
    }
    let after_scale = code.len();

    // rejouer mov ecx,[ebx+0x58]; test ecx,ecx
    code.extend_from_slice(&CAMVIEW_POST7712_ORIG);
    let jmp_resume = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());

    let native_at = code.len();
    code.extend_from_slice(&CAMVIEW_POST7712_ORIG);
    let jmp_resume_nat = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());

    let patch_jz = |code: &mut [u8], at: usize, target: usize| {
        let rel = (target as i32) - ((at + 6) as i32);
        code[at + 2..at + 6].copy_from_slice(&rel.to_le_bytes());
    };
    patch_jz(&mut code, jz_native, native_at);
    patch_jz(&mut code, jz_scale0, copy_fwd_at);
    let rel_skip = (after_scale as i32) - ((jmp_after_scale + 5) as i32);
    code[jmp_after_scale + 1..jmp_after_scale + 5].copy_from_slice(&rel_skip.to_le_bytes());

    let jr = code_abs + jmp_resume as u32;
    code[jmp_resume + 1..jmp_resume + 5].copy_from_slice(&enc_rel32(jr, resume_abs));
    let jrn = code_abs + jmp_resume_nat as u32;
    code[jmp_resume_nat + 1..jmp_resume_nat + 5].copy_from_slice(&enc_rel32(jrn, resume_abs));

    write_exact(process, code_abs as u64, &code).map_err(|_| {
        format!(
            "écriture cave post7712@334B2A refusée (code {})",
            last_error()
        )
    })?;

    let rel = code_abs.wrapping_sub(abs.wrapping_add(5)).to_le_bytes();
    let hook = [0xE9, rel[0], rel[1], rel[2], rel[3]];
    let page_base = (abs as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection post7712@334B2A refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, abs as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| {
        format!(
            "écriture hook post7712@334B2A refusée (code {})",
            last_error()
        )
    })?;
    Ok(())
}

/// Cave CamView : si enable, inject VIEW_FWD dans les slots forward puis jmp post-lookat.
/// Sinon rejoue les octets natifs (movss + ja).
#[cfg(windows)]
#[allow(dead_code)]
fn install_camview_fwd_hook(
    process: *mut std::ffi::c_void,
    image: u32,
    page: u32,
    site: &CamViewFwdSite,
) -> Result<(), String> {
    let abs = image + site.hook_rva;
    let orig_len = site.orig.len();
    if orig_len < 5 || orig_len > 16 {
        return Err(format!("{}: longueur orig invalide ({})", site.label, orig_len));
    }
    let mut cur = vec![0u8; orig_len];
    if read_bytes(process, abs as u64, &mut cur).is_err() {
        return Err(format!(
            "lecture {} refusée (code {})",
            site.label,
            last_error()
        ));
    }
    let code_abs = page + site.code_off;
    if cur[0] == 0xE9 {
        let rel = i32::from_le_bytes([cur[1], cur[2], cur[3], cur[4]]);
        let target = (abs as i64 + 5 + rel as i64) as u32;
        if target == code_abs {
            return Ok(());
        }
        return Err(format!("site {} déjà hooké ailleurs", site.label));
    }
    if cur.as_slice() != site.orig {
        return Err(format!(
            "octets inattendus {}: {}",
            site.label,
            hex_bytes(&cur)
        ));
    }

    let enable_abs = page + CAM_SHARE_ENABLE;
    // Horizon hold : même buffer VIEW_FWD pour early A/B (plus de split yaw).
    let fwd_abs = page + CAM_SHARE_VIEW_FWD;
    // pushfd + push eax + push ecx = 12 bytes → offsets esp d'origine + 0xC
    const PUSH_ADJ: u32 = 0x0C;

    let mut code = Vec::with_capacity(160);
    code.extend_from_slice(&[0x9C, 0x50, 0x51]); // pushfd; push eax; push ecx

    code.extend_from_slice(&[0xA1]);
    code.extend_from_slice(&enable_abs.to_le_bytes());
    code.extend_from_slice(&[0x85, 0xC0]);
    let jz_native = code.len();
    code.extend_from_slice(&[0x0F, 0x84, 0, 0, 0, 0]);

    let write_triplet = |code: &mut Vec<u8>, offs: [u32; 3]| {
        for (i, off) in offs.iter().enumerate() {
            let stack_off = *off + PUSH_ADJ;
            let src = fwd_abs + (i as u32) * 4;
            code.extend_from_slice(&[0x8B, 0x0D]);
            code.extend_from_slice(&src.to_le_bytes());
            code.extend_from_slice(&[0x89, 0x8C, 0x24]);
            code.extend_from_slice(&stack_off.to_le_bytes());
        }
    };
    write_triplet(&mut code, site.fwd_a);
    write_triplet(&mut code, site.fwd_b);

    code.extend_from_slice(&[0x59, 0x58, 0x9D]); // pop ecx; pop eax; popfd
    let jmp_post = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());

    let native_at = code.len();
    code.extend_from_slice(&[0x59, 0x58, 0x9D]);
    code.extend_from_slice(site.orig);
    let jmp_resume = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());

    let patch_jcc = |code: &mut [u8], at: usize, target: usize| {
        let rel = (target as i32) - ((at + 6) as i32);
        code[at + 2..at + 6].copy_from_slice(&rel.to_le_bytes());
    };
    patch_jcc(&mut code, jz_native, native_at);

    let post_abs = image + site.post_rva;
    let resume_abs = abs + orig_len as u32;
    let jp = code_abs + jmp_post as u32;
    code[jmp_post + 1..jmp_post + 5].copy_from_slice(&enc_rel32(jp, post_abs));
    let jr = code_abs + jmp_resume as u32;
    code[jmp_resume + 1..jmp_resume + 5].copy_from_slice(&enc_rel32(jr, resume_abs));

    write_exact(process, code_abs as u64, &code).map_err(|_| {
        format!(
            "écriture cave {} refusée (code {})",
            site.label,
            last_error()
        )
    })?;

    let rel = code_abs.wrapping_sub(abs.wrapping_add(5)).to_le_bytes();
    let mut hook = vec![0x90u8; orig_len];
    hook[0] = 0xE9;
    hook[1..5].copy_from_slice(&rel);
    let page_base = (abs as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection {} refusée (code {})",
            site.label,
            last_error()
        ));
    }
    let wrote = write_exact(process, abs as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| {
        format!(
            "écriture hook {} refusée (code {})",
            site.label,
            last_error()
        )
    })?;
    Ok(())
}

/// @0x334B76 : remplace lookat−eye/normalize par VIEW_FWD → `[esp+0xA0/A4/A8]`, jmp 0x334C48.
#[cfg(windows)]
#[allow(dead_code)]
fn install_camview_stack_dir_hook(
    process: *mut std::ffi::c_void,
    image: u32,
    page: u32,
) -> Result<(), String> {
    let abs = image + CAMVIEW_STACK_DIR_HOOK_RVA;
    let orig_len = CAMVIEW_STACK_DIR_ORIG.len();
    let mut cur = [0u8; 9];
    if read_bytes(process, abs as u64, &mut cur).is_err() {
        return Err(format!(
            "lecture stack-dir@334B76 refusée (code {})",
            last_error()
        ));
    }
    let code_abs = page + CAM_SHARE_CODE_FINAL;
    let already_ours = if cur[0] == 0xE9 {
        let rel = i32::from_le_bytes([cur[1], cur[2], cur[3], cur[4]]);
        let target = (abs as i64 + 5 + rel as i64) as u32;
        if target == code_abs {
            true
        } else {
            return Err("site stack-dir@334B76 déjà hooké ailleurs".into());
        }
    } else if cur == CAMVIEW_STACK_DIR_ORIG {
        false
    } else {
        return Err(format!(
            "octets inattendus stack-dir@334B76: {}",
            hex_bytes(&cur)
        ));
    };

    let enable_abs = page + CAM_SHARE_ENABLE;
    let fwd_abs = page + CAM_SHARE_VIEW_FWD;
    let resume_abs = image + CAMVIEW_STACK_DIR_RESUME_RVA;
    let skip_abs = image + CAMVIEW_STACK_DIR_SKIP_RVA;

    let mut code = Vec::with_capacity(96);
    code.extend_from_slice(&[0x83, 0x3D]);
    code.extend_from_slice(&enable_abs.to_le_bytes());
    code.push(0x00);
    let jz_native = code.len();
    code.extend_from_slice(&[0x0F, 0x84, 0, 0, 0, 0]);

    // [esp+0xA0/A4/A8] = VIEW_FWD (même layout que le normalize natif)
    for i in 0..3u32 {
        let src = fwd_abs + i * 4;
        let off = 0xA0u32 + i * 4;
        code.extend_from_slice(&[0xA1]);
        code.extend_from_slice(&src.to_le_bytes());
        code.extend_from_slice(&[0x89, 0x84, 0x24]);
        code.extend_from_slice(&off.to_le_bytes());
    }
    let jmp_skip = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());

    let native_at = code.len();
    code.extend_from_slice(&CAMVIEW_STACK_DIR_ORIG);
    let jmp_resume = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());

    let patch_jz = |code: &mut [u8], at: usize, target: usize| {
        let rel = (target as i32) - ((at + 6) as i32);
        code[at + 2..at + 6].copy_from_slice(&rel.to_le_bytes());
    };
    patch_jz(&mut code, jz_native, native_at);
    let js = code_abs + jmp_skip as u32;
    code[jmp_skip + 1..jmp_skip + 5].copy_from_slice(&enc_rel32(js, skip_abs));
    let jr = code_abs + jmp_resume as u32;
    code[jmp_resume + 1..jmp_resume + 5].copy_from_slice(&enc_rel32(jr, resume_abs));

    write_exact(process, code_abs as u64, &code).map_err(|_| {
        format!(
            "écriture cave stack-dir@334B76 refusée (code {})",
            last_error()
        )
    })?;

    if already_ours {
        return Ok(());
    }

    let rel = code_abs.wrapping_sub(abs.wrapping_add(5)).to_le_bytes();
    let mut hook = vec![0x90u8; orig_len];
    hook[0] = 0xE9;
    hook[1..5].copy_from_slice(&rel);
    let page_base = (abs as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection stack-dir@334B76 refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, abs as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| {
        format!(
            "écriture hook stack-dir@334B76 refusée (code {})",
            last_error()
        )
    })?;
    Ok(())
}

/// Cave @ 0x30C8CE : si enable, copie VIEW_QUAT → [esi]=mgr+0x108 (+ miroir +0x10), puis vcall.
#[cfg(windows)]
#[allow(dead_code)]
fn install_cam_publish_quat_hook(
    process: *mut std::ffi::c_void,
    image: u32,
    page: u32,
) -> Result<(), String> {
    let abs = image + CAM_PUBLISH_VCALL_RVA;
    let orig_len = CAM_PUBLISH_VCALL_ORIG.len();
    let mut cur = [0u8; 6];
    if read_bytes(process, abs as u64, &mut cur).is_err() {
        return Err(format!(
            "lecture publish-quat@30C8CE refusée (code {})",
            last_error()
        ));
    }
    let code_abs = page + CAM_SHARE_CODE_PUBLISH;
    if cur[0] == 0xE9 {
        let rel = i32::from_le_bytes([cur[1], cur[2], cur[3], cur[4]]);
        let target = (abs as i64 + 5 + rel as i64) as u32;
        if target == code_abs {
            return Ok(());
        }
        return Err("site publish-quat@30C8CE déjà hooké ailleurs".into());
    }
    if cur != CAM_PUBLISH_VCALL_ORIG {
        return Err(format!(
            "octets inattendus publish-quat@30C8CE: {}",
            hex_bytes(&cur)
        ));
    }

    let enable_abs = page + CAM_SHARE_ENABLE;
    let quat_abs = page + CAM_SHARE_VIEW_QUAT;
    let resume_abs = image + CAM_PUBLISH_VCALL_RESUME_RVA;

    let mut code = Vec::with_capacity(160);
    code.extend_from_slice(&[0x83, 0x3D]);
    code.extend_from_slice(&enable_abs.to_le_bytes());
    code.push(0x00);
    let jz_native = code.len();
    code.extend_from_slice(&[0x0F, 0x84, 0, 0, 0, 0]);

    // Préserver eax (vtable) et ecx (this) — le `A1` clobberait sinon le call [eax+0xAC].
    code.extend_from_slice(&[0x50, 0x51]); // push eax; push ecx
    code.extend_from_slice(&[0x8B, 0x54, 0x24, 0x08]); // mov edx, [esp+8] = esi (mgr+0x108)
    code.extend_from_slice(&[0x85, 0xD2]); // test edx, edx
    let jz_bad = code.len();
    code.extend_from_slice(&[0x0F, 0x84, 0, 0, 0, 0]); // jz restore_regs
    for i in 0..4u32 {
        let src = quat_abs + i * 4;
        code.extend_from_slice(&[0xA1]);
        code.extend_from_slice(&src.to_le_bytes());
        code.extend_from_slice(&[0x89, 0x82]); // mov [edx+disp32], eax
        code.extend_from_slice(&(i * 4).to_le_bytes());
        code.extend_from_slice(&[0x89, 0x82]); // miroir +0x118
        code.extend_from_slice(&(0x10 + i * 4).to_le_bytes());
    }
    let restore_regs = code.len();
    code.extend_from_slice(&[0x59, 0x58]); // pop ecx; pop eax
    code.extend_from_slice(&CAM_PUBLISH_VCALL_ORIG); // call [eax+0xAC]
    let jmp_resume = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());

    let native_at = code.len();
    code.extend_from_slice(&CAM_PUBLISH_VCALL_ORIG);
    let jmp_resume_nat = code.len();
    code.push(0xE9);
    code.extend_from_slice(&0u32.to_le_bytes());

    let patch_jz = |code: &mut [u8], at: usize, target: usize| {
        let rel = (target as i32) - ((at + 6) as i32);
        code[at + 2..at + 6].copy_from_slice(&rel.to_le_bytes());
    };
    patch_jz(&mut code, jz_native, native_at);
    patch_jz(&mut code, jz_bad, restore_regs);

    let jr = code_abs + jmp_resume as u32;
    code[jmp_resume + 1..jmp_resume + 5].copy_from_slice(&enc_rel32(jr, resume_abs));
    let jrn = code_abs + jmp_resume_nat as u32;
    code[jmp_resume_nat + 1..jmp_resume_nat + 5].copy_from_slice(&enc_rel32(jrn, resume_abs));

    write_exact(process, code_abs as u64, &code).map_err(|_| {
        format!(
            "écriture cave publish-quat@30C8CE refusée (code {})",
            last_error()
        )
    })?;

    let rel = code_abs.wrapping_sub(abs.wrapping_add(5)).to_le_bytes();
    let mut hook = vec![0x90u8; orig_len];
    hook[0] = 0xE9;
    hook[1..5].copy_from_slice(&rel);
    let page_base = (abs as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection publish-quat@30C8CE refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, abs as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| {
        format!(
            "écriture hook publish-quat@30C8CE refusée (code {})",
            last_error()
        )
    })?;
    Ok(())
}

#[cfg(windows)]
fn uninstall_freelook_hook(process: *mut std::ffi::c_void, image: u32, page: Option<u32>) {
    restore_cam_site5(process, image + CAM_VIEW_SITE_RVA, &CAM_VIEW_CALL_ORIG);
    restore_cam_site5(process, image + CAM_BAKER_SITE_RVA, &CAM_BAKER_CALL_ORIG);
    restore_cam_site5(process, image + CAM_NATIVE_PUBLISH_RVA, &CAM_NATIVE_PUBLISH_ORIG);
    restore_cam_site5(process, image + CAM_EPILOGUE_SITE_RVA, &CAM_EPILOGUE_CALL_ORIG);
    restore_cam_site5(process, image + CAM_LEGACY_SITE_RVA, &CAM_LEGACY_CALL_ORIG);
    restore_camview_fwd_sites(process, image);
    if let Some(p) = page {
        let _ = write_exact(process, p as u64 + CAM_SHARE_ENABLE as u64, &0u32.to_le_bytes());
        unsafe {
            VirtualFreeEx(process, p as *mut std::ffi::c_void, 0, 0x8000);
        }
    }
}

/// Soft-ret sur apply abs look @0x2E3060. Retourne l'octet d'origine sauvé.
#[cfg(windows)]
fn arm_abs_look_ret_byte(
    process: *mut std::ffi::c_void,
    image: u32,
    saved: &mut Option<u8>,
) -> Result<u8, String> {
    const RVA: u32 = 0x2E3060;
    if let Some(o) = *saved {
        return Ok(o);
    }
    let addr = image as u64 + RVA as u64;
    let mut cur = [0u8; 1];
    if read_bytes(process, addr, &mut cur).is_err() {
        return Err(format!("lecture 2E3060 KO err={}", last_error()));
    }
    if cur[0] == 0xC3 {
        *saved = Some(0xC3);
        return Ok(0xC3);
    }
    let page = addr & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!("VirtualProtect 2E3060 KO err={}", last_error()));
    }
    let wrote = write_exact(process, addr, &[0xC3]);
    unsafe { VirtualProtectEx(process, page, 0x1000, old, &mut old) };
    wrote.map_err(|_| format!("write ret 2E3060 KO err={}", last_error()))?;
    *saved = Some(cur[0]);
    Ok(cur[0])
}

#[cfg(windows)]
fn spawn_fps_mouse_watcher(app: AppHandle) {
    std::thread::Builder::new()
        .name("fps-mouse".into())
        .spawn(move || {
            let raw_hwnd = setup_fps_raw_input();
            let mut ready_log = false;
            let mut session_on = false;
            let mut healed = false;
            let mut in_fps = false;
            let mut session_started_at = std::time::Instant::now();
            let mut hit_log_at = std::time::Instant::now();
            let mut last_fps_check = std::time::Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now);
            let mut last_alive_check = std::time::Instant::now();
            let mut last_pid_image: Option<(u32, u32)> = None;
            let mut process: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut hwnd_cache: *mut std::ffi::c_void = std::ptr::null_mut();

            let close_proc = |process: &mut *mut std::ffi::c_void| {
                if !process.is_null() {
                    unsafe { CloseHandle(*process) };
                    *process = std::ptr::null_mut();
                }
            };

            // Probe sol : *player+0x58 → -80 → pitchByte 199. Coupe apply abs @0x2E3060.
            const ABS_LOOK_APPLY_RVA: u32 = 0x2E3060;
            let mut abs_look_saved: Option<u8> = None;

            let session_off = |app: &AppHandle,
                               process: *mut std::ffi::c_void,
                               image: u32,
                               on: &mut bool,
                               abs_saved: &mut Option<u8>,
                               reason: &str| {
                if !*on && abs_saved.is_none() {
                    return;
                }
                if !process.is_null() {
                    if let Some(orig) = *abs_saved {
                        if orig != 0xC3 {
                            let addr = image as u64 + ABS_LOOK_APPLY_RVA as u64;
                            let page = addr & !0xFFF;
                            let mut old = 0u32;
                            if unsafe { VirtualProtectEx(process, page, 0x1000, 0x40, &mut old) } != 0
                            {
                                let _ = write_exact(process, addr, &[orig]);
                                unsafe { VirtualProtectEx(process, page, 0x1000, old, &mut old) };
                            }
                        }
                        *abs_saved = None;
                    }
                    restore_mgr_look_horizon_hook(process, image);
                    restore_apply_look_cave(process, image);
                    uninstall_freelook_hook(process, image, None);
                    restore_post_6eaee0_horizon(process, image);
                    restore_identity_horizon(process, image);
                    restore_camview_fwd_sites(process, image);
                    let _ = restore_stomp_apply_call(process, image);
                    let _ = restore_pitch_stomp_skip(process, image);
                    let _ = restore_fire_look_gates(process, image);
                    restore_fire_look_gate_one(process, image);
                    let _ = restore_abs_look_no_center(process, image);
                    let _ = write_exact(process, image as u64 + 0x9A4740, &[0u8]);
                }
                append_log(app, &format!("fps-mouse: freelook OFF reason={reason}"));
                *on = false;
            };

            let heal_match = |process: *mut std::ffi::c_void, image: u32| -> Result<(), String> {
                restore_apply_look_cave(process, image);
                uninstall_freelook_hook(process, image, None);
                restore_mgr_look_horizon_hook(process, image);
                restore_post_6eaee0_horizon(process, image);
                restore_identity_horizon(process, image);
                let site = image as u64 + 0x309D50;
                let mut cur = [0u8; 6];
                if read_bytes(process, site, &mut cur).is_ok() && cur[0] == 0xE9 {
                    let page_base = site & !0xFFF;
                    let mut old = 0u32;
                    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } != 0 {
                        let orig: [u8; 6] = [0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x0C];
                        let _ = write_exact(process, site, &orig);
                        unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
                    }
                }
                let _ = restore_stomp_apply_call(process, image);
                let _ = restore_pitch_stomp_skip(process, image);
                let _ = restore_fire_look_gates(process, image);
                restore_fire_look_gate_one(process, image);
                let _ = restore_abs_look_no_center(process, image);
                let _ = restore_native_cam_publish(process, image);
                restore_cam_site5(process, image + CAM_VIEW_SITE_RVA, &CAM_VIEW_CALL_ORIG);
                restore_cam_site5(process, image + CAM_BAKER_SITE_RVA, &CAM_BAKER_CALL_ORIG);
                restore_cam_site5(
                    process,
                    image + CAM_EPILOGUE_SITE_RVA,
                    &CAM_EPILOGUE_CALL_ORIG,
                );
                restore_camview_fwd_sites(process, image);
                let _ = write_exact(process, image as u64 + 0x9A4740, &[0u8]);
                Ok(())
            };

            match clear_settings_gyro_flag() {
                Ok(s) => append_log(&app, &format!("fps-mouse: settings gyro {s}")),
                Err(e) => append_log(&app, &format!("fps-mouse: settings gyro KO: {e}")),
            }

            if raw_hwnd.is_some() {
                append_log(&app, "fps-mouse: Raw Input OK (probe mgr+0x108 quat, touch/mgr84 OFF)");
            } else {
                append_log(&app, "fps-mouse: Raw Input KO");
            }

            loop {
                if raw_hwnd.is_some() {
                    pump_fps_raw_input();
                }
                sleep(Duration::from_millis(8));
                if raw_hwnd.is_some() {
                    pump_fps_raw_input();
                }
                // DX/DY drain hors session (évite spike au freelook ON).
                if !session_on {
                    let _ = FPS_RAW_DX.swap(0, Ordering::Relaxed);
                    let _ = FPS_RAW_DY.swap(0, Ordering::Relaxed);
                }

                let state = FPS_LOOK_STATE.lock().ok().and_then(|g| *g);
                let Some((pid, image)) = state else {
                    if let Some((_, img)) = last_pid_image {
                        session_off(
                            &app,
                            process,
                            img,
                            &mut session_on,
                            &mut abs_look_saved,
                            "no-state",
                        );
                    }
                    close_proc(&mut process);
                    in_fps = false;
                    healed = false;
                    session_on = false;
                    hwnd_cache = std::ptr::null_mut();
                    last_pid_image = None;
                    continue;
                };

                if last_pid_image != Some((pid, image)) {
                    if let Some((_, oi)) = last_pid_image {
                        session_off(
                            &app,
                            process,
                            oi,
                            &mut session_on,
                            &mut abs_look_saved,
                            "pid-change",
                        );
                    }
                    close_proc(&mut process);
                    in_fps = false;
                    healed = false;
                    session_on = false;
                    hwnd_cache = std::ptr::null_mut();
                    last_pid_image = Some((pid, image));
                }

                if process.is_null() {
                    process = unsafe { OpenProcess(0x1F0FFF, 0, pid) };
                    if process.is_null() {
                        continue;
                    }
                }

                if last_alive_check.elapsed() >= Duration::from_millis(500) {
                    last_alive_check = std::time::Instant::now();
                    let mut exit_code = 0u32;
                    let alive = unsafe { GetExitCodeProcess(process, &mut exit_code) } != 0
                        && exit_code == 259;
                    if !alive || pid == 0 {
                        session_off(
                            &app,
                            process,
                            image,
                            &mut session_on,
                            &mut abs_look_saved,
                            "dead",
                        );
                        close_proc(&mut process);
                        if let Ok(mut g) = FPS_LOOK_STATE.lock() {
                            *g = None;
                        }
                        in_fps = false;
                        healed = false;
                        session_on = false;
                        hwnd_cache = std::ptr::null_mut();
                        last_pid_image = None;
                        continue;
                    }
                }

                if !ready_log {
                    append_log(
                        &app,
                        "fps-mouse: prêt (probe packer obj58 + ret abs@2E3060)",
                    );
                    ready_log = true;
                }

                if hwnd_cache.is_null() {
                    hwnd_cache = find_game_hwnd(pid).unwrap_or(std::ptr::null_mut());
                    if hwnd_cache.is_null() {
                        continue;
                    }
                }
                let hwnd = hwnd_cache;
                let mut rect = [0i32; 4];
                if unsafe { GetClientRect(hwnd, rect.as_mut_ptr()) } == 0 {
                    hwnd_cache = std::ptr::null_mut();
                    continue;
                }
                let width = rect[2] - rect[0];
                let height = rect[3] - rect[1];
                if width < 64 || height < 64 {
                    continue;
                }

                let fg = unsafe { GetForegroundWindow() };
                let mut fg_pid = 0u32;
                if !fg.is_null() {
                    unsafe { GetWindowThreadProcessId(fg, &mut fg_pid) };
                }
                if !(fg_pid == pid || fg == hwnd) {
                    continue;
                }

                if last_fps_check.elapsed() >= Duration::from_millis(400) {
                    last_fps_check = std::time::Instant::now();
                    if !match_player_ready(process, image) {
                        session_off(
                            &app,
                            process,
                            image,
                            &mut session_on,
                            &mut abs_look_saved,
                            "player",
                        );
                        in_fps = false;
                        healed = false;
                        continue;
                    }
                    in_fps = true;
                }
                if !in_fps {
                    continue;
                }

                if !healed {
                    match heal_match(process, image) {
                        Ok(()) => {
                            healed = true;
                            append_log(&app, "fps-mouse: heal OK (sites natifs)");
                        }
                        Err(e) => {
                            if hit_log_at.elapsed() >= Duration::from_secs(2) {
                                hit_log_at = std::time::Instant::now();
                                append_log(&app, &format!("fps-mouse: heal KO: {e}"));
                            }
                            continue;
                        }
                    }
                }

                let _ = write_exact(process, image as u64 + 0x9A4740, &[0u8]);

                if !session_on {
                    match arm_abs_look_ret_byte(process, image, &mut abs_look_saved) {
                        Ok(orig) => {
                            session_on = true;
                            session_started_at = std::time::Instant::now();
                            append_log(
                                &app,
                                &format!(
                                    "fps-mouse: freelook ON abs@2E3060 ret (saved=0x{orig:02X}) + packer pitch/yaw DX/DY"
                                ),
                            );
                        }
                        Err(e) => {
                            if hit_log_at.elapsed() >= Duration::from_secs(2) {
                                hit_log_at = std::time::Instant::now();
                                append_log(&app, &format!("fps-mouse: abs@2E3060 arm KO: {e}"));
                            }
                            continue;
                        }
                    }
                }

                // Packer : pitch +0x58 (−80≈sol), yaw +0x60. Scale commun.
                const PACKER_LOOK_SCALE: f32 = 0.06;
                const PACKER_PITCH_MIN: f32 = -70.0;
                const PACKER_PITCH_MAX: f32 = 70.0;
                const PACKER_PITCH_FLOOR_GUARD: f32 = -75.0;
                let dx = FPS_RAW_DX.swap(0, Ordering::Relaxed);
                let dy = FPS_RAW_DY.swap(0, Ordering::Relaxed);
                let mut pitch_now = f32::NAN;
                let mut yaw_now = f32::NAN;
                if let Some(a) = resolve_packer_angles(process, image) {
                    let mut p = a.obj_pitch_raw;
                    let mut y = a.obj_yaw_raw;
                    // Filet anti-sol (stomp résiduel) — pas un lock à 0.
                    if p < PACKER_PITCH_FLOOR_GUARD {
                        p = PACKER_PITCH_MIN;
                    }
                    if dy != 0 {
                        // dy>0 souris bas → pitch plus négatif (vers le sol).
                        p -= dy as f32 * PACKER_LOOK_SCALE;
                    }
                    p = p.clamp(PACKER_PITCH_MIN, PACKER_PITCH_MAX);
                    if dx != 0 {
                        // dx>0 souris droite → yaw ↓ (convention packer, sens FPS).
                        y -= dx as f32 * PACKER_LOOK_SCALE;
                    }
                    pitch_now = p;
                    yaw_now = y;
                    let pb = p.to_le_bytes();
                    let yb = y.to_le_bytes();
                    let _ = write_exact(process, a.obj as u64 + 0x58, &pb);
                    let _ = write_exact(process, a.player as u64 + 0x58, &pb);
                    let _ = write_exact(process, a.obj as u64 + 0x60, &yb);
                    let _ = write_exact(process, a.player as u64 + 0x60, &yb);
                }

                if hit_log_at.elapsed() >= Duration::from_millis(500) {
                    hit_log_at = std::time::Instant::now();
                    let uptime_ms = session_started_at.elapsed().as_millis();
                    let ctl = resolve_fps_camera_ctl(process, image);
                    let pitch = ctl.and_then(|c| read_f32_at(process, c as u64 + 0x88));
                    let pack = resolve_packer_angles(process, image);
                    let pitch_s = match pitch {
                        Some(p) => format!("ctl88={p:.3}"),
                        None => "ctl88=?".into(),
                    };
                    let pack_s = match pack {
                        Some(p) => format!(
                            "obj58={:.2} obj60={:.2} estPitch={} yawByte={}",
                            p.obj_pitch_raw, p.obj_yaw_raw, p.est_pitch_byte, p.est_yaw_byte
                        ),
                        None => "packer=?".into(),
                    };
                    append_log(
                        &app,
                        &format!(
                            "fps-mouse: packer tick uptimeMs={uptime_ms} absRet={} dx={dx} dy={dy} wrotePitch={pitch_now:.2} wroteYaw={yaw_now:.2} {pitch_s} {pack_s}",
                            abs_look_saved.is_some() as u8
                        ),
                    );
                }
            }
        })
        .ok();
}
#[cfg(windows)]
fn find_game_hwnd(pid: u32) -> Option<*mut std::ffi::c_void> {
    struct EnumState {
        pid: u32,
        hwnd: *mut std::ffi::c_void,
        area: i32,
    }
    unsafe extern "system" fn enum_proc(
        hwnd: *mut std::ffi::c_void,
        lparam: isize,
    ) -> i32 {
        let state = &mut *(lparam as *mut EnumState);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let mut rect = [0i32; 4];
        if GetClientRect(hwnd, rect.as_mut_ptr()) == 0 {
            return 1;
        }
        let w = rect[2] - rect[0];
        let h = rect[3] - rect[1];
        if w < 64 || h < 64 {
            return 1;
        }
        let mut wpid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut wpid);
        let mut title = [0u16; 64];
        let n = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
        let title = if n > 0 {
            String::from_utf16_lossy(&title[..n as usize])
        } else {
            String::new()
        };
        // UWP : la fenêtre visible est souvent ApplicationFrameHost, pas le pid jeu.
        let match_pid = wpid == state.pid;
        let match_title = title.eq_ignore_ascii_case("Blitz Brigade");
        if !match_pid && !match_title {
            return 1;
        }
        let area = w.saturating_mul(h);
        if area > state.area {
            state.area = area;
            state.hwnd = hwnd;
        }
        1
    }
    let mut state = EnumState {
        pid,
        hwnd: std::ptr::null_mut(),
        area: 0,
    };
    unsafe {
        EnumWindows(
            enum_proc,
            &mut state as *mut EnumState as isize,
        );
    }
    if state.hwnd.is_null() {
        None
    } else {
        Some(state.hwnd)
    }
}

/// Respawn (RVA 0x31171F) : si index caméra type 2 == -1, SetCamera(0x2755) puis retry.
/// Corrige active=-1 (pas de FPS) → pitch bloqué bas + crash contrôleur NULL ~4s.
#[cfg(windows)]
fn install_respawn_camera_fix(
    process: *mut std::ffi::c_void,
    image: u32,
) -> Result<(), String> {
    const SITE_RVA: u32 = 0x31171F;
    const SITE_LEN: usize = 10;
    const EXPECT: [u8; 10] = [0x85, 0xC0, 0x78, 0x0C, 0x8B, 0x8F, 0x14, 0x01, 0x00, 0x00];
    // Après le site : push eax ; call SetActive… — reprise si index OK.
    const CONTINUE_RVA: u32 = 0x311729;
    // Branche js originale (pas d'activation).
    const SKIP_RVA: u32 = 0x31172F;
    const SET_CAMERA_RVA: u32 = 0x3088E0;
    const GET_TYPE_ID_RVA: u32 = 0x306E40;
    const INDEX_OF_RVA: u32 = 0x306E70;
    const FPS_CAM_ID: u32 = 0x2755;

    let site = image + SITE_RVA;
    let mut cur = [0u8; SITE_LEN];
    if read_bytes(process, site as u64, &mut cur).is_err() {
        return Err(format!(
            "lecture caméra Respawn refusée (code {})",
            last_error()
        ));
    }
    if cur[0] == 0xE9 {
        return Ok(()); // déjà hooké
    }
    if cur != EXPECT {
        return Err(format!(
            "octets inattendus au RVA {SITE_RVA:X}: {}",
            hex_bytes(&cur)
        ));
    }

    let cave_ptr = unsafe {
        VirtualAllocEx(
            process,
            std::ptr::null_mut(),
            0x1000,
            0x3000,
            0x40,
        )
    };
    if cave_ptr.is_null() {
        return Err(format!(
            "VirtualAllocEx caméra refusé (code {})",
            last_error()
        ));
    }
    let cave = cave_ptr as u32;

    // Cave 69 octets (voir plan RE).
    let mut code = Vec::with_capacity(69);
    code.extend_from_slice(&[0x85, 0xC0]); // test eax,eax
    code.extend_from_slice(&[0x79, 0x2D]); // jns have (+49)
    code.extend_from_slice(&[0x8B, 0x8F, 0x14, 0x01, 0x00, 0x00]); // mov ecx,[edi+0x114]
    code.extend_from_slice(&[0x6A, 0x00]); // push 0
    code.push(0x68);
    code.extend_from_slice(&FPS_CAM_ID.to_le_bytes()); // push 0x2755
    code.push(0xE8);
    code.extend_from_slice(&enc_rel32(cave + 17, image + SET_CAMERA_RVA));
    code.extend_from_slice(&[0x8B, 0x8F, 0x14, 0x01, 0x00, 0x00]); // mov ecx,[edi+0x114]
    code.extend_from_slice(&[0x6A, 0x00, 0x6A, 0x02]); // push 0 ; push 2
    code.push(0xE8);
    code.extend_from_slice(&enc_rel32(cave + 32, image + GET_TYPE_ID_RVA));
    code.extend_from_slice(&[0x8B, 0x8F, 0x14, 0x01, 0x00, 0x00]); // mov ecx,[edi+0x114]
    code.push(0x50); // push eax
    code.push(0xE8);
    code.extend_from_slice(&enc_rel32(cave + 44, image + INDEX_OF_RVA));
    // have:
    code.extend_from_slice(&[0x85, 0xC0]); // test eax,eax
    code.extend_from_slice(&[0x78, 0x0B]); // js skip (+64)
    code.extend_from_slice(&[0x8B, 0x8F, 0x14, 0x01, 0x00, 0x00]); // mov ecx,[edi+0x114]
    code.push(0xE9);
    code.extend_from_slice(&enc_rel32(cave + 59, image + CONTINUE_RVA));
    // skip:
    code.push(0xE9);
    code.extend_from_slice(&enc_rel32(cave + 64, image + SKIP_RVA));
    debug_assert_eq!(code.len(), 69);

    write_exact(process, cave as u64, &code).map_err(|_| {
        format!("écriture cave caméra refusée (code {})", last_error())
    })?;

    let mut hook = [0x90u8; SITE_LEN];
    hook[0] = 0xE9;
    hook[1..5].copy_from_slice(&enc_rel32(site, cave));
    let page_base = (site as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection site caméra refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, site as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| format!("écriture hook caméra refusée (code {})", last_error()))?;
    Ok(())
}

#[cfg(windows)]
fn install_discover_inject(
    process: *mut std::ffi::c_void,
    image: u32,
    lobby: &LobbyConfig,
) -> Result<(), String> {
    const SITE_RVA: u32 = DISCOVER_INJECT_SITE_RVA;
    const OLD_SITE_RVA: u32 = 0x34F07F;

    let lan = lan_ipv4_octets()?;

    // Remettre l'ancien site recv s'il traîne encore un JMP.
    let mut old_site = [0u8; 5];
    if read_bytes(process, image as u64 + OLD_SITE_RVA as u64, &mut old_site).is_ok()
        && old_site[0] == 0xE9
    {
        let restore = [0x85u8, 0xFF, 0x74, 0x8D, 0x8D];
        let page = (image as u64 + OLD_SITE_RVA as u64) & !0xFFF;
        let mut prot = 0u32;
        if unsafe { VirtualProtectEx(process, page, 0x1000, 0x40, &mut prot) } != 0 {
            let _ = write_exact(process, image as u64 + OLD_SITE_RVA as u64, &restore);
            unsafe { VirtualProtectEx(process, page, 0x1000, prot, &mut prot) };
        }
    }

    if refresh_discover_inject_packet(process, image, lobby)? {
        return Ok(());
    }

    let mut orig = [0u8; 6];
    if read_bytes(process, image as u64 + SITE_RVA as u64, &mut orig).is_err() {
        return Err(format!(
            "lecture inject site refusée (code {})",
            last_error()
        ));
    }
    let expected = [0x0Fu8, 0x8E, 0x29, 0x01, 0x00, 0x00];
    if orig != expected {
        return Err(format!(
            "octets inattendus au RVA {SITE_RVA:X}: {}",
            hex_bytes(&orig)
        ));
    }

    let page = unsafe {
        VirtualAllocEx(
            process,
            std::ptr::null_mut(),
            0x1000,
            0x3000,
            0x40,
        )
    };
    if page.is_null() {
        return Err(format!(
            "VirtualAllocEx inject refusé (code {})",
            last_error()
        ));
    }
    let page = page as u32;
    let code = build_select_inject_stub(page, image, lan)?;
    let mut page_buf = vec![0u8; 0x1000];
    page_buf[..code.len()].copy_from_slice(&code);
    write_exact(process, page as u64, &page_buf).map_err(|_| {
        format!("écriture cave inject refusée (code {})", last_error())
    })?;
    write_inject_packet_blobs(process, page, lobby, true)?;

    let site = image + SITE_RVA;
    let rel = page.wrapping_sub(site.wrapping_add(5)).to_le_bytes();
    let hook = [0xE9, rel[0], rel[1], rel[2], rel[3], 0x90];
    let page_base = (site as u64) & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page_base, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection inject site refusée (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, site as u64, &hook);
    unsafe { VirtualProtectEx(process, page_base, 0x1000, old, &mut old) };
    wrote.map_err(|_| format!("écriture hook inject refusée (code {})", last_error()))?;

    install_search_enter_flag_hook(process, image, page)?;
    install_join_arm_hook(process, image, page)?;
    Ok(())
}

#[cfg(windows)]
fn patch_discover_to_lan(process: *mut std::ffi::c_void, image: u32) -> Result<(), String> {
    let octets = lan_ipv4_octets()?;
    let mut replace = [0u8; 10];
    replace[0] = 0xB8;
    replace[1..5].copy_from_slice(&octets);
    replace[5..].fill(0x90);

    let addr = image as u64 + 0x34F6D1;
    let mut current = [0u8; 10];
    if read_bytes(process, addr, &mut current).is_err() {
        return Err(format!(
            "lecture refusée au RVA 34F6D1 (code {})",
            last_error()
        ));
    }
    if current == replace {
        return Ok(());
    }
    let original = [0x6A, 0xFF, 0xE8, 0xB8, 0x0B, 0xD9, 0xFF, 0x83, 0xC4, 0x04];
    let loopback = [0xB8, 0x7F, 0x00, 0x00, 0x01, 0x90, 0x90, 0x90, 0x90, 0x90];
    if current != original && current != loopback && current[0] != 0xB8 {
        return Err(format!(
            "octets inattendus au RVA 34F6D1: {}",
            hex_bytes(&current)
        ));
    }
    // Accepte original, ancien 127.0.0.1, ou un ancien MOV EAX IP (repatch).
    let page = addr & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection refusée au RVA 34F6D1 (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, addr, &replace);
    unsafe { VirtualProtectEx(process, page, 0x1000, old, &mut old) };
    wrote.map_err(|_| {
        format!(
            "écriture refusée au RVA 34F6D1 (code {})",
            last_error()
        )
    })?;
    Ok(())
}

#[cfg(windows)]
#[repr(C)]
struct IpAdapterAddresses {
    length: u32,
    if_index: u32,
    next: *mut IpAdapterAddresses,
    adapter_name: *mut i8,
    first_unicast: *mut IpAdapterUnicastAddress,
}

#[cfg(windows)]
#[repr(C)]
struct IpAdapterUnicastAddress {
    length: u32,
    flags: u32,
    next: *mut IpAdapterUnicastAddress,
    lp_sockaddr: *const SockAddrRaw,
    sockaddr_length: i32,
}

#[cfg(windows)]
#[repr(C)]
struct SockAddrRaw {
    sa_family: u16,
    sa_data: [u8; 14],
}

#[cfg(windows)]
fn patch_code(
    process: *mut std::ffi::c_void,
    image: u32,
    rva: u32,
    expect: &[u8],
    replace: &[u8],
) -> Result<(), String> {
    let addr = (image as u64) + (rva as u64);
    let mut current = vec![0u8; replace.len()];
    if read_bytes(process, addr, &mut current).is_err() {
        return Err(format!("lecture refusée au RVA {rva:X} (code {})", last_error()));
    }
    if current == replace {
        return Ok(());
    }
    if current != expect {
        return Err(format!(
            "octets inattendus au RVA {rva:X}: {}",
            hex_bytes(&current)
        ));
    }
    let page = addr & !0xFFF;
    let mut old = 0u32;
    if unsafe { VirtualProtectEx(process, page, 0x1000, 0x40, &mut old) } == 0 {
        return Err(format!(
            "protection refusée au RVA {rva:X} (code {})",
            last_error()
        ));
    }
    let wrote = write_exact(process, addr, replace);
    unsafe { VirtualProtectEx(process, page, 0x1000, old, &mut old) };
    wrote.map_err(|_| format!("écriture refusée au RVA {rva:X} (code {})", last_error()))?;
    if read_bytes(process, addr, &mut current).is_err() || current != replace {
        return Err(format!("relecture inattendue au RVA {rva:X}", ));
    }
    Ok(())
}

#[cfg(windows)]
fn read_bytes(process: *mut std::ffi::c_void, addr: u64, out: &mut [u8]) -> Result<(), ()> {
    let mut read = 0usize;
    let ok = unsafe {
        ReadProcessMemory(
            process,
            addr as *const _,
            out.as_mut_ptr() as *mut _,
            out.len(),
            &mut read,
        )
    };
    if ok == 0 || read != out.len() {
        Err(())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
struct ProcessHandle(*mut std::ffi::c_void);

#[cfg(windows)]
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(windows)]
fn read_exact<T>(process: *mut std::ffi::c_void, addr: u64, out: &mut T) -> Result<(), ()> {
    let mut read = 0usize;
    let ok = unsafe {
        ReadProcessMemory(
            process,
            addr as *const _,
            out as *mut T as *mut _,
            std::mem::size_of::<T>(),
            &mut read,
        )
    };
    if ok == 0 || read != std::mem::size_of::<T>() {
        Err(())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn write_exact(process: *mut std::ffi::c_void, addr: u64, bytes: &[u8]) -> Result<(), ()> {
    let mut written = 0usize;
    let ok = unsafe {
        WriteProcessMemory(
            process,
            addr as *mut _,
            bytes.as_ptr() as *const _,
            bytes.len(),
            &mut written,
        )
    };
    if ok == 0 || written != bytes.len() {
        Err(())
    } else {
        Ok(())
    }
}

/// Suspend tous les threads du PID (sauf le nôtre) le temps d'écrire du code live.
#[cfg(windows)]
fn with_process_threads_suspended<T>(
    pid: u32,
    f: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    #[repr(C)]
    struct ThreadEntry32 {
        size: u32,
        usage: u32,
        tid: u32,
        owner_pid: u32,
        base_pri: i32,
        delta_pri: i32,
        flags: u32,
    }

    let self_tid = unsafe { GetCurrentThreadId() };
    let snap = unsafe { CreateToolhelp32Snapshot(0x4, 0) }; // TH32CS_SNAPTHREAD
    if snap.is_null() || snap == (-1isize as *mut std::ffi::c_void) {
        return f();
    }
    let snap = ProcessHandle(snap);
    let mut entry: ThreadEntry32 = unsafe { std::mem::zeroed() };
    entry.size = std::mem::size_of::<ThreadEntry32>() as u32;
    let mut handles: Vec<*mut std::ffi::c_void> = Vec::new();
    unsafe {
        if Thread32First(snap.0, &mut entry as *mut _ as *mut _) != 0 {
            loop {
                if entry.owner_pid == pid && entry.tid != self_tid {
                    // THREAD_SUSPEND_RESUME
                    let th = OpenThread(0x0002, 0, entry.tid);
                    if !th.is_null() {
                        SuspendThread(th);
                        handles.push(th);
                    }
                }
                if Thread32Next(snap.0, &mut entry as *mut _ as *mut _) == 0 {
                    break;
                }
            }
        }
    }
    let result = f();
    for th in handles {
        unsafe {
            ResumeThread(th);
            CloseHandle(th);
        }
    }
    result
}

#[cfg(windows)]
fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
fn last_error() -> u32 {
    unsafe { GetLastError() }
}

#[cfg(windows)]
fn enable_debug_privilege() {
    #[repr(C)]
    struct Luid {
        low: u32,
        high: i32,
    }
    #[repr(C)]
    struct TokenPrivileges {
        count: u32,
        luid: Luid,
        attr: u32,
    }
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), 0x0028, &mut token) == 0 {
            return;
        }
        let token = ProcessHandle(token);
        let name: Vec<u16> = "SeDebugPrivilege".encode_utf16().chain(Some(0)).collect();
        let mut luid = Luid { low: 0, high: 0 };
        if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid as *mut Luid as *mut _)
            == 0
        {
            return;
        }
        let mut tp = TokenPrivileges {
            count: 1,
            luid,
            attr: 0x2,
        };
        AdjustTokenPrivileges(
            token.0,
            0,
            &mut tp as *mut TokenPrivileges as *mut _,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
}

#[cfg(windows)]
fn find_game_pid() -> Option<u32> {
    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        cnt_usage: u32,
        pid: u32,
        heap: usize,
        module_id: u32,
        threads: u32,
        parent: u32,
        pri: i32,
        flags: u32,
        exe: [u16; 260],
    }
    unsafe {
        let snap = CreateToolhelp32Snapshot(0x2, 0);
        if snap.is_null() || snap == (-1isize as *mut std::ffi::c_void) {
            return None;
        }
        let snap = ProcessHandle(snap);
        let mut entry: ProcessEntry32W = std::mem::zeroed();
        entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
        if Process32FirstW(snap.0, &mut entry as *mut ProcessEntry32W as *mut _) == 0 {
            return None;
        }
        loop {
            let end = entry
                .exe
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.exe.len());
            let name = String::from_utf16_lossy(&entry.exe[..end]);
            if name.eq_ignore_ascii_case("InfamousHeroesEntry.exe") {
                return Some(entry.pid);
            }
            if Process32NextW(snap.0, &mut entry as *mut ProcessEntry32W as *mut _) == 0 {
                return None;
            }
        }
    }
}

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn GetForegroundWindow() -> *mut std::ffi::c_void;
    fn GetClientRect(hwnd: *mut std::ffi::c_void, rect: *mut i32) -> i32;
    fn GetWindowThreadProcessId(hwnd: *mut std::ffi::c_void, pid: *mut u32) -> u32;
    fn IsWindowVisible(hwnd: *mut std::ffi::c_void) -> i32;
    fn GetWindowTextW(hwnd: *mut std::ffi::c_void, buf: *mut u16, max: i32) -> i32;
    fn EnumWindows(
        cb: unsafe extern "system" fn(*mut std::ffi::c_void, isize) -> i32,
        lparam: isize,
    ) -> i32;
    fn RegisterClassW(wc: *const RawWndClass) -> u16;
    fn CreateWindowExW(
        ex: u32,
        class: *const u16,
        name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: *mut std::ffi::c_void,
        menu: *mut std::ffi::c_void,
        instance: *mut std::ffi::c_void,
        param: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn DefWindowProcW(
        hwnd: *mut std::ffi::c_void,
        msg: u32,
        wp: usize,
        lp: isize,
    ) -> isize;
    fn PeekMessageW(
        msg: *mut RawMsg,
        hwnd: *mut std::ffi::c_void,
        min: u32,
        max: u32,
        remove: u32,
    ) -> i32;
    fn TranslateMessage(msg: *const RawMsg) -> i32;
    fn DispatchMessageW(msg: *const RawMsg) -> isize;
    fn RegisterRawInputDevices(devs: *const RawInputDevice, num: u32, size: u32) -> i32;
    fn GetRawInputData(
        raw: *mut std::ffi::c_void,
        cmd: u32,
        data: *mut std::ffi::c_void,
        size: *mut u32,
        header_size: u32,
    ) -> u32;
    fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
    fn ClientToScreen(hwnd: *mut std::ffi::c_void, pt: *mut i32) -> i32;
    fn InitializeTouchInjection(max_count: u32, mode: u32) -> i32;
    fn InjectTouchInput(count: u32, contacts: *const PointerTouchInfo) -> i32;
    fn CreateSyntheticPointerDevice(
        pointer_type: u32,
        max_count: u32,
        mode: u32,
    ) -> *mut std::ffi::c_void;
    fn InjectSyntheticPointerInput(
        device: *mut std::ffi::c_void,
        pointer_info: *const PointerTypeInfo,
        count: u32,
    ) -> i32;
    #[allow(dead_code)]
    fn DestroySyntheticPointerDevice(device: *mut std::ffi::c_void);
}

/// POINTER_INPUT_TYPE / flags pour InjectTouchInput.
#[cfg(windows)]
const PT_TOUCH: u32 = 2;
#[cfg(windows)]
const TOUCH_FEEDBACK_DEFAULT: u32 = 0x1;
#[cfg(windows)]
const POINTER_FEEDBACK_DEFAULT: u32 = 0x1;
#[cfg(windows)]
const POINTER_FLAG_INRANGE: u32 = 0x0000_0002;
#[cfg(windows)]
const POINTER_FLAG_INCONTACT: u32 = 0x0000_0004;
#[cfg(windows)]
const POINTER_FLAG_PRIMARY: u32 = 0x0000_2000;
#[cfg(windows)]
const POINTER_FLAG_DOWN: u32 = 0x0001_0000;
#[cfg(windows)]
const POINTER_FLAG_UPDATE: u32 = 0x0002_0000;
#[cfg(windows)]
const POINTER_FLAG_UP: u32 = 0x0004_0000;
#[cfg(windows)]
const TOUCH_MASK_CONTACTAREA: u32 = 0x1;
#[cfg(windows)]
const TOUCH_MASK_ORIENTATION: u32 = 0x2;
#[cfg(windows)]
const TOUCH_MASK_PRESSURE: u32 = 0x4;
/// Doit être < maxCount de InitializeTouchInjection (0 .. max-1).
#[cfg(windows)]
const TOUCH_LOOK_POINTER_ID: u32 = 0;
#[cfg(windows)]
/// Raw Input → pixels contact. Le look tactile du jeu est déjà très sensible.
const TOUCH_LOOK_SCALE: f32 = 0.012;
#[cfg(windows)]
const TOUCH_LOOK_MAX_STEP: i32 = 1;
/// Idle → TOUCH_UP (stomp skip+apply armés ; pas de hold permanent = anti force-bas).
#[cfg(windows)]
const TOUCH_IDLE_UP_MS: u64 = 120;
#[cfg(windows)]
const TOUCH_FLAG_DOWN: u32 =
    POINTER_FLAG_PRIMARY | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_DOWN;
#[cfg(windows)]
const TOUCH_FLAG_UPDATE: u32 =
    POINTER_FLAG_PRIMARY | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT | POINTER_FLAG_UPDATE;
#[cfg(windows)]
const TOUCH_FLAG_UP: u32 = POINTER_FLAG_PRIMARY | POINTER_FLAG_INRANGE | POINTER_FLAG_UP;

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TouchBackend {
    Inject,
    Synthetic,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct WinPoint {
    x: i32,
    y: i32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct PointerInfo {
    pointer_type: u32,
    pointer_id: u32,
    frame_id: u32,
    pointer_flags: u32,
    source_device: *mut std::ffi::c_void,
    hwnd_target: *mut std::ffi::c_void,
    pt_pixel: WinPoint,
    pt_himetric: WinPoint,
    pt_pixel_raw: WinPoint,
    pt_himetric_raw: WinPoint,
    dw_time: u32,
    history_count: u32,
    input_data: i32,
    dw_key_states: u32,
    performance_count: u64,
    button_change_type: i32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct PointerTouchInfo {
    pointer_info: PointerInfo,
    touch_flags: u32,
    touch_mask: u32,
    rc_contact: WinRect,
    contact_orientation: u32,
    pressure: u32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct PointerTypeInfo {
    pointer_type: u32,
    touch_info: PointerTouchInfo,
}

#[cfg(windows)]
fn client_to_screen_pt(hwnd: *mut std::ffi::c_void, cx: i32, cy: i32) -> Option<(i32, i32)> {
    let mut pt = [cx, cy];
    if unsafe { ClientToScreen(hwnd, pt.as_mut_ptr()) } == 0 {
        return None;
    }
    Some((pt[0], pt[1]))
}

#[cfg(windows)]
fn make_pointer_touch_info(
    hwnd: *mut std::ffi::c_void,
    screen_x: i32,
    screen_y: i32,
    flags: u32,
) -> PointerTouchInfo {
    PointerTouchInfo {
        pointer_info: PointerInfo {
            pointer_type: PT_TOUCH,
            pointer_id: TOUCH_LOOK_POINTER_ID,
            frame_id: 0,
            pointer_flags: flags,
            source_device: std::ptr::null_mut(),
            hwnd_target: hwnd,
            pt_pixel: WinPoint {
                x: screen_x,
                y: screen_y,
            },
            pt_himetric: WinPoint { x: 0, y: 0 },
            pt_pixel_raw: WinPoint {
                x: screen_x,
                y: screen_y,
            },
            pt_himetric_raw: WinPoint { x: 0, y: 0 },
            dw_time: 0,
            history_count: 1,
            input_data: 0,
            dw_key_states: 0,
            performance_count: 0,
            button_change_type: 0,
        },
        touch_flags: 0,
        touch_mask: TOUCH_MASK_CONTACTAREA | TOUCH_MASK_ORIENTATION | TOUCH_MASK_PRESSURE,
        rc_contact: WinRect {
            left: screen_x - 4,
            top: screen_y - 4,
            right: screen_x + 4,
            bottom: screen_y + 4,
        },
        contact_orientation: 90,
        pressure: 512,
    }
}

#[cfg(windows)]
fn inject_touch_contact(
    hwnd: *mut std::ffi::c_void,
    screen_x: i32,
    screen_y: i32,
    flags: u32,
) -> bool {
    let info = make_pointer_touch_info(hwnd, screen_x, screen_y, flags);
    unsafe { InjectTouchInput(1, &info) != 0 }
}

#[cfg(windows)]
fn synthetic_touch_contact(
    device: *mut std::ffi::c_void,
    hwnd: *mut std::ffi::c_void,
    screen_x: i32,
    screen_y: i32,
    flags: u32,
) -> bool {
    if device.is_null() {
        return false;
    }
    let info = PointerTypeInfo {
        pointer_type: PT_TOUCH,
        touch_info: make_pointer_touch_info(hwnd, screen_x, screen_y, flags),
    };
    unsafe { InjectSyntheticPointerInput(device, &info, 1) != 0 }
}

#[cfg(windows)]
fn touch_contact(
    backend: TouchBackend,
    syn_dev: *mut std::ffi::c_void,
    hwnd: *mut std::ffi::c_void,
    screen_x: i32,
    screen_y: i32,
    flags: u32,
) -> bool {
    match backend {
        TouchBackend::Inject => inject_touch_contact(hwnd, screen_x, screen_y, flags),
        TouchBackend::Synthetic => {
            synthetic_touch_contact(syn_dev, hwnd, screen_x, screen_y, flags)
        }
    }
}

#[cfg(windows)]
#[repr(C)]
struct RawWndClass {
    style: u32,
    wnd_proc: Option<
        unsafe extern "system" fn(*mut std::ffi::c_void, u32, usize, isize) -> isize,
    >,
    cls_extra: i32,
    wnd_extra: i32,
    instance: *mut std::ffi::c_void,
    icon: *mut std::ffi::c_void,
    cursor: *mut std::ffi::c_void,
    background: *mut std::ffi::c_void,
    menu_name: *const u16,
    class_name: *const u16,
}

#[cfg(windows)]
#[repr(C)]
struct RawMsg {
    hwnd: *mut std::ffi::c_void,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt_x: i32,
    pt_y: i32,
    private: u32,
}

#[cfg(windows)]
#[repr(C)]
struct RawInputDevice {
    usage_page: u16,
    usage: u16,
    flags: u32,
    hwnd_target: *mut std::ffi::c_void,
}

#[cfg(windows)]
#[repr(C)]
struct RawInputHeader {
    ty: u32,
    size: u32,
    device: *mut std::ffi::c_void,
    wparam: usize,
}

/// RAWMOUSE (MSVC x64) : usFlags + pad puis union ULONG.
#[cfg(windows)]
#[repr(C)]
struct RawMouse {
    flags: u16,
    _pad: u16,
    buttons: u32,
    raw_buttons: u32,
    last_x: i32,
    last_y: i32,
    extra: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn GetExitCodeProcess(process: *mut std::ffi::c_void, exit_code: *mut u32) -> i32;
    fn GetLastError() -> u32;
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
    fn OpenProcessToken(
        process: *mut std::ffi::c_void,
        access: u32,
        token: *mut *mut std::ffi::c_void,
    ) -> i32;
    fn ReadProcessMemory(
        process: *mut std::ffi::c_void,
        addr: *const std::ffi::c_void,
        buf: *mut std::ffi::c_void,
        size: usize,
        read: *mut usize,
    ) -> i32;
    fn WriteProcessMemory(
        process: *mut std::ffi::c_void,
        addr: *mut std::ffi::c_void,
        buf: *const std::ffi::c_void,
        size: usize,
        written: *mut usize,
    ) -> i32;
    fn VirtualAllocEx(
        process: *mut std::ffi::c_void,
        addr: *mut std::ffi::c_void,
        size: usize,
        kind: u32,
        prot: u32,
    ) -> *mut std::ffi::c_void;
    fn VirtualFreeEx(
        process: *mut std::ffi::c_void,
        addr: *mut std::ffi::c_void,
        size: usize,
        free_type: u32,
    ) -> i32;
    fn VirtualProtectEx(
        process: *mut std::ffi::c_void,
        addr: u64,
        size: usize,
        prot: u32,
        old: *mut u32,
    ) -> i32;
    #[allow(dead_code)]
    fn VirtualQueryEx(
        process: *mut std::ffi::c_void,
        addr: *const std::ffi::c_void,
        mbi: *mut std::ffi::c_void,
        len: usize,
    ) -> usize;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut std::ffi::c_void;
    fn Process32FirstW(snap: *mut std::ffi::c_void, entry: *mut std::ffi::c_void) -> i32;
    fn Process32NextW(snap: *mut std::ffi::c_void, entry: *mut std::ffi::c_void) -> i32;
    fn Module32FirstW(snap: *mut std::ffi::c_void, entry: *mut std::ffi::c_void) -> i32;
    fn Module32NextW(snap: *mut std::ffi::c_void, entry: *mut std::ffi::c_void) -> i32;
    fn Thread32First(snap: *mut std::ffi::c_void, entry: *mut std::ffi::c_void) -> i32;
    fn Thread32Next(snap: *mut std::ffi::c_void, entry: *mut std::ffi::c_void) -> i32;
    fn OpenThread(access: u32, inherit: i32, tid: u32) -> *mut std::ffi::c_void;
    fn SuspendThread(thread: *mut std::ffi::c_void) -> u32;
    fn ResumeThread(thread: *mut std::ffi::c_void) -> u32;
    fn GetCurrentThreadId() -> u32;
}

#[cfg(windows)]
#[link(name = "iphlpapi")]
extern "system" {
    fn GetAdaptersAddresses(
        family: u32,
        flags: u32,
        reserved: *mut std::ffi::c_void,
        addresses: *mut std::ffi::c_void,
        size: *mut u32,
    ) -> u32;
}

#[cfg(windows)]
#[link(name = "advapi32")]
extern "system" {
    fn LookupPrivilegeValueW(
        system: *const u16,
        name: *const u16,
        luid: *mut std::ffi::c_void,
    ) -> i32;
    fn AdjustTokenPrivileges(
        token: *mut std::ffi::c_void,
        disable_all: i32,
        new_state: *mut std::ffi::c_void,
        len: u32,
        prev: *mut std::ffi::c_void,
        ret_len: *mut u32,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "ntdll")]
extern "system" {
    fn NtQueryInformationProcess(
        process: *mut std::ffi::c_void,
        class: u32,
        info: *mut std::ffi::c_void,
        len: u32,
        ret_len: *mut u32,
    ) -> i32;
}

fn wants_store_launch(game_path: &str) -> bool {
    let trimmed = game_path.trim();
    if trimmed.is_empty() || is_store_game_path(trimmed) {
        return true;
    }
    let name = Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name == "infamousheroesentry.exe" || name == "blitzbrigade.exe"
}

fn launch_game_exe(app: &AppHandle, game_path: &str, lobby: &LobbyConfig) -> Result<(), String> {
    let path = Path::new(game_path);
    // Copie locale ou chemin vide : le paquet Store, via shell:AppsFolder.
    if wants_store_launch(game_path) {
        return launch_store_app(app, lobby);
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

fn find_store_game_exe() -> Option<PathBuf> {
    let script = format!(
        r#"$pkg = Get-AppxPackage -Name '{STORE_PACKAGE_NAME}' | Select-Object -First 1
if (-not $pkg) {{
  $pkg = Get-AppxPackage | Where-Object {{ $_.Name -match 'BlitzBrigade|Blitz.Brigade' }} | Select-Object -First 1
}}
if (-not $pkg) {{ exit 0 }}
$root = $pkg.InstallLocation
$preferred = Join-Path $root 'InfamousHeroesEntry.exe'
if (Test-Path -LiteralPath $preferred) {{ Write-Output $preferred; exit 0 }}
Get-ChildItem -LiteralPath $root -Filter '*.exe' -Recurse -ErrorAction SilentlyContinue |
  Select-Object -First 1 -ExpandProperty FullName"#
    );
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = PathBuf::from(trimmed);
        // WindowsApps refuse parfois is_file() alors que le chemin est le bon.
        if path.is_file() || trimmed.to_ascii_lowercase().ends_with(".exe") {
            return Some(path);
        }
    }
    None
}

fn collect_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    let pf86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into());
    let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let user = std::env::var("USERPROFILE").unwrap_or_default();

    for base in [&pf, &pf86] {
        push_unique(&mut roots, PathBuf::from(base).join("Ubisoft"));
        push_unique(
            &mut roots,
            PathBuf::from(base).join("Ubisoft Game Launcher\\games"),
        );
        push_unique(&mut roots, PathBuf::from(base).join("Steam\\steamapps\\common"));
        push_unique(
            &mut roots,
            PathBuf::from(base).join("Epic Games\\BlitzBrigade"),
        );
    }

    // Dossiers Store connus (versions variables)
    let windows_apps = PathBuf::from(&pf).join("WindowsApps");
    if windows_apps.is_dir() {
        if let Ok(entries) = fs::read_dir(&windows_apps) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name
                    .to_ascii_lowercase()
                    .starts_with("gameloftsa.blitzbrigade_")
                {
                    push_unique(&mut roots, entry.path());
                }
            }
        }
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
                if is_game_exe_name(name) {
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
fn find_game(app: AppHandle, preferred: Option<String>) -> Option<String> {
    if let Some(ref p) = preferred {
        let path = Path::new(p);
        if path.is_file() {
            append_log(&app, &format!("jeu (chemin connu): {}", path.display()));
            return Some(p.clone());
        }
    }

    if let Some(store) = find_store_game_exe() {
        append_log(&app, &format!("jeu Store détecté: {}", store.display()));
        return Some(store.display().to_string());
    }

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

const STORE_PRODUCT_ID: &str = "9WZDNCRFJ265";

fn game_folder_from_path(path: &str) -> Option<PathBuf> {
    let trimmed = path.trim();
    let file = PathBuf::from(trimmed);
    if file.is_dir() {
        return Some(file);
    }
    // Parent si .exe (évite is_file() : ACL WindowsApps parfois trompeur)
    if trimmed.to_ascii_lowercase().ends_with(".exe") {
        return file.parent().map(Path::to_path_buf);
    }
    if file.is_file() {
        return file.parent().map(Path::to_path_buf);
    }
    file.parent().map(Path::to_path_buf).or(Some(file))
}

#[tauri::command]
fn reveal_game_path(app: AppHandle, path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Aucun chemin jeu".to_string());
    }
    let dir = game_folder_from_path(trimmed)
        .ok_or_else(|| "Dossier introuvable".to_string())?;
    let dir_str = dir.to_string_lossy().to_string();
    append_log(&app, &format!("ouverture dossier: {dir_str}"));

    // Un seul argument = chemins avec espaces OK (ex. Program Files\WindowsApps\…)
    Command::new("explorer")
        .arg(&dir_str)
        .spawn()
        .map_err(|e| format!("ouverture Explorer: {e}"))?;
    Ok(())
}

#[tauri::command]
fn open_game_store(app: AppHandle) -> Result<(), String> {
    let uri = format!("ms-windows-store://pdp/?ProductId={STORE_PRODUCT_ID}");
    append_log(&app, &format!("ouverture Store: {uri}"));
    let status = Command::new("cmd")
        .args(["/C", "start", "", &uri])
        .status()
        .map_err(|e| format!("ouverture Store: {e}"))?;
    if status.success() {
        return Ok(());
    }
    let web = format!("https://apps.microsoft.com/detail/{STORE_PRODUCT_ID}");
    append_log(&app, &format!("repli web Store: {web}"));
    Command::new("cmd")
        .args(["/C", "start", "", &web])
        .status()
        .map_err(|e| format!("ouverture Store web: {e}"))?;
    Ok(())
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
    let running = {
        let alive = stub_is_running(&state);
        if !alive {
            if let Ok(mut g) = state.child.lock() {
                *g = None;
            }
        }
        alive
    };

    let join_debug = join_inject_debug_live().ok().and_then(|s| {
        if s.contains("non lancé") || s.contains("non hooké") {
            None
        } else {
            Some(s)
        }
    });
    if let Some(ref d) = join_debug {
        if d.contains("mode=1") {
            append_log(&app, &format!("join_inject: {d}"));
        }
    }
    match stub_dir(&app) {
        Ok(dir) => StubStatus {
            running,
            stub_ready: true,
            stub_path: Some(dir.display().to_string()),
            join_debug,
        },
        Err(_) => StubStatus {
            running,
            stub_ready: false,
            stub_path: None,
            join_debug,
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
fn start_party(
    app: AppHandle,
    state: State<'_, StubState>,
    port: u16,
    lobby_name: String,
    level_id: Option<u8>,
    map_id: Option<u8>,
    max_players: Option<u8>,
    game_mode: Option<u8>,
) -> Result<String, String> {
    let map_id = map_id.unwrap_or(0);
    let max_players = max_players.unwrap_or(8);
    let game_mode = game_mode.unwrap_or(0);
    let level_id = level_id.unwrap_or(game_mode.saturating_mul(6).saturating_add(map_id));
    spawn_stub(
        &app,
        &state,
        port,
        &lobby_name,
        level_id,
        map_id,
        max_players,
        game_mode,
    )?;
    let lobby = state
        .lobby
        .lock()
        .map_err(|_| "lock lobby".to_string())?
        .clone();
    match sync_lobby_inject_live(&lobby) {
        Ok(msg) => append_log(&app, &format!("sync inject après Créer: {msg}")),
        Err(e) => append_log(&app, &format!("sync inject après Créer échec: {e}")),
    }
    Ok(format!(
        "Partie « {lobby_name} » carte {map_id} mode {game_mode} ({max_players} slots)"
    ))
}

#[tauri::command]
fn sync_lobby_inject(app: AppHandle, state: State<'_, StubState>) -> Result<String, String> {
    let lobby = state
        .lobby
        .lock()
        .map_err(|_| "lock lobby".to_string())?
        .clone();
    let msg = sync_lobby_inject_live(&lobby)?;
    append_log(&app, &format!("sync_lobby_inject: {msg}"));
    Ok(msg)
}

/// Lit MODE/IDX/compteurs de la cave inject (diag Rejoindre).
#[tauri::command]
fn join_inject_debug(app: AppHandle) -> Result<String, String> {
    let msg = join_inject_debug_live()?;
    append_log(&app, &format!("join_inject_debug: {msg}"));
    Ok(msg)
}

#[cfg(windows)]
struct InjectCave {
    process: ProcessHandle,
    cave: u32,
    image: u32,
    pid: u32,
}

#[cfg(windows)]
fn resolve_inject_cave() -> Option<InjectCave> {
    let pid = find_game_pid()?;
    let process = unsafe { OpenProcess(0x0438, 0, pid) };
    if process.is_null() {
        return None;
    }
    let process = ProcessHandle(process);
    let mut peb32: u64 = 0;
    let mut returned = 0u32;
    let status = unsafe {
        NtQueryInformationProcess(
            process.0,
            26,
            &mut peb32 as *mut _ as *mut _,
            8,
            &mut returned,
        )
    };
    if status != 0 || peb32 == 0 {
        return None;
    }
    let mut image_buf = [0u8; 4];
    if read_bytes(process.0, peb32 + 0x08, &mut image_buf).is_err() {
        return None;
    }
    let image = u32::from_le_bytes(image_buf);
    let mut site = [0u8; 5];
    if read_bytes(
        process.0,
        image as u64 + DISCOVER_INJECT_SITE_RVA as u64,
        &mut site,
    )
    .is_err()
        || site[0] != 0xE9
    {
        return None;
    }
    let rel = i32::from_le_bytes([site[1], site[2], site[3], site[4]]);
    let cave =
        (image as i64 + DISCOVER_INJECT_SITE_RVA as i64 + 5 + rel as i64) as u32;
    Some(InjectCave {
        process,
        cave,
        image,
        pid,
    })
}

#[cfg(windows)]
fn read_inject_meta(process: *mut std::ffi::c_void, cave: u32) -> Option<(u8, u8, u8, u8)> {
    let mut meta = [0u8; 4];
    if read_bytes(process, cave as u64 + INJECT_MODE_OFF as u64, &mut meta).is_err() {
        return None;
    }
    Some((meta[0], meta[1], meta[2], meta[3])) // mode, idx, hits, join_ok
}

#[cfg(windows)]
fn join_inject_debug_live() -> Result<String, String> {
    let Some(cave) = resolve_inject_cave() else {
        return Ok("jeu/inject indisponible".into());
    };
    let Some((mode, idx, hits, join_ok)) = read_inject_meta(cave.process.0, cave.cave) else {
        return Ok(format!("lecture cave pid={} refusée", cave.pid));
    };
    let mut no_peer = [0u8; 1];
    let _ = read_bytes(
        cave.process.0,
        cave.cave as u64 + INJECT_JOIN_NO_PEER_OFF as u64,
        &mut no_peer,
    );
    Ok(format!(
        "pid={} mode={mode} idx={idx} hits={hits} join_ok={join_ok} no_peer={}",
        cave.pid,
        no_peer[0]
    ))
}

/// Watcher : après phase A + 0x11 stub, injecte 0x06→0x08→0x12 (+ keepalive).
/// Nécessaire car l'inbound UWP n'atteint jamais le jeu.
#[cfg(windows)]
fn spawn_phase_b_watcher(app: AppHandle) {
    std::thread::Builder::new()
        .name("bb-phase-b".into())
        .spawn(move || phase_b_watcher_loop(app))
        .ok();
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum PhaseBState {
    Idle,
    Armed,
    ClockQueued,
    ClockDrained,
    Started,
}

#[cfg(windows)]
fn phase_b_watcher_loop(app: AppHandle) {
    use std::collections::BTreeSet;
    use std::io::{Read, Seek, SeekFrom};

    let mut state = PhaseBState::Idle;
    let mut log_pos: u64 = 0;
    let mut saw_finished = false;
    let mut armed_at: Option<Instant> = None;
    let mut send_clock_time: Option<u32> = None;
    let mut clock_drain_at: Option<Instant> = None;
    let mut last_keepalive = Instant::now();
    let mut last_ack = Instant::now();
    let mut ping_seq: u32 = 1;
    let mut last_log = Instant::now();
    let mut ack_seen: BTreeSet<u32> = [0u32, 1].into_iter().collect();
    let mut ack_max: u32 = 1;
    let mut ack_dirty = false;
    let mut need_sync_reply = false;
    let mut sync_replied = false;
    let mut class_select: Option<(u8, u8, u8)> = None; // id, faction, class
    let mut class_replied = false;
    let mut pending_packed: u8 = 0x01; // (class<<4)|team — défaut soldier / équipe 1

    let reset_ack = |seen: &mut BTreeSet<u32>, max: &mut u32, dirty: &mut bool| {
        seen.clear();
        seen.insert(0);
        seen.insert(1);
        *max = 1;
        *dirty = false;
    };

    loop {
        sleep(Duration::from_millis(120));

        let Some(inj) = resolve_inject_cave() else {
            state = PhaseBState::Idle;
            saw_finished = false;
            armed_at = None;
            send_clock_time = None;
            clock_drain_at = None;
            need_sync_reply = false;
            sync_replied = false;
            class_select = None;
            class_replied = false;
            pending_packed = 0x01;
            reset_ack(&mut ack_seen, &mut ack_max, &mut ack_dirty);
            continue;
        };
        let Some((mode, idx, _hits, join_ok)) = read_inject_meta(inj.process.0, inj.cave) else {
            continue;
        };

        if mode != 1 {
            if state != PhaseBState::Idle {
                let lobby = app
                    .state::<StubState>()
                    .lobby
                    .lock()
                    .ok()
                    .map(|g| g.clone());
                if let Some(lobby) = lobby {
                    let _ = write_inject_packet_blobs(inj.process.0, inj.cave, &lobby, false);
                }
                if last_log.elapsed() > Duration::from_secs(2) {
                    append_log(&app, "phase_b: reset (MODE=0)");
                    last_log = Instant::now();
                }
            }
            state = PhaseBState::Idle;
            saw_finished = false;
            armed_at = None;
            send_clock_time = None;
            clock_drain_at = None;
            need_sync_reply = false;
            sync_replied = false;
            class_select = None;
            class_replied = false;
            pending_packed = 0x01;
            reset_ack(&mut ack_seen, &mut ack_max, &mut ack_dirty);
            continue;
        }

        // Tail du log stub.
        if let Some(path) = log_file(&app) {
            if let Ok(mut f) = OpenOptions::new().read(true).open(&path) {
                let _ = f.seek(SeekFrom::Start(log_pos));
                let mut buf = String::new();
                if f.read_to_string(&mut buf).is_ok() {
                    log_pos += buf.len() as u64;
                    for line in buf.lines() {
                        if line.contains("type=0x11") || line.contains("FinishedLoading") {
                            saw_finished = true;
                        }
                        if line.contains("SendClock") {
                            if let Some(idx_t) = line.find("time=") {
                                let rest = &line[idx_t + 5..];
                                let num: String =
                                    rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                                if let Ok(t) = num.parse::<u32>() {
                                    send_clock_time = Some(t);
                                }
                            } else {
                                send_clock_time = Some(send_clock_time.unwrap_or(0));
                            }
                        }
                        if let Some(seq) = parse_reliable_seq(line) {
                            ack_seen.insert(seq);
                            if seq > ack_max {
                                ack_max = seq;
                            }
                            ack_dirty = true;
                        }
                        if line.contains("type=0x14") || line.contains("SyncRequest") {
                            need_sync_reply = true;
                        }
                        if line.contains("ClassSelect") {
                            let dig = |key: &str| -> Option<u8> {
                                let i = line.find(key)? + key.len();
                                let s: String = line[i..]
                                    .chars()
                                    .take_while(|c| c.is_ascii_digit())
                                    .collect();
                                s.parse().ok()
                            };
                            let id = dig("id=").unwrap_or(0);
                            let faction = dig("faction=").unwrap_or(1).max(1);
                            let class_id = dig("class=").unwrap_or(0);
                            let next = (id, faction, class_id);
                            if class_select != Some(next) {
                                class_replied = false;
                            }
                            class_select = Some(next);
                            pending_packed = (class_id << 4) | (faction & 0x0F);
                        }
                    }
                }
            }
        }

        match state {
            PhaseBState::Idle => {
                if join_ok >= INJECT_JOIN_COUNT && idx >= INJECT_JOIN_COUNT {
                    state = PhaseBState::Armed;
                    saw_finished = false;
                    armed_at = Some(Instant::now());
                    send_clock_time = None;
                    clock_drain_at = None;
                    need_sync_reply = false;
                    sync_replied = false;
                    class_select = None;
                    class_replied = false;
                    pending_packed = 0x01;
                    reset_ack(&mut ack_seen, &mut ack_max, &mut ack_dirty);
                    if let Some(path) = log_file(&app) {
                        if let Ok(meta) = fs::metadata(&path) {
                            log_pos = meta.len();
                        }
                    }
                    append_log(&app, "phase_b: armed (phase A drained)");
                }
            }
            PhaseBState::Armed => {
                let timed_out = armed_at
                    .map(|t| t.elapsed() >= Duration::from_secs(2))
                    .unwrap_or(false);
                if !saw_finished && !timed_out {
                    continue;
                }
                if !saw_finished && timed_out {
                    append_log(&app, "phase_b: Armed timeout → force 0x06 (0x11 déjà passé?)");
                    saw_finished = true;
                }
                match inject_push_batch(inj.process.0, inj.cave, &build_phase_b_clock_batch()) {
                    Ok(true) => {
                        state = PhaseBState::ClockQueued;
                        clock_drain_at = None;
                        append_log(&app, "phase_b: inject 0x06 ReqClock");
                    }
                    Ok(false) => {}
                    Err(e) => append_log(&app, &format!("phase_b clock err: {e}")),
                }
            }
            PhaseBState::ClockQueued => {
                if idx >= INJECT_JOIN_COUNT {
                    state = PhaseBState::ClockDrained;
                    clock_drain_at = Some(Instant::now());
                    append_log(&app, "phase_b: 0x06 drained, await 0x07/fallback");
                }
            }
            PhaseBState::ClockDrained => {
                let ready = send_clock_time.is_some()
                    || clock_drain_at
                        .map(|t| t.elapsed() >= Duration::from_millis(800))
                        .unwrap_or(false);
                if !ready {
                    continue;
                }
                let delay = send_clock_time.unwrap_or(0);
                match inject_push_batch(
                    inj.process.0,
                    inj.cave,
                    &build_phase_b_start_batch(delay),
                ) {
                    Ok(true) => {
                        state = PhaseBState::Started;
                        last_keepalive = Instant::now();
                        last_ack = Instant::now();
                        append_log(
                            &app,
                            &format!("phase_b: inject 0x08/0x12 delay={delay}"),
                        );
                    }
                    Ok(false) => {}
                    Err(e) => append_log(&app, &format!("phase_b start err: {e}")),
                }
            }
            PhaseBState::Started => {
                // Ne pas forcer settings/schéma ici (casse les clics lobby/menu).
                if idx < INJECT_JOIN_COUNT {
                    continue;
                }
                let (pb, mask) = ack_fields(&ack_seen, ack_max);

                // 1) SyncRequest 0x14 → 0x15 (table joueurs) + état 2 + 0x6d
                // Si ClassSelect arrive sans 0x14 vu, forcer aussi le sync.
                if (need_sync_reply || class_select.is_some()) && !sync_replied {
                    let sync = build_inject_sync_snapshot(pb, mask, pending_packed);
                    let ack = build_inject_ack_only(pb, mask);
                    let batch = [sync, ack.clone(), ack];
                    match inject_push_batch(inj.process.0, inj.cave, &batch) {
                        Ok(true) => {
                            sync_replied = true;
                            ack_dirty = false;
                            last_ack = Instant::now();
                            append_log(
                                &app,
                                &format!(
                                    "phase_b: sync 0x15 + state2 + 0x6d packed=0x{pending_packed:02x}"
                                ),
                            );
                        }
                        Ok(false) => {}
                        Err(e) => append_log(&app, &format!("phase_b sync err: {e}")),
                    }
                    continue;
                }

                // 2) ClassSelect 0x61 → 0x62+0x25 (après sync ; table remplie par 0x15)
                if sync_replied {
                    if let Some((id, faction, class_id)) = class_select {
                        if !class_replied {
                            let loadout = read_profile_loadout(inj.process.0, inj.image);
                            let cls = build_inject_class_batch(
                                id, faction, class_id, &loadout, pb, mask,
                            );
                            let ack = build_inject_ack_only(pb, mask);
                            let batch = [cls, ack.clone(), ack];
                            match inject_push_batch(inj.process.0, inj.cave, &batch) {
                                Ok(true) => {
                                    class_replied = true;
                                    ack_dirty = false;
                                    last_ack = Instant::now();
                                    append_log(
                                        &app,
                                        &format!(
                                            "phase_b: class confirm id={id} faction={faction} class={class_id} loadout={}",
                                            loadout.len()
                                        ),
                                    );
                                }
                                Ok(false) => {}
                                Err(e) => append_log(&app, &format!("phase_b class err: {e}")),
                            }
                            continue;
                        }
                    }
                }

                // 3) ACK dynamique
                if ack_dirty && last_ack.elapsed() >= Duration::from_millis(250) {
                    let a = build_inject_ack_only(pb, mask);
                    match inject_push_batch(inj.process.0, inj.cave, &[a.clone(), a.clone(), a]) {
                        Ok(true) => {
                            ack_dirty = false;
                            last_ack = Instant::now();
                            append_log(
                                &app,
                                &format!("phase_b: ack peerB={pb} mask=0x{mask:08x}"),
                            );
                        }
                        Ok(false) => {}
                        Err(e) => append_log(&app, &format!("phase_b ack err: {e}")),
                    }
                    continue;
                }

                // 4) Keepalive (1s : en jeu select>0, le stub force-injecte la file)
                if last_keepalive.elapsed() < Duration::from_millis(1000) {
                    continue;
                }
                match inject_push_batch(
                    inj.process.0,
                    inj.cave,
                    &build_phase_b_keepalive_batch(ping_seq, pb, mask),
                ) {
                    Ok(true) => {
                        ping_seq = ping_seq.wrapping_add(1);
                        last_keepalive = Instant::now();
                        if ping_seq <= 3 || ping_seq % 8 == 0 {
                            append_log(&app, &format!("phase_b: keepalive seq={ping_seq}"));
                        }
                    }
                    Ok(false) => {}
                    Err(e) => append_log(&app, &format!("phase_b keepalive err: {e}")),
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn spawn_phase_b_watcher(_app: AppHandle) {}

#[cfg(not(windows))]
fn join_inject_debug_live() -> Result<String, String> {
    Ok("debug inject Windows uniquement".into())
}

#[tauri::command]
fn launch_game(
    app: AppHandle,
    state: State<'_, StubState>,
    game_path: String,
    lobby_name: Option<String>,
    level_id: Option<u8>,
    map_id: Option<u8>,
    max_players: Option<u8>,
    game_mode: Option<u8>,
) -> Result<String, String> {
    let lobby = {
        let mut guard = state.lobby.lock().map_err(|_| "lock lobby".to_string())?;
        if lobby_name.is_some()
            || level_id.is_some()
            || map_id.is_some()
            || game_mode.is_some()
            || max_players.is_some()
        {
            let next = LobbyConfig {
                host_name: normalize_lobby_name(
                    lobby_name.as_deref().unwrap_or(&guard.host_name),
                ),
                level_id: level_id.unwrap_or(guard.level_id),
                map_id: map_id.unwrap_or(guard.map_id),
                max_players: {
                    let m = max_players.unwrap_or(guard.max_players);
                    if m == 0 {
                        8
                    } else {
                        m
                    }
                },
                game_mode: game_mode.unwrap_or(guard.game_mode),
            };
            *guard = next;
        }
        guard.clone()
    };
    append_log(
        &app,
        &format!(
            "launch_game lobby={} level={} map={} mode={} max={}",
            lobby.host_name, lobby.level_id, lobby.map_id, lobby.game_mode, lobby.max_players
        ),
    );
    launch_game_exe(&app, &game_path, &lobby)?;
    Ok("Jeu lancé".to_string())
}

#[tauri::command]
fn launch_all(
    app: AppHandle,
    state: State<'_, StubState>,
    port: u16,
    lobby_name: String,
    game_path: String,
    level_id: Option<u8>,
    map_id: Option<u8>,
    max_players: Option<u8>,
    game_mode: Option<u8>,
) -> Result<String, String> {
    let map_id = map_id.unwrap_or(0);
    let max_players = max_players.unwrap_or(8);
    let game_mode = game_mode.unwrap_or(0);
    let level_id = level_id.unwrap_or(game_mode.saturating_mul(6).saturating_add(map_id));
    let lobby = {
        let mut guard = state.child.lock().map_err(|_| "lock stub".to_string())?;
        if let Some(child) = guard.as_mut() {
            if is_child_running(child) {
                drop(guard);
                state
                    .lobby
                    .lock()
                    .map_err(|_| "lock lobby".to_string())?
                    .clone()
            } else {
                *guard = None;
                drop(guard);
                spawn_stub(
                    &app,
                    &state,
                    port,
                    &lobby_name,
                    level_id,
                    map_id,
                    max_players,
                    game_mode,
                )?;
                state
                    .lobby
                    .lock()
                    .map_err(|_| "lock lobby".to_string())?
                    .clone()
            }
        } else {
            drop(guard);
            spawn_stub(
                &app,
                &state,
                port,
                &lobby_name,
                level_id,
                map_id,
                max_players,
                game_mode,
            )?;
            state
                .lobby
                .lock()
                .map_err(|_| "lock lobby".to_string())?
                .clone()
        }
    };

    match launch_game_exe(&app, &game_path, &lobby) {
        Ok(()) => Ok(format!("En ligne — lobby {} :{port}", lobby.host_name)),
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
            lobby: Mutex::new(LobbyConfig::default()),
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            stop_stub,
            start_party,
            sync_lobby_inject,
            join_inject_debug,
            launch_game,
            launch_all,
            find_game,
            reveal_game_path,
            open_game_store,
            read_recent_logs
        ])
        .setup(|app| {
            append_log(app.handle(), "launcher démarré");
            let _ = stub_dir(app.handle());
            spawn_phase_b_watcher(app.handle().clone());
            #[cfg(windows)]
            spawn_fps_mouse_watcher(app.handle().clone());
            // BB_EMU_AUTOLAUNCH=1 : stub + Store (tests agent).
            if std::env::var("BB_EMU_AUTOLAUNCH").ok().as_deref() == Some("1") {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    sleep(Duration::from_millis(2500));
                    let lobby = LobbyConfig::default();
                    let state = handle.state::<StubState>();
                    if let Err(e) = spawn_stub(
                        &handle,
                        &state,
                        7891,
                        &lobby.host_name,
                        lobby.level_id,
                        lobby.map_id,
                        lobby.max_players,
                        lobby.game_mode,
                    ) {
                        append_log(&handle, &format!("autolaunch stub échec: {e}"));
                        return;
                    }
                    match launch_store_app(&handle, &lobby) {
                        Ok(()) => append_log(&handle, "autolaunch Store OK"),
                        Err(e) => append_log(&handle, &format!("autolaunch échec: {e}")),
                    }
                });
            }
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

#[cfg(all(test, windows))]
mod launch_once {
    #[test]
    #[ignore]
    fn launch_store_without_match() {
        let lobby = super::LobbyConfig::default();
        let detail = super::launch_store_process(&lobby).expect("lancement Store");
        println!("{detail}");
    }
}

#[cfg(all(test, windows))]
mod inject_stub {
    #[test]
    fn select_inject_stub_builds() {
        let code = super::build_select_inject_stub(0x2000_0000, 0x0040_0000, [192, 168, 1, 10])
            .expect("stub");
        assert!(
            code.len() <= super::INJECT_ENTER_STUB_OFF as usize,
            "cave trop longue {}",
            code.len()
        );
        assert!(code.len() > 64);
    }

    #[test]
    fn sync_snapshot_fits_slot() {
        let pkt = super::build_inject_sync_snapshot(1, 0b1111, 0x01);
        assert!(
            pkt.len() <= super::INJECT_SLOT_SIZE as usize,
            "sync snapshot trop grand {}",
            pkt.len()
        );
        assert!(pkt.len() > 300);
        let cave_end = super::INJECT_JOIN_PKT_BASE + 3 * super::INJECT_SLOT_SIZE;
        assert!(cave_end <= 0x1000);
    }
}

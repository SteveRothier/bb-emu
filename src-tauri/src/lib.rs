use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

const STORE_PACKAGE_NAME: &str = "GAMELOFTSA.BlitzBrigade";
const STORE_APP_ID: &str = "GAMELOFTSA.BlitzBrigade_0pp20fcewvvtj!App";

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

fn launch_store_process() -> Result<String, String> {
    let target = format!("shell:AppsFolder\\{STORE_APP_ID}");
    Command::new("explorer.exe")
        .arg(&target)
        .spawn()
        .map_err(|e| format!("Échec lancement Store: {e}"))?;
    apply_sizechanged_jump()
}

fn launch_store_app(app: &AppHandle) -> Result<(), String> {
    let target = format!("shell:AppsFolder\\{STORE_APP_ID}");
    append_log(app, &format!("lancement Store: {target}"));
    let detail = launch_store_process()?;
    append_log(app, &format!("saut SizeChanged écrit: {detail}"));
    Ok(())
}

#[cfg(windows)]
fn apply_sizechanged_jump() -> Result<String, String> {
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
        match try_patch(pid, SITE_RVA, CAVE_RVA, PAGE_RVA, &EXPECT, &JUMP, &CAVE) {
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
fn apply_sizechanged_jump() -> Result<String, String> {
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
        let mut old = 0u32;
        if unsafe { VirtualProtectEx(process.0, page, 0x1000, 0x40, &mut old) } == 0 {
            return PatchTry::Fail(format!(
                "protection de la page refusée (code {})",
                last_error()
            ));
        }
        if write_exact(process.0, (image + site_rva) as u64, jump).is_err() {
            let err = last_error();
            unsafe { VirtualProtectEx(process.0, page, 0x1000, old, &mut old) };
            return PatchTry::Fail(format!("écriture du saut refusée (code {err})"));
        }
        if write_exact(process.0, (image + cave_rva) as u64, cave).is_err() {
            let err = last_error();
            unsafe { VirtualProtectEx(process.0, page, 0x1000, old, &mut old) };
            return PatchTry::Fail(format!("écriture de la cave refusée (code {err})"));
        }
        unsafe { VirtualProtectEx(process.0, page, 0x1000, old, &mut old) };
        if read_exact(process.0, (image + site_rva) as u64, &mut site).is_err() {
            return PatchTry::Fail(format!("relecture refusée (code {})", last_error()));
        }
        if &site != jump {
            return PatchTry::Fail(format!("relecture inattendue: {}", hex_bytes(&site)));
        }
    }
    PatchTry::Done(format!(
        "pid={pid} base=0x{image:X} site={}",
        hex_bytes(&site)
    ))
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
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
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
    fn VirtualProtectEx(
        process: *mut std::ffi::c_void,
        addr: u64,
        size: usize,
        prot: u32,
        old: *mut u32,
    ) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut std::ffi::c_void;
    fn Process32FirstW(snap: *mut std::ffi::c_void, entry: *mut std::ffi::c_void) -> i32;
    fn Process32NextW(snap: *mut std::ffi::c_void, entry: *mut std::ffi::c_void) -> i32;
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

fn launch_game_exe(app: &AppHandle, game_path: &str) -> Result<(), String> {
    let path = Path::new(game_path);
    // Copie locale ou chemin vide : le paquet Store, via shell:AppsFolder.
    if wants_store_launch(game_path) {
        return launch_store_app(app);
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
    let stub_up = {
        let mut guard = state.child.lock().map_err(|_| "lock stub".to_string())?;
        if let Some(child) = guard.as_mut() {
            if is_child_running(child) {
                true
            } else {
                *guard = None;
                false
            }
        } else {
            false
        }
    };
    if stub_up {
        return launch_game_exe(&app, &game_path)
            .map(|_| format!("En ligne — lobby {lobby_name} :{port}"));
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
            reveal_game_path,
            open_game_store,
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

#[cfg(all(test, windows))]
mod launch_once {
    #[test]
    #[ignore]
    fn launch_store_without_match() {
        let detail = super::launch_store_process().expect("lancement Store");
        println!("{detail}");
    }
}

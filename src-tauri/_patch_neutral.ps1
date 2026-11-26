$path = 'c:\Users\Steve\react\bb-emu\src-tauri\src\lib.rs'
$text = [IO.File]::ReadAllText($path)
$nl = if ($text.Contains("`r`n")) { "`r`n" } else { "`n" }

# Prefixe sans accent pour tolerer les variantes du commentaire.
$start = $text.IndexOf("                // Ancre haute :")
if ($start -lt 0) { throw 'start not found' }
$end = $text.IndexOf("                let dx = FPS_RAW_DX.swap(0, Ordering::Relaxed);", $start)
if ($end -lt 0) { throw 'end not found' }

$new = @'
                // Ancre neutre pad droit — DOWN+hold ici uniquement (pas de swipe/nudge).
                let anchor_x = (width as f32 * 0.72) as i32;
                let anchor_y = (height as f32 * 0.42) as i32;
                // Pad large : clamp soft sans UP/DOWN aux bords.
                let pad_x = ((width as f32) * 0.22).max(48.0) as i32;
                let pad_y = ((height as f32) * 0.22).max(48.0) as i32;

                let try_down_neutral = |backend: TouchBackend,
                                        syn: *mut std::ffi::c_void,
                                        hwnd: *mut std::ffi::c_void,
                                        cx: &mut i32,
                                        cy: &mut i32,
                                        hwnd_out: &mut *mut std::ffi::c_void,
                                        down: &mut bool|
                 -> bool {
                    let Some((sx, sy)) = client_to_screen_pt(hwnd, *cx, *cy) else {
                        return false;
                    };
                    if !touch_contact(backend, syn, hwnd, sx, sy, TOUCH_FLAG_DOWN) {
                        return false;
                    }
                    *down = true;
                    *hwnd_out = hwnd;
                    true
                };

                if !session_on {
                    if !inject_ok && !syn_ok {
                        if hit_log_at.elapsed() >= Duration::from_secs(2) {
                            hit_log_at = std::time::Instant::now();
                            append_log(&app, "fps-mouse: touch APIs KO (inject+synthetic)");
                        }
                        continue;
                    }
                    touch_cx = anchor_x;
                    touch_cy = anchor_y;
                    touch_acc_x = 0.0;
                    touch_acc_y = 0.0;
                    touch_hwnd = hwnd;
                    let mut down_ok = false;
                    let mut backend_name = "inject";
                    if inject_ok {
                        touch_backend = TouchBackend::Inject;
                        down_ok = try_down_neutral(
                            touch_backend,
                            syn_dev,
                            hwnd,
                            &mut touch_cx,
                            &mut touch_cy,
                            &mut touch_hwnd,
                            &mut touch_down,
                        );
                        backend_name = "inject";
                    }
                    if !down_ok && syn_ok {
                        let err_inject = last_error();
                        touch_backend = TouchBackend::Synthetic;
                        down_ok = try_down_neutral(
                            touch_backend,
                            syn_dev,
                            hwnd,
                            &mut touch_cx,
                            &mut touch_cy,
                            &mut touch_hwnd,
                            &mut touch_down,
                        );
                        backend_name = "synthetic";
                        if !down_ok && hit_log_at.elapsed() >= Duration::from_secs(2) {
                            hit_log_at = std::time::Instant::now();
                            append_log(
                                &app,
                                &format!(
                                    "fps-mouse: TOUCH_DOWN KO anchor=({anchor_x},{anchor_y}) inject_err={err_inject} syn_err={}",
                                    last_error()
                                ),
                            );
                        }
                    } else if !down_ok {
                        if hit_log_at.elapsed() >= Duration::from_secs(2) {
                            hit_log_at = std::time::Instant::now();
                            append_log(
                                &app,
                                &format!(
                                    "fps-mouse: TOUCH_DOWN KO anchor=({anchor_x},{anchor_y}) backend=inject err={}",
                                    last_error()
                                ),
                            );
                        }
                    }
                    if !down_ok {
                        continue;
                    }
                    look_ox = anchor_x;
                    look_oy = anchor_y;
                    session_on = true;
                    let _ = FPS_RAW_DX.swap(0, Ordering::Relaxed);
                    let _ = FPS_RAW_DY.swap(0, Ordering::Relaxed);
                    append_log(
                        &app,
                        &format!(
                            "fps-mouse: freelook ON touch backend={backend_name} scale={TOUCH_LOOK_SCALE} maxStep={TOUCH_LOOK_MAX_STEP} anchor=({anchor_x},{anchor_y}) hold=({look_ox},{look_oy}) client={width}x{height}"
                        ),
                    );
                }

'@
if ($nl -eq "`r`n") {
  $new = $new -replace "(?<!`r)`n", "`r`n"
}
$out = $text.Substring(0, $start) + $new + $text.Substring($end)
[IO.File]::WriteAllText($path, $out)
Write-Host "OK replaced $($end-$start) -> $($new.Length)"

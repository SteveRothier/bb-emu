$path = 'c:\Users\Steve\react\bb-emu\src-tauri\src\lib.rs'
$text = [IO.File]::ReadAllText($path)
$nl = if ($text.Contains("`r`n")) { "`r`n" } else { "`n" }

$symbols = @(
  'fn force_packer_pitch_horizon',
  'fn arm_abs_look_no_center',
  'fn resolve_abs_look_ptr',
  'fn write_abs_look_i16',
  'fn invalidate_abs_look_cache',
  'fn sync_abs_look_cache',
  'fn freelook_view_fwd',
  'fn write_share_view_fwd',
  'fn install_apply_look_cave',
  'fn widen_aim_look_limits',
  'fn sync_aim_stomp_to_ctl',
  'fn clamp_ctl_pitch_band',
  'fn write_aim_lookat_gate',
  'fn read_aim_i32',
  'fn write_aim_pitch_hold',
  'fn read_aim_c0',
  'fn read_cam_forward',
  'fn nop_stomp_apply_call',
  'fn write_horizon_share',
  'fn install_horizon_hold_hook',
  'fn install_camview_stack_dir_hook',
  'fn skip_call_ret4',
  'fn DestroySyntheticPointerDevice',
  'const APPLY_YAW_RVA',
  'const APPLY_PITCH_RVA',
  'const CAM_APPLY_RESUME_RVA',
  'const LOOK_SHARE_DX',
  'const LOOK_SHARE_DY',
  'const LOOK_SHARE_CODE',
  'const LOOK_APPLY_SCALE',
  'const PITCH_BAND_MIN',
  'const PITCH_BAND_MAX',
  'const HORIZON_VIEW_FWD',
  'const CAM_LOOK_SENS',
  'const CAM_PITCH_LIMIT',
  'const MGR_CTL_ANTI_ID',
  'const CAM_CTL_PITCH_HOLD',
  'const CAMVIEW_STACK_DIR_RESUME_RVA',
  'const CAMVIEW_STACK_DIR_SKIP_RVA',
  'const CAM_SHARE_VIEW_YAW'
)

$count = 0
foreach ($sym in $symbols) {
  # Search for line containing the symbol definition
  $idx = 0
  while (($idx = $text.IndexOf($sym, $idx)) -ge 0) {
    # Walk back to start of line
    $lineStart = $text.LastIndexOf($nl, $idx) + $nl.Length
    if ($lineStart -lt $nl.Length) { $lineStart = 0 }
    # Check previous attribute lines
    $before = $text.Substring(0, $lineStart)
    $prevChunk = $before.TrimEnd()
    if ($prevChunk.EndsWith('#[allow(dead_code)]') -or $prevChunk.EndsWith('#[allow(dead_code)]' + $nl) -or ($prevChunk -match '\#\[allow\(dead_code\)\]\s*$')) {
      $idx = $idx + $sym.Length
      continue
    }
    # Also skip if within a few lines above there's allow
    $checkFrom = [Math]::Max(0, $lineStart - 200)
    $nearby = $text.Substring($checkFrom, $lineStart - $checkFrom)
    if ($nearby -match '(?s)\#\[allow\(dead_code\)\]\s*(\r?\n\s*///[^\r\n]*)*\s*$') {
      $idx = $idx + $sym.Length
      continue
    }
    # Insert allow before doc comments if present
    $insertAt = $lineStart
    $scan = $lineStart
    while ($true) {
      $prevNl = $text.LastIndexOf($nl, $scan - 1)
      if ($prevNl -lt 0) { break }
      $prevLineStart = if ($prevNl -ge 0) { $prevNl + $nl.Length } else { 0 }
      $prevLine = $text.Substring($prevLineStart, $scan - $prevLineStart)
      if ($prevLine.TrimStart().StartsWith('///') -or $prevLine.TrimStart().StartsWith('#[cfg') -or $prevLine.TrimStart().StartsWith('#[allow')) {
        if ($prevLine.TrimStart().StartsWith('#[allow(dead_code)]')) {
          $insertAt = -1
          break
        }
        $insertAt = $prevLineStart
        $scan = $prevLineStart
        if ($prevLineStart -eq 0) { break }
        continue
      }
      break
    }
    if ($insertAt -ge 0) {
      # Get indent from definition line
      $defLine = $text.Substring($lineStart, [Math]::Min(80, $text.Length - $lineStart))
      $indent = ''
      if ($defLine -match '^(\s*)') { $indent = $Matches[1] }
      $attr = "${indent}#[allow(dead_code)]$nl"
      $text = $text.Insert($insertAt, $attr)
      $count++
      $idx = $idx + $sym.Length + $attr.Length
    } else {
      $idx = $idx + $sym.Length
    }
  }
}

[IO.File]::WriteAllText($path, $text)
Write-Host "Added $count allow(dead_code) attributes"

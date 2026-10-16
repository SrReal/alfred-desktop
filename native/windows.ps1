param()
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$request = [Console]::In.ReadToEnd() | ConvertFrom-Json
$op = $request.operation
$p = $request.input
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class AlfredPetNative {
 [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left,Top,Right,Bottom; }
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h,System.Text.StringBuilder b,int n);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out RECT r);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h,IntPtr dc,uint flags);
 [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h,IntPtr after,int x,int y,int w,int ht,uint flags);
 [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h,int cmd);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
 [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
 [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h,uint flags);
 [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int key);
 public delegate bool WindowEnumProc(IntPtr h,IntPtr data);
 [DllImport("user32.dll")] public static extern bool EnumWindows(WindowEnumProc callback,IntPtr data);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
 [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr h);
 [DllImport("user32.dll",EntryPoint="GetWindowLongW")] public static extern int GetWindowLong(IntPtr h,int index);
 [DllImport("user32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern bool SetProp(IntPtr h,string name,IntPtr value);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern IntPtr GetProp(IntPtr h,string name);
 [StructLayout(LayoutKind.Sequential)] public struct POINTPLACEMENT { public int X,Y; }
 [StructLayout(LayoutKind.Sequential)] public struct WINDOWPLACEMENT { public uint length,flags,showCmd; public POINTPLACEMENT minimum,maximum; public RECT normal; }
 [DllImport("user32.dll",SetLastError=true)] public static extern bool GetWindowPlacement(IntPtr h,ref WINDOWPLACEMENT placement);
 [DllImport("user32.dll",SetLastError=true)] public static extern bool SetWindowPlacement(IntPtr h,ref WINDOWPLACEMENT placement);
 public static long[] VisibleWindows() { var list=new System.Collections.Generic.List<long>(); EnumWindows((h,d)=>{if(IsWindowVisible(h))list.Add(h.ToInt64());return true;},IntPtr.Zero);return list.ToArray(); }
 [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
 [DllImport("shcore.dll")] public static extern int GetDpiForMonitor(IntPtr h,int type,out uint x,out uint y);
 [DllImport("user32.dll")] public static extern IntPtr MonitorFromPoint(POINT p,uint flags);
 [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X,Y; }
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
 [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public INPUTUNION data; }
 [StructLayout(LayoutKind.Explicit)] public struct INPUTUNION { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
 [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx,dy; public uint mouseData,dwFlags,time; public UIntPtr extra; }
 [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort vk,scan; public uint flags,time; public UIntPtr extra; }
 [DllImport("user32.dll",SetLastError=true)] public static extern uint SendInput(uint n,INPUT[] inputs,int size);
 [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)] public struct CREDENTIAL { public uint Flags,Type; public string TargetName,Comment; public long LastWritten; public uint CredentialBlobSize; public IntPtr CredentialBlob; public uint Persist,AttributeCount; public IntPtr Attributes; public string TargetAlias,UserName; }
 [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern bool CredWrite(ref CREDENTIAL c,uint flags);
 [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] public static extern bool CredRead(string target,uint type,uint flags,out IntPtr c);
 [DllImport("advapi32.dll")] public static extern void CredFree(IntPtr c);
}
'@
[void][AlfredPetNative]::SetThreadDpiAwarenessContext([IntPtr](-4))
function WindowMetadata([IntPtr]$handle) {
 if(-not [AlfredPetNative]::IsWindow($handle)){throw 'WINDOW_TARGET_MISSING'}
 $b = [Text.StringBuilder]::new(1024); [void][AlfredPetNative]::GetWindowText($handle,$b,1024)
 [uint32]$processIdValue=0; [void][AlfredPetNative]::GetWindowThreadProcessId($handle,[ref]$processIdValue)
 $r = [AlfredPetNative+RECT]::new(); [void][AlfredPetNative]::GetWindowRect($handle,[ref]$r)
 @{hwnd=$handle.ToInt64();title=$b.ToString();process_id=$processIdValue;x=$r.Left;y=$r.Top;width=($r.Right-$r.Left);height=($r.Bottom-$r.Top);scale=([AlfredPetNative]::GetDpiForWindow($handle)/96.0)}
}
function ProcessCreated([uint32]$ownerId) {
 $target=$null
 # Query this PID directly instead of invoking a process cmdlet for every
 # snapshot. Open a fresh Process for both identity checks, so a terminated or
 # reused PID is still detected.
 try{$target=[Diagnostics.Process]::GetProcessById([int]$ownerId); $target.StartTime.ToUniversalTime().Ticks.ToString([Globalization.CultureInfo]::InvariantCulture)}
 catch{throw 'LAYOUT_PROCESS_IDENTITY_UNAVAILABLE'}
 finally{if($target){$target.Dispose()}}
}
function LayoutTarget($placement) {
 $meta=WindowMetadata ([IntPtr]([long]$placement.hwnd))
 if(-not $placement.process_id -or [long]$placement.process_id -ne [long]$meta.process_id){throw 'WINDOW_TARGET_CHANGED'}
 if($placement.process_started){
  if([string]$placement.process_started -notmatch '^[1-9][0-9]{0,18}$' -or (ProcessCreated ([uint32]$meta.process_id)) -ne [string]$placement.process_started){throw 'LAYOUT_PROCESS_RECREATED'}
  $handle=[IntPtr]([long]$placement.hwnd)
  if(-not [AlfredPetNative]::IsWindowVisible($handle) -or [AlfredPetNative]::IsIconic($handle) -or [AlfredPetNative]::IsZoomed($handle)){throw 'LAYOUT_WINDOW_NOT_NORMAL'}
 }
 if($placement.window_identity -and (WindowIdentity ([IntPtr]([long]$placement.hwnd)) $false) -ne [string]$placement.window_identity){throw 'WINDOW_IDENTITY_CHANGED'}
 $meta
}
function WindowIdentity([IntPtr]$handle,[bool]$initialize) {
 $name='AlfredPet.WindowIdentity.v1';$value=[AlfredPetNative]::GetProp($handle,$name)
 if($value -eq [IntPtr]::Zero -and $initialize){
  $bytes=[Guid]::NewGuid().ToByteArray();$number=[BitConverter]::ToInt64($bytes,0) -band [long]::MaxValue
  if($number -eq 0){throw 'WINDOW_IDENTITY_RANDOM'}
  if(-not [AlfredPetNative]::SetProp($handle,$name,[IntPtr]$number)){throw ('WINDOW_IDENTITY_DENIED: os='+[Runtime.InteropServices.Marshal]::GetLastWin32Error())}
  $value=[AlfredPetNative]::GetProp($handle,$name)
  if($value.ToInt64() -ne $number){throw 'WINDOW_IDENTITY_CHANGED'}
 }
 if($value -eq [IntPtr]::Zero){return $null}
 $value.ToInt64().ToString('x16',[Globalization.CultureInfo]::InvariantCulture)
}
function RecoverySnapshot($target,[bool]$initialize=$false) {
 $handle=[IntPtr]([long]$target.hwnd);$meta=WindowMetadata $handle
 if(-not [AlfredPetNative]::IsWindowVisible($handle)){throw 'WINDOW_NOT_VISIBLE'}
 if($target.process_id -and [long]$target.process_id -ne [long]$meta.process_id){throw 'WINDOW_TARGET_CHANGED'}
 $started=ProcessCreated ([uint32]$meta.process_id)
 if($target.process_started -and [string]$target.process_started -ne $started){throw 'LAYOUT_PROCESS_RECREATED'}
 $identity=WindowIdentity $handle $initialize
 if($target.window_identity -and [string]$target.window_identity -ne $identity){throw 'WINDOW_IDENTITY_CHANGED'}
 $placement=[AlfredPetNative+WINDOWPLACEMENT]::new();$placement.length=[Runtime.InteropServices.Marshal]::SizeOf($placement)
 if(-not [AlfredPetNative]::GetWindowPlacement($handle,[ref]$placement)){throw 'WINDOW_PLACEMENT_READ'}
 # Inactive min/max positions are OS bookkeeping, not the current placement.
 $minX=-1;$minY=-1;$maxX=-1;$maxY=-1
 if($placement.showCmd -eq 2){$minX=$placement.minimum.X;$minY=$placement.minimum.Y}
 if($placement.showCmd -eq 3){$maxX=$placement.maximum.X;$maxY=$placement.maximum.Y}
 $second=WindowMetadata $handle
 $screen=[Windows.Forms.Screen]::FromHandle($handle);$offsetX=0;$offsetY=0
 if(([AlfredPetNative]::GetWindowLong($handle,-20) -band 0x80) -eq 0){$offsetX=$screen.WorkingArea.X-$screen.Bounds.X;$offsetY=$screen.WorkingArea.Y-$screen.Bounds.Y}
 $normalScreen=@{x=($placement.normal.Left+$offsetX);y=($placement.normal.Top+$offsetY);width=($placement.normal.Right-$placement.normal.Left);height=($placement.normal.Bottom-$placement.normal.Top)}
 if($second.process_id -ne $meta.process_id -or (ProcessCreated ([uint32]$second.process_id)) -ne $started -or (WindowIdentity $handle $false) -ne $identity){throw 'WINDOW_IDENTITY_CHANGED'}
 $latent=($placement.showCmd -eq 2 -and ($placement.flags -band 2) -ne 0)
 $snapshot=@{hwnd=$meta.hwnd;process_id=$meta.process_id;process_started=$started;window_identity=$identity;monitor=[Windows.Forms.Screen]::FromHandle($handle).DeviceName;x=$second.x;y=$second.y;width=$second.width;height=$second.height;normal_screen=$normalScreen;placement=@{show_cmd=$placement.showCmd;min_x=$minX;min_y=$minY;max_x=$maxX;max_y=$maxY;left=$placement.normal.Left;top=$placement.normal.Top;right=$placement.normal.Right;bottom=$placement.normal.Bottom}}
 if($latent){$snapshot.placement.restore_maximized=$true}
 $snapshot
}
function SameRecovery($actual,$expected) {
 if([bool]$actual.placement.restore_maximized -ne [bool]$expected.placement.restore_maximized){return $false}
 foreach($field in @('hwnd','process_id','process_started','window_identity','monitor','x','y','width','height')){if([string]$actual.$field -cne [string]$expected.$field){return $false}}
 foreach($field in @('x','y','width','height')){if([string]$actual.normal_screen.$field -cne [string]$expected.normal_screen.$field){return $false}}
 foreach($field in @('show_cmd','min_x','min_y','max_x','max_y','left','top','right','bottom')){if([string]$actual.placement.$field -cne [string]$expected.placement.$field){return $false}}
 $true
}
function MoveWindow($placement) {
 $before=WindowMetadata ([IntPtr]([long]$placement.hwnd))
 if($placement.process_id -and [long]$placement.process_id -ne [long]$before.process_id){throw 'WINDOW_TARGET_CHANGED'}
 if($placement.window_identity -and (WindowIdentity ([IntPtr]([long]$placement.hwnd)) $false) -ne [string]$placement.window_identity){throw 'WINDOW_IDENTITY_CHANGED'}
 foreach($key in @('x','y','width','height')){
  $n=$placement.$key
  if($null -eq $n -or [double]$n -ne [int]$n -or [Math]::Abs([double]$n) -gt 100000){throw 'WINDOW_BOUNDS'}
 }
 if($placement.width -lt 1 -or $placement.height -lt 1){throw 'WINDOW_BOUNDS'}
 if($placement.process_started){$null=LayoutTarget $placement}
 # Do not activate or change z-order while positioning.
 if(-not [AlfredPetNative]::SetWindowPos([IntPtr]([long]$placement.hwnd),[IntPtr]::Zero,[int]$placement.x,[int]$placement.y,[int]$placement.width,[int]$placement.height,20)){throw 'WINDOW_MOVE_FAILED'}
 @{before=$before;after=(WindowMetadata ([IntPtr]([long]$placement.hwnd)))}
}
function TargetScreen($metadata) {
 if($p.monitor){
  $screen=[Windows.Forms.Screen]::AllScreens | Where-Object {$_.DeviceName -eq $p.monitor}
  if(-not $screen){throw 'MONITOR_NOT_FOUND'}
  return $screen
 }
 [Windows.Forms.Screen]::FromHandle([IntPtr]([long]$metadata.hwnd))
}
function SelectorPart($spec,[string]$defaultScope='descendants'){
 $scope=if($spec.scope){[string]$spec.scope}else{$defaultScope}
 [AlfredPet.UIA.Selector]::new([string]$spec.name,[string]$spec.automation_id,[int]$spec.control_type,$scope)
}
function GetTarget($spec=$p) {
 Add-Type -Path (Join-Path $PSScriptRoot 'uia.cs')
 if(!$spec.hwnd){throw 'UIA_TARGET_HWND_REQUIRED'}
 $selector=$spec.selector
 if($selector -is [string]){throw 'UIA_SELECTOR_INVALID'}
 foreach($field in @('name','automation_id')){if($selector.$field -and $spec.$field -and $selector.$field -ne $spec.$field){throw 'UIA_SELECTOR_CONFLICT'}}
 $leaf=@{name=$(if($selector.name){$selector.name}else{$spec.name});automation_id=$(if($selector.automation_id){$selector.automation_id}else{$spec.automation_id});control_type=$selector.control_type;scope=$selector.scope}
 $path=[Collections.Generic.List[AlfredPet.UIA.Selector]]::new()
 foreach($part in @($selector.path)){if($null -ne $part){$path.Add((SelectorPart $part))}}
 [AlfredPet.UIA.Target]::FindSelector([long]$spec.hwnd,[int]$spec.process_id,$path.ToArray(),(SelectorPart $leaf))
}
function CaptureBitmapResult($bitmap,$rect,$source='Screen') {
 if($p.return_png){
  $stream=[IO.MemoryStream]::new()
  try {$bitmap.Save($stream,[Drawing.Imaging.ImageFormat]::Png);if($stream.Length -gt 33554432){throw 'CAPTURE_BYTE_LIMIT'};@{png_base64=[Convert]::ToBase64String($stream.ToArray());rect=$rect;coordinate_space='physical';source=$source}}finally{$stream.Dispose()}
 }else{$bitmap.Save([string]$p.output,[Drawing.Imaging.ImageFormat]::Png);@{path=$p.output;rect=$rect;coordinate_space='physical';source=$source}}
}
function Capture($rect) {
 if ($rect.width -le 0 -or $rect.height -le 0 -or $rect.width*$rect.height -gt $(if($p.return_png){33554432}else{50000000}) -or ($p.return_png -and ($rect.width -gt 16384 -or $rect.height -gt 16384))) {throw 'CAPTURE_BOUNDS'}
 $bitmap=[Drawing.Bitmap]::new([int]$rect.width,[int]$rect.height)
 $graphics=[Drawing.Graphics]::FromImage($bitmap)
 try { $graphics.CopyFromScreen([int]$rect.x,[int]$rect.y,0,0,$bitmap.Size); CaptureBitmapResult $bitmap $rect }
 finally { $graphics.Dispose(); $bitmap.Dispose() }
}

. (Join-Path $PSScriptRoot 'gui-input.ps1')
try {
 $result = switch ($op) {
  'monitors' {
   @([Windows.Forms.Screen]::AllScreens | ForEach-Object {
    $pt=[AlfredPetNative+POINT]::new();$pt.X=$_.Bounds.X+10;$pt.Y=$_.Bounds.Y+10
    $mh=[AlfredPetNative]::MonitorFromPoint($pt,2);[uint32]$dx=96;[uint32]$dy=96
    [void][AlfredPetNative]::GetDpiForMonitor($mh,0,[ref]$dx,[ref]$dy)
    @{id=$_.DeviceName;work=@{x=$_.WorkingArea.X;y=$_.WorkingArea.Y;width=$_.WorkingArea.Width;height=$_.WorkingArea.Height};scale=$dx/96.0;primary=$_.Primary}
   })
  }
  'windows.list_processes' { @(Get-Process | Select-Object Id,ProcessName,CPU,WorkingSet64) }
  'windows.open_app' { Start-Process -FilePath ([string]$p.app) -ArgumentList @($p.args) -WindowStyle Normal; @{opened=$true} }
  'windows.close_app' { (Get-Process -Id ([int]$p.process_id)).CloseMainWindow() }
  'windows.stop_process' { Stop-Process -Id ([int]$p.process_id); @{stopped=$true} }
  'windows.list_services' { @(Get-Service | Select-Object Name,DisplayName,@{n='Status';e={$_.Status.ToString()}}) }
  'windows.service_control' { switch ($p.action) { 'start' {Start-Service -Name $p.name}; 'stop' {Stop-Service -Name $p.name}; 'restart' {Restart-Service -Name $p.name}; default {throw 'SERVICE_ACTION'} }; @{done=$true} }
  'windows.registry_get' { Get-ItemPropertyValue -LiteralPath $p.path -Name $p.name }
  'windows.registry_set' { Set-ItemProperty -LiteralPath $p.path -Name $p.name -Value $p.value; @{done=$true} }
  'windows.list_scheduled_tasks' { @(Get-ScheduledTask | Select-Object TaskName,TaskPath,@{n='State';e={$_.State.ToString()}}) }
  'windows.run_scheduled_task' { Start-ScheduledTask -TaskName $p.name; @{started=$true} }
  'windows.power_status' { $s=[Windows.Forms.SystemInformation]::PowerStatus; @{line=$s.PowerLineStatus.ToString();battery=$s.BatteryLifePercent} }
  'windows.devices' { @(Get-PnpDevice -PresentOnly | Select-Object Status,Class,FriendlyName,InstanceId) }
  'windows.open_settings_uri' { if ($p.uri -notmatch '^ms-settings:[a-z-]*$') {throw 'SETTINGS_URI'}; Start-Process $p.uri; @{opened=$true} }
  'window.active' { WindowMetadata ([AlfredPetNative]::GetForegroundWindow()) }
  'watch.process' {
   $target=Get-Process -Id ([int]$p.process_id) -ErrorAction SilentlyContinue
   if($target){try{@{present=$true;process_id=$target.Id;started=$target.StartTime.ToUniversalTime().Ticks}}finally{$target.Dispose()}}
   else {@{present=$false}}
  }
  'watch.window' {
   $handle=[IntPtr]([long]$p.hwnd)
   if([AlfredPetNative]::IsWindow($handle)){
    [uint32]$owner=0;[void][AlfredPetNative]::GetWindowThreadProcessId($handle,[ref]$owner)
    @{present=$true;process_id=$owner;visible=[AlfredPetNative]::IsWindowVisible($handle)}
   }else{@{present=$false}}
  }
  'window.list' { @([AlfredPetNative]::VisibleWindows() | ForEach-Object { WindowMetadata ([IntPtr]$_) } | Where-Object {$_.title}) }
  'window.get_layout' {
   $targets=@($p.body);if($targets.Count -lt 1 -or $targets.Count -gt 32){throw 'LAYOUT_COUNT'}
   $seen=@{};$windows=@()
   foreach($target in $targets){
    if(-not $target.hwnd -or -not $target.process_id -or $seen.ContainsKey([string]$target.hwnd)){throw 'LAYOUT_TARGET'}
    $seen[[string]$target.hwnd]=$true;$meta=LayoutTarget $target
    $started=ProcessCreated ([uint32]$meta.process_id)
    $verified=LayoutTarget @{hwnd=$meta.hwnd;process_id=$meta.process_id;process_started=$started}
    $identity=WindowIdentity ([IntPtr]([long]$verified.hwnd)) $true
    $windows+=,@{hwnd=$verified.hwnd;process_id=$verified.process_id;process_started=$started;window_identity=$identity;x=$verified.x;y=$verified.y;width=$verified.width;height=$verified.height}
   }
   @{windows=@($windows);coordinate_space='physical'}
  }
  'window.recovery_capture' {
   $targets=@($p.body);if($targets.Count -lt 1 -or $targets.Count -gt 32){throw 'LAYOUT_COUNT'}
   $snapshots=@();$seen=@{}
   foreach($target in $targets){if($seen.ContainsKey([string]$target.hwnd)){throw 'LAYOUT_TARGET'};$seen[[string]$target.hwnd]=$true;$snapshots+=,(RecoverySnapshot $target ([bool]$p.initialize))}
   @{windows=@($snapshots)}
  }
  'window.recovery_change' {
   $before=RecoverySnapshot $p.expected
   if(-not (SameRecovery $before $p.expected)){throw 'WINDOW_CHANGED_REVIEW_REQUIRED'}
   $windowChangeError=$null
   try {
    if($p.action -eq 'restore_snapshot'){
     $saved=$p.restore.placement;$wp=[AlfredPetNative+WINDOWPLACEMENT]::new();$wp.length=[Runtime.InteropServices.Marshal]::SizeOf($wp);$wp.flags=if([int]$saved.show_cmd -eq 2){1}else{0} # WPF_SETMINPOSITION applies to an active minimized placement.
     $wp.showCmd=switch([int]$saved.show_cmd){1{4};2{if($saved.restore_maximized){$wp.flags=$wp.flags -bor 2;2}else{7}};3{3};default{throw 'WINDOW_SAVED_STATE'}}
     $minimum=[AlfredPetNative+POINTPLACEMENT]::new();$minimum.X=[int]$saved.min_x;$minimum.Y=[int]$saved.min_y;$wp.minimum=$minimum
     $maximum=[AlfredPetNative+POINTPLACEMENT]::new();$maximum.X=[int]$saved.max_x;$maximum.Y=[int]$saved.max_y;$wp.maximum=$maximum
     $normal=[AlfredPetNative+RECT]::new();$normal.Left=[int]$saved.left;$normal.Top=[int]$saved.top;$normal.Right=[int]$saved.right;$normal.Bottom=[int]$saved.bottom;$wp.normal=$normal
     if(-not [AlfredPetNative]::SetWindowPlacement([IntPtr]([long]$before.hwnd),[ref]$wp)){throw 'WINDOW_RESTORE_FAILED'}
    }else{
     # This branch is dispatched by the guarded Rust journal, one target at a time.
     $params=@{};foreach($property in $p.input.psobject.Properties){$params[$property.Name]=$property.Value};$params.process_id=$before.process_id;$params.window_identity=$before.window_identity
     if($p.restore_to_normal -and $before.placement.show_cmd -ne 1){[void][AlfredPetNative]::ShowWindow([IntPtr]([long]$before.hwnd),4)}
     switch($p.action){
      'move' {$null=MoveWindow $params}
      'state' {$code=switch($params.state){'minimize'{6};'maximize'{3};'restore'{9};default{throw 'WINDOW_STATE'}};[void][AlfredPetNative]::ShowWindow([IntPtr]([long]$before.hwnd),$code)}
      default {throw 'WINDOW_RECOVERY_ACTION'}
     }
    }
   }catch{$windowChangeError=$_.Exception.Message}
   $after=RecoverySnapshot $before
   @{before=$before;after=$after;operation_error=$windowChangeError}
  }
  'window.move' { MoveWindow $p }
  'window.state' { $code=switch($p.state){'minimize'{6};'maximize'{3};'restore'{9};default{throw 'WINDOW_STATE'}}; [void][AlfredPetNative]::ShowWindow([IntPtr]([long]$p.hwnd),$code); @{done=$true} }
  'window.focus' { [AlfredPetNative]::SetForegroundWindow([IntPtr]([long]$p.hwnd)) }
  'window.snap' {
   $before=WindowMetadata ([IntPtr]([long]$p.hwnd));$area=(TargetScreen $before).WorkingArea
   $left=[int][Math]::Floor($area.Width/2);$top=[int][Math]::Floor($area.Height/2)
   $placement=@{hwnd=$p.hwnd;process_id=$before.process_id;x=$area.X;y=$area.Y;width=$area.Width;height=$area.Height}
   switch($p.kind){
    'left' {$placement.width=$left}
    'right' {$placement.x+=$left;$placement.width-=$left}
    'top' {$placement.height=$top}
    'bottom' {$placement.y+=$top;$placement.height-=$top}
    'top_left' {$placement.width=$left;$placement.height=$top}
    'top_right' {$placement.x+=$left;$placement.width-=$left;$placement.height=$top}
    'bottom_left' {$placement.y+=$top;$placement.width=$left;$placement.height-=$top}
    'bottom_right' {$placement.x+=$left;$placement.y+=$top;$placement.width-=$left;$placement.height-=$top}
    'full' {}
    default {throw 'SNAP_PRESET_UNKNOWN'}
   }
   [void][AlfredPetNative]::ShowWindow([IntPtr]([long]$p.hwnd),9)
   MoveWindow $placement
  }
  'window.move_monitor' {
   $before=WindowMetadata ([IntPtr]([long]$p.hwnd));$source=[Windows.Forms.Screen]::FromHandle([IntPtr]([long]$p.hwnd));$dest=TargetScreen $before
   $area=$dest.WorkingArea;$old=$source.WorkingArea
   $width=[Math]::Min($before.width,$area.Width);$height=[Math]::Min($before.height,$area.Height)
   $x=$area.X+[Math]::Round(($before.x-$old.X)/[double][Math]::Max(1,$old.Width)*$area.Width)
   $y=$area.Y+[Math]::Round(($before.y-$old.Y)/[double][Math]::Max(1,$old.Height)*$area.Height)
   $x=[Math]::Max($area.X,[Math]::Min($x,$area.Right-$width));$y=[Math]::Max($area.Y,[Math]::Min($y,$area.Bottom-$height))
   [void][AlfredPetNative]::ShowWindow([IntPtr]([long]$p.hwnd),9)
   MoveWindow @{hwnd=$p.hwnd;process_id=$before.process_id;x=[int]$x;y=[int]$y;width=[int]$width;height=[int]$height}
  }
  'window.set_layout' {
   $placements=@($p.body)
   if($placements.Count -lt 1 -or $placements.Count -gt 32){throw 'LAYOUT_COUNT'}
   $seen=@{};$before=@()
   # Validate every target before moving the first. PID guards reject reused HWNDs.
   foreach($item in $placements){
    if(-not $item.hwnd -or -not $item.process_id -or $seen.ContainsKey([string]$item.hwnd)){throw 'LAYOUT_TARGET'}
    $seen[[string]$item.hwnd]=$true;$meta=WindowMetadata ([IntPtr]([long]$item.hwnd))
    if([long]$meta.process_id -ne [long]$item.process_id){throw 'WINDOW_TARGET_CHANGED'}
    if($item.process_started){$null=LayoutTarget $item;$meta.process_started=[string]$item.process_started}
    foreach($key in @('x','y','width','height')){if($null -eq $item.$key -or [double]$item.$key -ne [int]$item.$key -or [Math]::Abs([double]$item.$key) -gt 100000){throw 'WINDOW_BOUNDS'}}
    if($item.width -lt 1 -or $item.height -lt 1){throw 'WINDOW_BOUNDS'}
    $before+=,$meta
   }
   $changed=@()
   try {foreach($item in $placements){$changed+=,(MoveWindow $item)};@{windows=$changed;coordinate_space='physical'}}
   catch {
    $originalError=$_.Exception.Message;$rollback=$true
    for($i=$changed.Count-1;$i -ge 0;$i--){try{$null=MoveWindow $before[$i]}catch{$rollback=$false}}
    throw ('LAYOUT_FAILED: '+$originalError+'; rollback='+$rollback)
   }
  }
  'vision.capture_monitor' { $screen=[Windows.Forms.Screen]::AllScreens | Where-Object {$_.DeviceName -eq $p.monitor}; if (!$screen) {$screen=[Windows.Forms.Screen]::PrimaryScreen}; Capture @{x=$screen.Bounds.X;y=$screen.Bounds.Y;width=$screen.Bounds.Width;height=$screen.Bounds.Height} }
  'vision.capture_window' { $h=if($p.hwnd){[IntPtr]([long]$p.hwnd)}else{[AlfredPetNative]::GetForegroundWindow()}; $rect=WindowMetadata $h;if($rect.width -le 0 -or $rect.height -le 0 -or $rect.width*$rect.height -gt $(if($p.return_png){33554432}else{50000000}) -or ($p.return_png -and ($rect.width -gt 16384 -or $rect.height -gt 16384))){throw 'CAPTURE_BOUNDS'}; $bitmap=[Drawing.Bitmap]::new([int]$rect.width,[int]$rect.height); $graphics=$null;try{$graphics=[Drawing.Graphics]::FromImage($bitmap);$dc=$graphics.GetHdc();try{if(![AlfredPetNative]::PrintWindow($h,$dc,2)){throw 'WINDOW_CAPTURE_UNAVAILABLE'}}finally{$graphics.ReleaseHdc($dc)};CaptureBitmapResult $bitmap $rect 'HWND'}finally{if($graphics){$graphics.Dispose()};$bitmap.Dispose()} }
  'vision.capture_region' { Capture $p }
  'documents.render_page' {
   Add-Type -AssemblyName System.Runtime.WindowsRuntime
   function AwaitPdf($operation,$type) { $m=[System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {$_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.IsGenericMethod} | Select-Object -First 1; $task=$m.MakeGenericMethod($type).Invoke($null,@($operation));try{$task.Wait();$task.Result}catch{throw ('PDF_NATIVE_ERROR: '+$_.Exception.GetBaseException().Message)} }
   $null=[Windows.Storage.StorageFile,Windows.Storage,ContentType=WindowsRuntime]
   $null=[Windows.Data.Pdf.PdfDocument,Windows.Data.Pdf,ContentType=WindowsRuntime]
   $null=[Windows.Data.Pdf.PdfPageRenderOptions,Windows.Data.Pdf,ContentType=WindowsRuntime]
   $null=[Windows.Storage.Streams.InMemoryRandomAccessStream,Windows.Storage.Streams,ContentType=WindowsRuntime]
   $null=[Windows.Storage.Streams.DataReader,Windows.Storage.Streams,ContentType=WindowsRuntime]
   $file=AwaitPdf ([Windows.Storage.StorageFile]::GetFileFromPathAsync([string]$p.path)) ([Windows.Storage.StorageFile])
   $pdf=AwaitPdf ([Windows.Data.Pdf.PdfDocument]::LoadFromFileAsync($file)) ([Windows.Data.Pdf.PdfDocument])
   $number=[int]$p.page;$edge=[int]$p.width
   if($number -lt 1 -or $number -gt $pdf.PageCount -or $pdf.PageCount -gt 1000 -or $edge -lt 64 -or $edge -gt 2048){throw 'PDF_RENDER_BOUNDS'}
   $page=$pdf.GetPage([uint32]($number-1));$stream=$null;$reader=$null
   try {
    if($page.Size.Width -le 0 -or $page.Size.Height -le 0){throw 'PDF_RENDER_BOUNDS'}
    $ratio=[Math]::Min($edge/$page.Size.Width,$edge/$page.Size.Height)
    $options=[Windows.Data.Pdf.PdfPageRenderOptions]::new();$options.DestinationWidth=[uint32][Math]::Max(1,[Math]::Round($page.Size.Width*$ratio));$options.DestinationHeight=[uint32][Math]::Max(1,[Math]::Round($page.Size.Height*$ratio))
    $stream=[Windows.Storage.Streams.InMemoryRandomAccessStream]::new()
    $action=$page.RenderToStreamAsync($stream,$options)
    $awaitAction=[System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {$_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and !$_.IsGenericMethod -and $_.GetParameters()[0].ParameterType.FullName -eq 'Windows.Foundation.IAsyncAction'} | Select-Object -First 1
    $awaitAction.Invoke($null,@($action)).Wait()
    if($stream.Size -gt 4MB -or $stream.Size -eq 0){throw 'PDF_RENDER_BYTE_LIMIT'}
    $reader=[Windows.Storage.Streams.DataReader]::new($stream.GetInputStreamAt(0));$loaded=AwaitPdf ($reader.LoadAsync([uint32]$stream.Size)) ([uint32])
    if($loaded -ne $stream.Size){throw 'PDF_RENDER_READ'}
    $bytes=[byte[]]::new([int]$stream.Size);$reader.ReadBytes($bytes)
    @{png_base64=[Convert]::ToBase64String($bytes);page=$number;width=$options.DestinationWidth;height=$options.DestinationHeight}
   } finally {if($reader){$reader.Dispose()};if($stream){$stream.Dispose()};$page.Dispose()}
  }
  'vision.ocr' {
   Add-Type -AssemblyName System.Runtime.WindowsRuntime
   function Await($task,$type) { $m=[System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {$_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.IsGenericMethod} | Select-Object -First 1; $t=$m.MakeGenericMethod($type).Invoke($null,@($task));$t.Wait();$t.Result }
   $null=[Windows.Storage.StorageFile,Windows.Storage,ContentType=WindowsRuntime]
   $null=[Windows.Graphics.Imaging.BitmapDecoder,Windows.Graphics.Imaging,ContentType=WindowsRuntime]
   $null=[Windows.Media.Ocr.OcrEngine,Windows.Foundation,ContentType=WindowsRuntime]
   $file=Await ([Windows.Storage.StorageFile]::GetFileFromPathAsync([string]$p.path)) ([Windows.Storage.StorageFile])
   $stream=Await ($file.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStream])
   try { $decoder=Await ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder]);$bitmap=Await ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap]);$engine=[Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages();if(!$engine){throw 'OCR_LANGUAGE_MISSING'};$ocr=Await ($engine.RecognizeAsync($bitmap)) ([Windows.Media.Ocr.OcrResult]);@{text=$ocr.Text} } finally {$stream.Dispose()}
  }
  'explain.selection' { Add-Type -Path (Join-Path $PSScriptRoot 'uia.cs'); @{text=[AlfredPet.UIA.Target]::FocusedSelection([long]$p.hwnd)} }
  'gui.inspect' {
   $e=GetTarget;try{@($e.Inspect())}finally{$e.Dispose()}
  }
  'gui.find_element' { $e=GetTarget;try{$e.Metadata()}finally{$e.Dispose()} }
  'gui.invoke' { $e=GetTarget;try{$e.Invoke();@{done=$true}}finally{$e.Dispose()} }
  'gui.set_value' { $e=GetTarget;try{$e.SetValue([string]$p.text);@{done=$true}}finally{$e.Dispose()} }
  'gui.get_text' { $e=GetTarget;try{@{text=$e.GetText()}}finally{$e.Dispose()} }
  'gui.select' { $e=GetTarget;try{$e.Select();@{done=$true}}finally{$e.Dispose()} }
  'gui.click' { $e=GetTarget;try{$invoked=$e.TryInvoke()}finally{$e.Dispose()};if($invoked){@{done=$true;method='InvokePattern'}}else{GuiPointer 'click'} }
  'gui.hover' { GuiPointer 'hover' }
  'gui.drag' { GuiPointer 'drag' }
  'gui.screenshot_target' { GuiCaptureTarget }
  'gui.wait_for_element' {
   $timeout=if($p.timeout_seconds){[int]$p.timeout_seconds}else{5}
   if($timeout -lt 1 -or $timeout -gt 30){throw 'UIA_WAIT_TIMEOUT_INVALID'}
   $state=if($p.state){[string]$p.state}else{'present'}
   if($state -notin @('present','enabled','absent')){throw 'UIA_WAIT_STATE_INVALID'}
   $watch=[Diagnostics.Stopwatch]::StartNew()
   do{
    $found=$false;$metadata=$null
    try{$e=GetTarget;try{$metadata=$e.Metadata();$found=$true}finally{$e.Dispose()}}
    catch{if(-not $_.Exception.ToString().Contains('UIA_ELEMENT_NOT_FOUND')){throw}}
    if(($state -eq 'absent' -and !$found) -or ($found -and ($state -eq 'present' -or ($state -eq 'enabled' -and $metadata.enabled)))){break}
    if($watch.Elapsed.TotalSeconds -ge $timeout){throw 'UIA_WAIT_TIMEOUT'}
    Start-Sleep -Milliseconds 100
   }while($true)
   @{matched=$true;state=$state;elapsed_ms=$watch.ElapsedMilliseconds;element=$metadata}
  }
  'gui.send_keys' {
   if (!$p.hwnd) {throw 'INPUT_REQUIRES_TARGET_HWND'}
   $previous=[AlfredPetNative]::GetForegroundWindow();$restore=$p.body.focus_and_restore -eq $true
   if($restore -and $previous -eq [IntPtr]::Zero){throw 'INPUT_PREVIOUS_WINDOW_MISSING'}
   # Optional scoped focus is restored inside the same process, after readback.
   # Validate the target and its readable text before changing global focus.
   $originalText=$null
   if($restore){$target=GetTarget;try{$originalText=$target.GetText()}finally{$target.Dispose()}}
   try {
   if($restore){
    # UIA requests focus on the already validated element. It does not guarantee
    # foreground activation; native foreground and keyboard-focus guards remain.
    $focusTarget=GetTarget;try{$focusTarget.SetFocus()}finally{$focusTarget.Dispose()}
    if([AlfredPetNative]::GetForegroundWindow().ToInt64() -ne [long]$p.hwnd -and -not [AlfredPetNative]::SetForegroundWindow([IntPtr]([long]$p.hwnd))){throw 'INPUT_FOCUS_DENIED'}
    if(-not (WaitForeground ([IntPtr]([long]$p.hwnd)))){throw 'INPUT_TARGET_NOT_FOREGROUND'}
   }
   if ([AlfredPetNative]::GetForegroundWindow().ToInt64() -ne [long]$p.hwnd) {throw 'INPUT_TARGET_NOT_FOREGROUND'}
   $e=GetTarget;try{
    $e.SetFocus();$focused=$false
    for($attempt=0;$attempt -lt 20;$attempt++){if($e.HasKeyboardFocus){$focused=$true;break};Start-Sleep -Milliseconds 50}
    if(!$focused){throw 'INPUT_TARGET_NOT_FOCUSED'}
   }finally{$e.Dispose()}
   $inputs=[Collections.Generic.List[AlfredPetNative+INPUT]]::new()
   foreach ($ch in ([string]$p.text).ToCharArray()) { foreach ($flag in @(4,6)) {
    # Nested structs are values in PowerShell: build each value before assigning
    # it, otherwise scan/flags are changed on a temporary copy and lost.
    $key=[AlfredPetNative+KEYBDINPUT]::new();$key.scan=[uint16]$ch;$key.flags=$flag
    if($p._input_tag){$key.extra=[UIntPtr]([uint64]$p._input_tag)}
    $union=[AlfredPetNative+INPUTUNION]::new();$union.ki=$key
    $i=[AlfredPetNative+INPUT]::new();$i.type=1;$i.data=$union;$inputs.Add($i)
   } }
   if ([AlfredPetNative]::GetForegroundWindow().ToInt64() -ne [long]$p.hwnd) {throw 'INPUT_TARGET_CHANGED'}
   $arr=$inputs.ToArray();$n=[AlfredPetNative]::SendInput($arr.Length,$arr,[Runtime.InteropServices.Marshal]::SizeOf([type][AlfredPetNative+INPUT])); if($n -ne $arr.Length){throw 'INPUT_BLOCKED'}
   if($restore -and ([string]$p.text).Length -gt 0){
    $readback=$false
    for($attempt=0;$attempt -lt 25;$attempt++){
     if([AlfredPetNative]::GetForegroundWindow().ToInt64() -ne [long]$p.hwnd){throw 'INPUT_TARGET_CHANGED'}
     $target=GetTarget;try{$after=$target.GetText()}finally{$target.Dispose()}
     if($after -ne $originalText -and $after.Contains([string]$p.text)){$readback=$true;break}
     Start-Sleep -Milliseconds 20
    }
    if(!$readback){throw 'INPUT_READBACK_FAILED'}
   }
   } finally {
    if($restore -and $previous -ne [IntPtr]::Zero -and [AlfredPetNative]::GetForegroundWindow().ToInt64() -eq [long]$p.hwnd){
     if([AlfredPetNative]::GetForegroundWindow() -ne $previous -and -not [AlfredPetNative]::SetForegroundWindow($previous)){throw 'INPUT_RESTORE_DENIED'}
     if(-not (WaitForeground $previous)){throw 'INPUT_RESTORE_DENIED'}
    }
   }
   @{sent=$n;focus_restored=($restore -and [AlfredPetNative]::GetForegroundWindow() -eq $previous)}
  }
  'printers.list' { ,@(Get-Printer | Select-Object -First 256 Name,DriverName,PortName,PrinterStatus) }
  'printers.default' {
   $default=@(Get-CimInstance Win32_Printer | Where-Object {$_.Default});if($default.Count -ne 1){throw 'BLOCKED_EXTERNAL: PRINTER_DEFAULT_MISSING'}
   $target=@(Get-Printer | Where-Object {$_.Name -ceq $default[0].Name});if($target.Count -ne 1){throw 'PRINTER_NOT_FOUND'}
   $target[0] | Select-Object Name,DriverName,PortName,PrinterStatus
  }
  'printers.status' { $target=@(Get-Printer | Where-Object {$_.Name -ceq [string]$p.name});if($target.Count -ne 1){throw 'PRINTER_NOT_FOUND'};$target[0] | Select-Object Name,DriverName,PortName,PrinterStatus }
  'printers.queue' { ,@(Get-PrintJob -PrinterName ([string]$p.name) | Select-Object -First 256 ID,DocumentName,JobStatus,Size) }
  'printers.jobs' { ,@(Get-PrintJob -PrinterName ([string]$p.name) | Select-Object -First 256 ID,DocumentName,JobStatus,Size) }
  'printers.cancel_job' {
   $job=@(Get-PrintJob -PrinterName ([string]$p.name) | Where-Object {$_.ID -eq [int]$p.job_id});if($job.Count -ne 1){throw 'PRINT_JOB_NOT_FOUND'}
   if(!$p._job -or $job[0].DocumentName -cne $p._job.DocumentName -or [long]$job[0].Size -ne [long]$p._job.Size -or [string]$job[0].JobStatus -cne [string]$p._job.JobStatus){throw 'PRINT_JOB_CHANGED_REVIEW_REQUIRED'}
   Remove-PrintJob -PrinterName ([string]$p.name) -ID ([int]$p.job_id);@{cancel_requested=$true;job_id=[int]$p.job_id;printer=[string]$p.name;completion='NOT_CONFIRMED'}
  }
  'printers.print' {
   if(!$p._printer_identity -or [string]$p.name -match '["\r\n]'){throw 'PRINT_PREPARED_TARGET_REQUIRED'}
   $target=@(Get-Printer | Where-Object {$_.Name -ceq [string]$p.name});if($target.Count -ne 1){throw 'PRINTER_NOT_FOUND'}
   if($target[0].DriverName -cne $p._printer_identity.DriverName -or $target[0].PortName -cne $p._printer_identity.PortName){throw 'PRINTER_CHANGED_REVIEW_REQUIRED'}
   $info=[Diagnostics.ProcessStartInfo]::new([string]$p.path);if('PrintTo' -notin $info.Verbs){throw 'BLOCKED_EXTERNAL: PRINTTO_HANDLER_MISSING'}
   Start-Process -FilePath ([string]$p.path) -Verb PrintTo -ArgumentList ('"'+[string]$p.name+'"') -WindowStyle Hidden -Wait
   @{submitted=$true;printer=[string]$p.name;completion='NOT_CONFIRMED'}
  }
  'audio.play' { if (Test-Path -LiteralPath $p.path) { $sound=[Media.SoundPlayer]::new([string]$p.path);try{$sound.PlaySync()}finally{$sound.Dispose()};@{played=$true} }else{throw 'AUDIO_FILE_MISSING'} }
  'secret.set' { $target='alfredpet.public.'+[string]$p.alias; $bytes=[Text.Encoding]::Unicode.GetBytes([string]$p.value);$ptr=[IntPtr]::Zero;try{$ptr=[Runtime.InteropServices.Marshal]::AllocCoTaskMem($bytes.Length);if($bytes.Length -gt 0){[Runtime.InteropServices.Marshal]::Copy($bytes,0,$ptr,$bytes.Length)};$cred=[AlfredPetNative+CREDENTIAL]::new();$cred.Type=1;$cred.TargetName=$target;$cred.CredentialBlobSize=$bytes.Length;$cred.CredentialBlob=$ptr;$cred.Persist=2;$cred.UserName='AlfredPet';if(![AlfredPetNative]::CredWrite([ref]$cred,0)){throw 'CREDENTIAL_WRITE_FAILED'};@{saved=$true}}finally{[Array]::Clear($bytes,0,$bytes.Length);try{if($ptr -ne [IntPtr]::Zero -and $bytes.Length -gt 0){[Runtime.InteropServices.Marshal]::Copy($bytes,0,$ptr,$bytes.Length)}}finally{if($ptr -ne [IntPtr]::Zero){[Runtime.InteropServices.Marshal]::FreeCoTaskMem($ptr)}}} }
  'secret.get' { $ptr=[IntPtr]::Zero;if(![AlfredPetNative]::CredRead(('alfredpet.public.'+[string]$p.alias),1,0,[ref]$ptr)){throw 'CREDENTIAL_NOT_FOUND'};try{$cred=[Runtime.InteropServices.Marshal]::PtrToStructure($ptr,[type][AlfredPetNative+CREDENTIAL]);@{value=[Runtime.InteropServices.Marshal]::PtrToStringUni($cred.CredentialBlob,$cred.CredentialBlobSize/2)}}finally{[AlfredPetNative]::CredFree($ptr)} }
  default {throw ('NATIVE_OPERATION_UNKNOWN: '+$op)}
 }
 @{ok=$true;data=$result} | ConvertTo-Json -Depth 20 -Compress | Write-Output
} catch { @{ok=$false;error=$_.Exception.Message} | ConvertTo-Json -Compress | Write-Output; exit 1 }

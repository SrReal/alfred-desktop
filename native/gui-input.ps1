function WaitForeground([IntPtr]$target) {
 for($attempt=0;$attempt -lt 20;$attempt++){
  if([AlfredPetNative]::GetForegroundWindow() -eq $target){return $true}
  Start-Sleep -Milliseconds 25
 }
 return ([AlfredPetNative]::GetForegroundWindow() -eq $target)
}
function MousePosition {
 $point=[AlfredPetNative+POINT]::new()
 if(-not [AlfredPetNative]::GetCursorPos([ref]$point)){throw 'INPUT_CURSOR_UNAVAILABLE'}
 @{x=$point.X;y=$point.Y}
}
function SamePoint($first,$second){[Math]::Abs($first.x-$second.x) -le 2 -and [Math]::Abs($first.y-$second.y) -le 2}
function ElementPoint($target){
 $bounds=$target.Bounds()
 @{x=[int][Math]::Floor($bounds[0]+$bounds[2]/2);y=[int][Math]::Floor($bounds[1]+$bounds[3]/2)}
}
function CheckMousePoint($target,$point,[long]$hwnd){
 $target.ValidateAction()
 $nativePoint=[AlfredPetNative+POINT]::new();$nativePoint.X=$point.x;$nativePoint.Y=$point.y
 $hit=[AlfredPetNative]::WindowFromPoint($nativePoint)
 if($hit -eq [IntPtr]::Zero -or [AlfredPetNative]::GetAncestor($hit,2).ToInt64() -ne $hwnd){throw 'INPUT_TARGET_OCCLUDED'}
 $target.ValidatePoint($point.x,$point.y)
}
function MousePacket([uint32]$flags,$point){
 $mouse=[AlfredPetNative+MOUSEINPUT]::new()
 $mouse.dwFlags=$flags
 if($p._input_tag){$mouse.extra=[UIntPtr]([uint64]$p._input_tag)}
 if($flags -band 1){
  $screen=[Windows.Forms.SystemInformation]::VirtualScreen
  if($screen.Width -lt 2 -or $screen.Height -lt 2 -or $point.x -lt $screen.Left -or $point.y -lt $screen.Top -or $point.x -ge $screen.Right -or $point.y -ge $screen.Bottom){throw 'INPUT_POINT_OUTSIDE_DESKTOP'}
  $mouse.dx=[int][Math]::Round(($point.x-$screen.Left)*65535.0/($screen.Width-1))
  $mouse.dy=[int][Math]::Round(($point.y-$screen.Top)*65535.0/($screen.Height-1))
 }
 $union=[AlfredPetNative+INPUTUNION]::new();$union.mi=$mouse
 $packet=[AlfredPetNative+INPUT]::new();$packet.type=0;$packet.data=$union
 $packets=[AlfredPetNative+INPUT[]]@($packet)
 if([AlfredPetNative]::SendInput(1,$packets,[Runtime.InteropServices.Marshal]::SizeOf([type][AlfredPetNative+INPUT])) -ne 1){throw 'INPUT_BLOCKED'}
}
function MouseMove($point){MousePacket 49153 $point}
function GuiPointer([string]$action){
 if($p._fallback_enabled -ne $true){throw 'SENDINPUT_DISABLED: opt-in required'}
 foreach($key in @(1,2,4,16,17,18)){if([AlfredPetNative]::GetAsyncKeyState($key) -lt 0){throw 'INPUT_USER_BUSY'}}
 $source=$null;$destination=$null;$down=$false;$moved=$false;$count=0
 $originalCursor=MousePosition;$last=$originalCursor;$previous=[AlfredPetNative]::GetForegroundWindow()
 $restoreFocus=$p.body.focus_and_restore -eq $true
 $destinationHwnd=[long]$p.hwnd
 try{
  $source=GetTarget
  $source.ValidateAction();$start=ElementPoint $source
  if($action -eq 'drag'){
   $spec=$p.body.destination
   if(!$spec){throw 'GUI_DRAG_DESTINATION_REQUIRED'}
   if(!$spec.hwnd){$spec | Add-Member -NotePropertyName hwnd -NotePropertyValue $p.hwnd -Force}
   $destinationHwnd=[long]$spec.hwnd
   if($destinationHwnd -eq [long]$p.hwnd -and !$spec.process_id){$spec | Add-Member -NotePropertyName process_id -NotePropertyValue $p.process_id -Force}
   $destination=GetTarget $spec;$destination.ValidateAction();$end=ElementPoint $destination
   CheckMousePoint $destination $end $destinationHwnd
  }
  if($restoreFocus){
   if($previous -eq [IntPtr]::Zero){throw 'INPUT_PREVIOUS_WINDOW_MISSING'}
   if([AlfredPetNative]::GetForegroundWindow().ToInt64() -ne [long]$p.hwnd -and -not [AlfredPetNative]::SetForegroundWindow([IntPtr]([long]$p.hwnd))){throw 'INPUT_FOCUS_DENIED'}
   if(-not (WaitForeground ([IntPtr]([long]$p.hwnd)))){throw 'INPUT_TARGET_NOT_FOREGROUND'}
  }
  if([AlfredPetNative]::GetForegroundWindow().ToInt64() -ne [long]$p.hwnd){throw 'INPUT_TARGET_NOT_FOREGROUND'}
  CheckMousePoint $source $start ([long]$p.hwnd)
  if(-not (SamePoint (MousePosition) $originalCursor)){throw 'INPUT_POINTER_CHANGED'}
  MouseMove $start;$moved=$true;$last=$start;$count++
  if(-not (SamePoint (MousePosition) $last)){throw 'INPUT_POINTER_READBACK_FAILED'}
  if($action -eq 'hover'){Start-Sleep -Milliseconds 200}
  elseif($action -eq 'click'){
   CheckMousePoint $source $start ([long]$p.hwnd)
   if([AlfredPetNative]::GetForegroundWindow().ToInt64() -ne [long]$p.hwnd){throw 'INPUT_TARGET_CHANGED'}
   MousePacket 2 $null;$down=$true;$count++;Start-Sleep -Milliseconds 50
   MousePacket 4 $null;$down=$false;$count++
  }elseif($action -eq 'drag'){
   $duration=if($p.body.duration_ms){[int]$p.body.duration_ms}else{500}
   if($duration -lt 100 -or $duration -gt 3000){throw 'GUI_DRAG_DURATION'}
   MousePacket 2 $null;$down=$true;$count++
   for($step=1;$step -le 24;$step++){
    $source.ValidateAction();$destination.ValidateAction();$source.ValidateIdentity();$destination.ValidateIdentity()
    $foreground=[AlfredPetNative]::GetForegroundWindow().ToInt64()
    if($foreground -ne [long]$p.hwnd -and $foreground -ne $destinationHwnd){throw 'INPUT_TARGET_CHANGED'}
    if(-not (SamePoint (MousePosition) $last)){throw 'INPUT_POINTER_CHANGED'}
    $next=@{x=[int][Math]::Round($start.x+($end.x-$start.x)*$step/24);y=[int][Math]::Round($start.y+($end.y-$start.y)*$step/24)}
    MouseMove $next;$last=$next;$count++;Start-Sleep -Milliseconds ([int][Math]::Ceiling($duration/24))
   }
   if(-not (SamePoint (ElementPoint $destination) $end)){throw 'INPUT_DESTINATION_MOVED'}
   CheckMousePoint $destination $end $destinationHwnd
   MousePacket 4 $null;$down=$false;$count++
  }else{throw 'GUI_POINTER_ACTION'}
  if(-not (SamePoint (MousePosition) $last)){throw 'INPUT_POINTER_CHANGED'}
 }finally{
  try{if($down){MousePacket 4 $null}}finally{
   try{
   $pointerRestored=$false
   if($moved -and (SamePoint (MousePosition) $last)){MouseMove $originalCursor;$pointerRestored=(SamePoint (MousePosition) $originalCursor)}
   $foreground=[AlfredPetNative]::GetForegroundWindow().ToInt64()
   if($restoreFocus -and ($foreground -eq [long]$p.hwnd -or $foreground -eq $destinationHwnd)){
    if([AlfredPetNative]::GetForegroundWindow() -ne $previous -and -not [AlfredPetNative]::SetForegroundWindow($previous)){throw 'INPUT_RESTORE_DENIED'}
    if(-not (WaitForeground $previous)){throw 'INPUT_RESTORE_DENIED'}
   }
   }finally{if($destination){$destination.Dispose()};if($source){$source.Dispose()}}
  }
 }
 @{method='SendInput';sent=$count;pointer_restored=$pointerRestored;focus_restored=($restoreFocus -and [AlfredPetNative]::GetForegroundWindow() -eq $previous)}
}
function GuiCaptureTarget{
 $target=GetTarget
 try{
  $rect=$target.Bounds();$window=WindowMetadata ([IntPtr]([long]$p.hwnd))
  if($window.width -lt 1 -or $window.height -lt 1 -or $window.width*$window.height -gt 50000000){throw 'CAPTURE_BOUNDS'}
  if($p.return_png -and ($window.width -gt 16384 -or $window.height -gt 16384 -or [long]$window.width*$window.height -gt 33554432)){throw 'CAPTURE_PIXEL_LIMIT'}
  $x=[int][Math]::Floor($rect[0]-$window.x);$y=[int][Math]::Floor($rect[1]-$window.y)
  $width=[int][Math]::Ceiling($rect[2]);$height=[int][Math]::Ceiling($rect[3])
  if($x -lt 0 -or $y -lt 0 -or $width -lt 1 -or $height -lt 1 -or $x+$width -gt $window.width -or $y+$height -gt $window.height){throw 'CAPTURE_TARGET_OUTSIDE_WINDOW'}
  $bitmap=[Drawing.Bitmap]::new([int]$window.width,[int]$window.height);$graphics=[Drawing.Graphics]::FromImage($bitmap)
  try{
   $dc=$graphics.GetHdc()
   try{if(-not [AlfredPetNative]::PrintWindow([IntPtr]([long]$p.hwnd),$dc,2)){throw 'CAPTURE_WINDOW_DENIED'}}finally{$graphics.ReleaseHdc($dc)}
   $target.ValidateIdentity()
   $crop=$bitmap.Clone([Drawing.Rectangle]::new($x,$y,$width,$height),[Drawing.Imaging.PixelFormat]::Format32bppArgb)
   try{$result=CaptureBitmapResult $crop @{x=[int]$rect[0];y=[int]$rect[1];width=$width;height=$height} 'PrintWindow';$result.method='PrintWindow'}finally{$crop.Dispose()}
  }finally{$graphics.Dispose();$bitmap.Dispose()}
  $result
 }finally{$target.Dispose()}
}

param([string]$Helper)
$ErrorActionPreference='Stop'
# Replace operating-system I/O only. The production GuiPointer state machine
# runs unchanged; this fixture never calls SendInput or moves the real cursor.
Add-Type -TypeDefinition @'
using System;
public static class AlfredPetNative {
 public struct POINT {public int X,Y;}
 public static IntPtr Foreground=new IntPtr(123);
 public static IntPtr GetForegroundWindow(){return Foreground;}
 public static bool SetForegroundWindow(IntPtr value){Foreground=value;return true;}
 public static short GetAsyncKeyState(int key){return 0;}
}
'@
. $Helper
function Assert($condition,$message){if(!$condition){throw $message}}
function MousePosition{@{x=$script:cursor.x;y=$script:cursor.y}}
function MouseMove($point){
 $script:moves++;$script:cursor=@{x=$point.x;y=$point.y}
 if($script:scenario -eq 'user_pointer' -and $script:moves -eq 4){$script:cursor.x=999}
}
function MousePacket([uint32]$flags,$point){$script:packets.Add($flags)}
function CheckMousePoint($target,$point,[long]$hwnd){$target.ValidateAction()}
function GetTarget($spec=$p){
 $target=[pscustomobject]@{destination=($spec.name -eq 'destination');disposed=$false}
 $target | Add-Member -MemberType ScriptMethod -Name ValidateAction -Value {}
 $target | Add-Member -MemberType ScriptMethod -Name ValidateIdentity -Value {
  if($script:scenario -eq 'identity' -and $script:moves -ge 3){throw 'UIA_TARGET_CHANGED'}
  if($script:scenario -eq 'foreground' -and $script:moves -ge 3){[AlfredPetNative]::Foreground=[IntPtr]999}
 }
 $target | Add-Member -MemberType ScriptMethod -Name Bounds -Value {
  if($this.destination){if($script:scenario -eq 'destination_moved' -and $script:moves -ge 25){return @(200,100,20,20)};return @(100,100,20,20)}
  @(10,10,20,20)
 }
 $target | Add-Member -MemberType ScriptMethod -Name Dispose -Value {$this.disposed=$true}
 $script:targets.Add($target)
 $target
}
$checks=@()
foreach($scenario in @('hover','click','drag','identity','foreground','user_pointer','destination_moved')){
 $script:scenario=$scenario;$script:moves=0;$script:cursor=@{x=5;y=6}
 $script:packets=[Collections.Generic.List[uint32]]::new();$script:targets=[Collections.Generic.List[object]]::new()
 [AlfredPetNative]::Foreground=[IntPtr]123
 $p=[pscustomobject]@{hwnd=123;process_id=1;_fallback_enabled=$true;body=[pscustomobject]@{duration_ms=100;destination=[pscustomobject]@{name='destination'}}}
 $action=if($scenario -in @('hover','click')){$scenario}else{'drag'}
 $errorCode=$null
 try{$result=GuiPointer $action}catch{$errorCode=$_.Exception.ToString()}
 $success=$scenario -in @('hover','click','drag')
 if($success){Assert (!$errorCode) ($scenario+': '+$errorCode);Assert $result.pointer_restored ($scenario+': cursor not restored')}
 else{
  $expected=@{identity='UIA_TARGET_CHANGED';foreground='INPUT_TARGET_CHANGED';user_pointer='INPUT_POINTER_CHANGED';destination_moved='INPUT_DESTINATION_MOVED'}[$scenario]
  Assert ($errorCode -and $errorCode.Contains($expected)) ($scenario+': wrong failure '+$errorCode)
 }
 $downs=@($script:packets | Where-Object {$_ -eq 2}).Count;$ups=@($script:packets | Where-Object {$_ -eq 4}).Count
 Assert ($downs -eq $ups) ($scenario+': unbalanced button state')
 Assert (@($script:targets | Where-Object {!$_.disposed}).Count -eq 0) ($scenario+': target leaked')
 if($scenario -eq 'user_pointer'){Assert ($script:cursor.x -eq 999) 'overwrote user cursor'}
 if($scenario -eq 'foreground'){Assert ([AlfredPetNative]::Foreground.ToInt64() -eq 999) 'overwrote user foreground'}
 $checks+=@{name=$scenario;status='PASS';downs=$downs;ups=$ups;actual_os_input=$false}
}
@{status='PASS';scope='Production pointer state machine with mocked operating-system I/O; no hardware or crash proof';checks=$checks} | ConvertTo-Json -Depth 6 -Compress
